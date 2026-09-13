// Copyright 2015-2019 Benjamin Fry <benjaminfry@me.com>
// SPDX-License-Identifier: MIT
// Started as Hickory's store/forwarder.rs at f09321075b1f97902b7bc4ca4ffda7816fcf2971.
// Replaced zone/record lookup with full-message forwarding; see UPSTREAM.md.
use crate::{
    Config, MAX_MESSAGE,
    cache::{self, Cache},
    transport::Endpoint,
};
use hickory_proto::{
    op::{Message, MessageType, OpCode, ResponseCode},
    rr::RecordType,
    serialize::binary::{BinEncodable, BinEncoder},
};
use std::{
    io,
    sync::{Arc, Mutex},
};
use tokio::time::{Duration, Instant};

pub struct Forwarder {
    endpoints: Vec<Endpoint>,
    cache: Mutex<Cache>,
}
impl Forwarder {
    pub fn new(config: Config, tls: rustls::ClientConfig) -> Self {
        let tls = Arc::new(tls);
        Self {
            endpoints: config
                .upstreams
                .into_iter()
                .map(|u| Endpoint::new(u, tls.clone()))
                .collect(),
            cache: Mutex::new(Cache::default()),
        }
    }
    pub async fn exchange(&self, request: Message, deadline: Instant) -> Message {
        if request.message_type != MessageType::Query
            || request.queries.len() != 1
            || !request.answers.is_empty()
            || !request.authorities.is_empty()
        {
            return error(&request, ResponseCode::FormErr);
        }
        if request.op_code != OpCode::Query {
            return error(&request, ResponseCode::NotImp);
        }
        if request.signature.is_some()
            || matches!(
                request.queries[0].query_type(),
                RecordType::AXFR | RecordType::IXFR
            )
        {
            return error(&request, ResponseCode::Refused);
        }
        if request.edns.as_ref().is_some_and(|e| e.version() != 0) {
            return error(&request, ResponseCode::BADVERS);
        }
        let mut upstream = request.clone();
        upstream.metadata.id = 0; // RFC 8484: HTTP exchange correlates requests.
        let Ok(bytes) = upstream.to_vec() else {
            return error(&request, ResponseCode::FormErr);
        };
        if bytes.len() > MAX_MESSAGE {
            return error(&request, ResponseCode::FormErr);
        }
        let cacheable = cache::eligible(&request);
        if cacheable {
            if let Some(mut response) = self
                .cache
                .lock()
                .unwrap()
                .get(&bytes, std::time::Instant::now())
            {
                response.metadata.id = request.id;
                return response;
            }
        }
        for endpoint in &self.endpoints {
            let attempt = deadline.min(Instant::now() + Duration::from_millis(1500));
            if Instant::now() >= attempt {
                break;
            }
            if let Ok((mut response, age)) = endpoint.exchange(&bytes, attempt).await {
                if response.message_type != MessageType::Response
                    || response.id != 0
                    || response.op_code != request.op_code
                    || response.queries != request.queries
                    || response.signature.is_some()
                {
                    continue;
                }
                response.metadata.id = request.id;
                // Forward authenticated upstream AD only when the client understands it.
                if !request.authentic_data
                    && !request.edns.as_ref().is_some_and(|e| e.flags().dnssec_ok)
                {
                    response.metadata.authentic_data = false;
                }
                if cacheable {
                    self.cache.lock().unwrap().insert(
                        &bytes,
                        &response,
                        age,
                        std::time::Instant::now(),
                    );
                }
                return response;
            }
        }
        error(&request, ResponseCode::ServFail)
    }
}
pub fn error(request: &Message, code: ResponseCode) -> Message {
    let mut response = Message::error_msg(request.id, request.op_code, code);
    response.metadata.recursion_desired = request.recursion_desired;
    response.metadata.recursion_available = true;
    response.metadata.checking_disabled = request.checking_disabled;
    response.queries = request.queries.clone();
    if let Some(edns) = &request.edns {
        let mut reply = hickory_proto::op::Edns::new();
        reply.set_max_payload(edns.max_payload().min(1232));
        response.set_edns(reply);
    }
    response
}
pub fn encode(message: &Message, limit: usize) -> io::Result<Vec<u8>> {
    let mut bytes = Vec::with_capacity(512);
    let mut encoder = BinEncoder::new(&mut bytes);
    encoder.set_max_size(limit.min(MAX_MESSAGE) as u16);
    message.emit(&mut encoder).map_err(io::Error::other)?;
    Ok(bytes)
}
