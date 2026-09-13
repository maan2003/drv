// Copyright 2015-2019 Benjamin Fry <benjaminfry@me.com>
// SPDX-License-Identifier: MIT
// UDP/TCP lifecycle adapted from Hickory server/mod.rs at f09321075;
// admission precedes spawn, bounded direct replies replace unbounded channels.
use crate::{MAX_CLIENTS, MAX_MESSAGE, QUERY_TIMEOUT, forward::Forwarder, forward::encode};
use hickory_proto::{
    op::{Message, Query, ResponseCode},
    rr::{Name, RData, RecordType},
};
use std::{io, sync::Arc};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream, UdpSocket, UnixListener, UnixStream},
    sync::Semaphore,
    task::JoinSet,
    time::{Instant, timeout_at},
};

pub async fn udp(
    socket: UdpSocket,
    engine: Arc<Forwarder>,
    capacity: Arc<Semaphore>,
) -> io::Result<()> {
    let socket = Arc::new(socket);
    let mut tasks = JoinSet::new();
    let mut buffer = vec![0; MAX_MESSAGE + 1];
    loop {
        tokio::select! {
            result = tasks.join_next(), if !tasks.is_empty() => {
                if result.is_some_and(|r| r.is_err()) { return Err(io::Error::other("DNS UDP worker failed")); }
            }
            result = socket.recv_from(&mut buffer) => {
                let (n, peer) = result?;
                if n > MAX_MESSAGE { continue; }
                let Ok(permit) = capacity.clone().try_acquire_owned() else { continue; };
                let bytes = buffer[..n].to_vec();
                let socket = socket.clone();
                let engine = engine.clone();
                tasks.spawn(async move {
                    let _permit = permit;
                    let deadline = Instant::now() + QUERY_TIMEOUT;
                    let _ = timeout_at(deadline, async {
                        let request = Message::from_vec(&bytes).map_err(io::Error::other)?;
                        let limit = request.max_payload().min(1232) as usize;
                        let response = engine.exchange(request, deadline).await;
                        socket.send_to(&encode(&response, limit)?, peer).await?;
                        Ok::<_, io::Error>(())
                    }).await;
                });
            }
        }
    }
}
pub async fn tcp(
    listener: TcpListener,
    engine: Arc<Forwarder>,
    capacity: Arc<Semaphore>,
) -> io::Result<()> {
    let connections = Arc::new(Semaphore::new(MAX_CLIENTS));
    let mut tasks = JoinSet::new();
    loop {
        tokio::select! {
            result = tasks.join_next(), if !tasks.is_empty() => {
                if result.is_some_and(|r| r.is_err()) { return Err(io::Error::other("DNS TCP worker failed")); }
            }
            result = listener.accept() => {
                let (stream, _) = result?;
                let Ok(connection) = connections.clone().try_acquire_owned() else { continue; };
                let engine = engine.clone();
                let capacity = capacity.clone();
                tasks.spawn(async move {
                    let _connection = connection;
                    let _ = tcp_client(stream, engine, capacity).await;
                });
            }
        }
    }
}
async fn tcp_client(
    mut stream: TcpStream,
    engine: Arc<Forwarder>,
    capacity: Arc<Semaphore>,
) -> io::Result<()> {
    loop {
        let deadline = Instant::now() + QUERY_TIMEOUT;
        // Includes prefix, partial body, capacity wait, lookup and response write.
        timeout_at(deadline, async {
            let size = stream.read_u16().await? as usize;
            if size < 12 {
                return Err(io::Error::other("short DNS frame"));
            }
            let _permit = capacity.acquire().await.map_err(io::Error::other)?;
            let mut bytes = vec![0; size];
            stream.read_exact(&mut bytes).await?;
            let request = Message::from_vec(&bytes).map_err(io::Error::other)?;
            let response = engine.exchange(request, deadline).await;
            let bytes = encode(&response, MAX_MESSAGE)?;
            stream.write_u16(bytes.len() as u16).await?;
            stream.write_all(&bytes).await
        })
        .await
        .map_err(io::Error::other)??;
    }
}
pub async fn nss(
    listener: UnixListener,
    engine: Arc<Forwarder>,
    capacity: Arc<Semaphore>,
) -> io::Result<()> {
    let connections = Arc::new(Semaphore::new(MAX_CLIENTS));
    let mut tasks = JoinSet::new();
    loop {
        tokio::select! {
            result = tasks.join_next(), if !tasks.is_empty() => {
                if result.is_some_and(|r| r.is_err()) { return Err(io::Error::other("NSS worker failed")); }
            }
            result = listener.accept() => {
                let (stream, _) = result?;
                let Ok(connection) = connections.clone().try_acquire_owned() else { continue; };
                let engine = engine.clone();
                let capacity = capacity.clone();
                tasks.spawn(async move {
                    let _connection = connection;
                    let deadline = Instant::now() + QUERY_TIMEOUT;
                    let _ = timeout_at(deadline, nss_client(stream, engine, capacity, deadline)).await;
                });
            }
        }
    }
}
async fn nss_client(
    mut stream: UnixStream,
    engine: Arc<Forwarder>,
    capacity: Arc<Semaphore>,
    deadline: Instant,
) -> io::Result<()> {
    use drv_dns_wire as wire;
    let mut bytes = [0; wire::REQUEST_LEN];
    stream.read_exact(&mut bytes).await?;
    let name = wire::name(&bytes).ok_or_else(|| io::Error::other("invalid NSS request"))?;
    let mut name = Name::from_ascii(name).map_err(io::Error::other)?;
    name.set_fqdn(true);
    let _permit = capacity.acquire_many(2).await.map_err(io::Error::other)?;
    let lookup = async {
        // Reserve time to deliver completed results if the other family times out.
        let lookup_deadline = deadline - std::time::Duration::from_millis(100);
        let resolve = |kind| {
            let mut request = Message::query();
            request.metadata.recursion_desired = true;
            request.add_query(Query::query(name.clone(), kind));
            engine.exchange(request, lookup_deadline)
        };
        let (a, aaaa) = tokio::join!(resolve(RecordType::A), resolve(RecordType::AAAA));
        let mut ips = Vec::new();
        let mut temporary = false;
        for (kind, response) in [(RecordType::A, a), (RecordType::AAAA, aaaa)] {
            match response.response_code {
                ResponseCode::NoError => ips.extend(addresses(&name, kind, &response)),
                ResponseCode::NXDomain => {}
                _ => temporary = true,
            }
        }
        ips.truncate(wire::MAX_ADDRESSES);
        wire::response(
            if !ips.is_empty() {
                wire::OK
            } else if temporary {
                wire::TEMPORARY
            } else {
                wire::NOT_FOUND
            },
            &ips,
        )
    };
    // A peer closes (or sends forbidden extra bytes): drop its lookup future.
    let mut extra = [0];
    let response = tokio::select! {
        response = lookup => response,
        _ = stream.read(&mut extra) => return Ok(()),
    };
    stream.write_all(&response).await
}

// NSS accepts only the requested family's records at the queried/CNAME owner.
// A cycle or excessively long alias chain is not a source of arbitrary addresses.
pub fn addresses(name: &Name, kind: RecordType, response: &Message) -> Vec<std::net::IpAddr> {
    let mut owner = name.clone();
    let mut visited = Vec::new();
    for _ in 0..16 {
        if visited.contains(&owner) {
            return Vec::new();
        }
        visited.push(owner.clone());
        if let Some(target) = response.answers.iter().find_map(|rr| {
            if rr.name != owner {
                return None;
            }
            if let RData::CNAME(target) = &rr.data {
                Some(target.0.clone())
            } else {
                None
            }
        }) {
            owner = target;
        } else {
            return response
                .answers
                .iter()
                .filter_map(|rr| {
                    if rr.name != owner {
                        return None;
                    }
                    match (&rr.data, kind) {
                        (RData::A(ip), RecordType::A) => Some(std::net::IpAddr::V4(ip.0)),
                        (RData::AAAA(ip), RecordType::AAAA) => Some(std::net::IpAddr::V6(ip.0)),
                        _ => None,
                    }
                })
                .take(drv_dns_wire::MAX_ADDRESSES)
                .collect();
        }
    }
    Vec::new()
}
