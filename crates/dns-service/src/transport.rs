// Copyright 2015-2019 Benjamin Fry <benjaminfry@me.com>
// SPDX-License-Identifier: MIT
// Adapted from Hickory f09321075b1f97902b7bc4ca4ffda7816fcf2971,
// crates/net/src/h2.rs connect/send. See UPSTREAM.md for intentional changes.
use crate::{MAX_MESSAGE, Upstream};
use bytes::Bytes;
use hickory_proto::{
    op::{Message, ResponseCode},
    rr::RData,
};
use std::{io, sync::Arc};
use tokio::{
    net::TcpStream,
    sync::Mutex,
    task::JoinHandle,
    time::{Duration, Instant, timeout_at},
};
use tokio_rustls::TlsConnector;

pub(crate) struct Connection {
    sender: h2::client::SendRequest<Bytes>,
    driver: JoinHandle<()>,
}
impl Drop for Connection {
    fn drop(&mut self) {
        self.driver.abort();
    }
}
struct State {
    connection: Option<Arc<Connection>>,
    retry_at: Instant,
}
pub(crate) struct Endpoint {
    config: Upstream,
    tls: Arc<rustls::ClientConfig>,
    state: Mutex<State>,
}
impl Endpoint {
    pub fn new(config: Upstream, tls: Arc<rustls::ClientConfig>) -> Self {
        Self {
            config,
            tls,
            state: Mutex::new(State {
                connection: None,
                retry_at: Instant::now(),
            }),
        }
    }
    async fn connection(&self) -> io::Result<Arc<Connection>> {
        let mut state = self.state.lock().await;
        if let Some(c) = &state.connection {
            return Ok(c.clone());
        }
        if Instant::now() < state.retry_at {
            return Err(io::Error::other("upstream cooling down"));
        }
        // Set before awaiting: canceled/failed establishment also gets a cooldown.
        state.retry_at = Instant::now() + Duration::from_secs(1);
        let tcp = TcpStream::connect(self.config.address).await?;
        let name = rustls::pki_types::ServerName::try_from(self.config.server_name.clone())
            .map_err(io::Error::other)?;
        let tls = TlsConnector::from(self.tls.clone())
            .connect(name, tcp)
            .await?;
        if tls.get_ref().1.alpn_protocol() != Some(b"h2") {
            return Err(io::Error::other("upstream did not negotiate h2"));
        }
        let mut builder = h2::client::Builder::new();
        builder
            .enable_push(false)
            .max_header_list_size(16384)
            .initial_window_size(65535)
            .initial_connection_window_size(65535)
            .max_send_buffer_size(65535);
        let (sender, driver) = builder.handshake(tls).await.map_err(io::Error::other)?;
        let connection = Arc::new(Connection {
            sender,
            driver: tokio::spawn(async move {
                let _ = driver.await;
            }),
        });
        state.connection = Some(connection.clone());
        Ok(connection)
    }
    pub async fn exchange(&self, query: &[u8], deadline: Instant) -> io::Result<(Message, u32)> {
        let connection = timeout_at(deadline, self.connection())
            .await
            .map_err(io::Error::other)??;
        let result = timeout_at(deadline, self.send(&connection, query))
            .await
            .map_err(io::Error::other)
            .and_then(|r| r);
        if result.is_err() {
            // A locked state here is another establishment/retirement: that owner
            // has already removed this generation or will observe its send error.
            if let Ok(mut state) = self.state.try_lock() {
                if state
                    .connection
                    .as_ref()
                    .is_some_and(|c| Arc::ptr_eq(c, &connection))
                {
                    state.connection = None;
                    state.retry_at = Instant::now() + Duration::from_secs(1);
                }
            }
        }
        result
    }
    async fn send(&self, connection: &Connection, query: &[u8]) -> io::Result<(Message, u32)> {
        let mut sender = connection
            .sender
            .clone()
            .ready()
            .await
            .map_err(io::Error::other)?;
        let request = http::Request::post(format!(
            "https://{}{}",
            self.config.server_name, self.config.path
        ))
        .header("content-type", "application/dns-message")
        .header("accept", "application/dns-message")
        .header("content-length", query.len())
        .body(())
        .map_err(io::Error::other)?;
        let (response, mut body) = sender
            .send_request(request, false)
            .map_err(io::Error::other)?;
        body.send_data(Bytes::copy_from_slice(query), true)
            .map_err(io::Error::other)?;
        let response = response.await.map_err(io::Error::other)?;
        if !response.status().is_success() {
            return Err(io::Error::other("DoH HTTP failure"));
        }
        if response
            .headers()
            .get("content-type")
            .and_then(|v| v.to_str().ok())
            != Some("application/dns-message")
        {
            return Err(io::Error::other("invalid DoH content type"));
        }
        let length = response
            .headers()
            .get("content-length")
            .map(|v| {
                v.to_str()
                    .map_err(io::Error::other)?
                    .parse::<usize>()
                    .map_err(io::Error::other)
            })
            .transpose()?;
        if length.is_some_and(|n| n > MAX_MESSAGE) {
            return Err(io::Error::other("oversized DoH length"));
        }
        let age = response
            .headers()
            .get("age")
            .map(|v| {
                v.to_str()
                    .map_err(io::Error::other)?
                    .parse::<u32>()
                    .map_err(io::Error::other)
            })
            .transpose()?
            .unwrap_or(0);
        let mut body = response.into_body();
        let mut bytes = Vec::with_capacity(length.unwrap_or(512));
        while let Some(chunk) = body.data().await {
            let chunk = chunk.map_err(io::Error::other)?;
            if chunk.len() > MAX_MESSAGE - bytes.len() {
                return Err(io::Error::other("oversized DoH body"));
            }
            bytes.extend_from_slice(&chunk);
            body.flow_control()
                .release_capacity(chunk.len())
                .map_err(io::Error::other)?;
        }
        if length.is_some_and(|n| n != bytes.len()) {
            return Err(io::Error::other("DoH length mismatch"));
        }
        let mut message = Message::from_vec(&bytes).map_err(io::Error::other)?;
        let negative = message.response_code == ResponseCode::NXDomain
            || !message.answers.iter().any(|rr| {
                message
                    .queries
                    .first()
                    .is_some_and(|q| rr.record_type() == q.query_type())
            });
        if negative {
            for rr in &mut message.authorities {
                if let RData::SOA(soa) = &rr.data {
                    rr.ttl = rr.ttl.min(soa.minimum);
                }
            }
        }
        for rr in message
            .answers
            .iter_mut()
            .chain(&mut message.authorities)
            .chain(&mut message.additionals)
        {
            rr.ttl = rr.ttl.saturating_sub(age);
        }
        Ok((message, age))
    }
}
