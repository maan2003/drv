// SPDX-License-Identifier: GPL-2.0-only
//! Conservative full-message cache. Inspired by dnscrypt-proxy's packet cache,
//! not its TTL extension/serve-stale policy. Payload+key bytes are capped; entry
//! count separately bounds container overhead. Unknown EDNS semantics bypass it.
use hickory_proto::op::{Message, ResponseCode};
use hickory_proto::rr::RData;
use std::{
    collections::VecDeque,
    time::{Duration, Instant},
};

const ENTRIES: usize = 256;
const BYTES: usize = 4 * 1024 * 1024;
struct Entry {
    key: Vec<u8>,
    wire: Vec<u8>,
    inserted: Instant,
    lifetime: Duration,
}
#[derive(Default)]
pub(crate) struct Cache {
    entries: VecDeque<Entry>,
    bytes: usize,
}
pub(crate) fn eligible(message: &Message) -> bool {
    message.signature.is_none()
        && message.additionals.is_empty()
        && message
            .edns
            .as_ref()
            .is_none_or(|e| e.options().as_ref().is_empty())
}
pub(crate) fn age(message: &mut Message, seconds: u32) {
    for rr in message
        .answers
        .iter_mut()
        .chain(&mut message.authorities)
        .chain(&mut message.additionals)
    {
        rr.ttl = rr.ttl.saturating_sub(seconds);
    }
}
impl Cache {
    pub fn get(&mut self, key: &[u8], now: Instant) -> Option<Message> {
        let index = self.entries.iter().position(|e| e.key == key)?;
        let entry = self.entries.remove(index)?;
        self.bytes -= entry.key.len() + entry.wire.len();
        let elapsed = now.saturating_duration_since(entry.inserted);
        if elapsed >= entry.lifetime {
            return None;
        }
        let mut message = Message::from_vec(&entry.wire).ok()?;
        age(&mut message, elapsed.as_secs().min(u32::MAX as u64) as u32);
        self.bytes += entry.key.len() + entry.wire.len();
        self.entries.push_back(entry);
        Some(message)
    }
    // Response TTLs already include HTTP Age, but SOA.MINIMUM does not.
    pub fn insert(&mut self, key: &[u8], response: &Message, http_age: u32, now: Instant) {
        if response.truncation
            || response.signature.is_some()
            || response
                .edns
                .as_ref()
                .is_some_and(|e| !e.options().as_ref().is_empty())
            || !matches!(
                response.response_code,
                ResponseCode::NoError | ResponseCode::NXDomain
            )
        {
            return;
        }
        let mut ttl = response
            .answers
            .iter()
            .chain(&response.authorities)
            .chain(&response.additionals)
            .map(|rr| rr.ttl)
            .min()
            .unwrap_or(0);
        let negative = response.response_code == ResponseCode::NXDomain
            || !response.answers.iter().any(|rr| {
                response
                    .queries
                    .first()
                    .is_some_and(|q| rr.record_type() == q.query_type())
            });
        if negative {
            let Some(soa) = response.authorities.iter().find_map(|rr| {
                if let RData::SOA(soa) = &rr.data {
                    Some(rr.ttl.min(soa.minimum.saturating_sub(http_age)))
                } else {
                    None
                }
            }) else {
                return;
            };
            ttl = ttl.min(soa);
        }
        if ttl == 0 {
            return;
        }
        let Ok(wire) = response.to_vec() else {
            return;
        };
        let size = key.len() + wire.len();
        if size > BYTES {
            return;
        }
        if let Some(i) = self.entries.iter().position(|e| e.key == key) {
            let old = self.entries.remove(i).unwrap();
            self.bytes -= old.key.len() + old.wire.len();
        }
        while self.entries.len() >= ENTRIES || self.bytes + size > BYTES {
            let old = self.entries.pop_front().unwrap();
            self.bytes -= old.key.len() + old.wire.len();
        }
        self.bytes += size;
        self.entries.push_back(Entry {
            key: key.to_vec(),
            wire,
            inserted: now,
            lifetime: Duration::from_secs(ttl as u64),
        });
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use hickory_proto::{
        op::Query,
        rr::{
            Name, Record, RecordType,
            rdata::{A, SOA},
        },
    };
    fn response() -> Message {
        let name = Name::from_ascii("example.test.").unwrap();
        let mut message = Message::response(0, hickory_proto::op::OpCode::Query);
        message.add_query(Query::query(name.clone(), RecordType::A));
        message.answers.push(Record::from_rdata(
            name,
            10,
            RData::A(A("192.0.2.1".parse().unwrap())),
        ));
        message
    }
    #[test]
    fn age_expiry_and_replacement_accounting() {
        let mut cache = Cache::default();
        let now = Instant::now();
        let m = response();
        cache.insert(b"q", &m, 0, now);
        let bytes = cache.bytes;
        cache.insert(b"q", &m, 0, now);
        assert_eq!(cache.bytes, bytes);
        assert_eq!(
            cache
                .get(b"q", now + Duration::from_secs(4))
                .unwrap()
                .answers[0]
                .ttl,
            6
        );
        assert!(cache.get(b"q", now + Duration::from_secs(10)).is_none());
        assert_eq!(cache.bytes, 0);
        for i in 0..300u32 {
            cache.insert(&i.to_be_bytes(), &m, 0, now);
        }
        assert_eq!(cache.entries.len(), ENTRIES);
        assert!(cache.bytes <= BYTES);
    }
    #[test]
    fn negative_soa_minimum_and_http_age() {
        let mut cache = Cache::default();
        let now = Instant::now();
        let mut m = response();
        m.answers.clear();
        m.metadata.response_code = ResponseCode::NXDomain;
        cache.insert(b"q", &m, 12, now);
        assert!(cache.entries.is_empty());
        m.authorities.push(Record::from_rdata(
            Name::root(),
            20,
            RData::SOA(SOA::new(Name::root(), Name::root(), 1, 60, 60, 60, 20)),
        ));
        age(&mut m, 12);
        cache.insert(b"q", &m, 12, now);
        assert_eq!(cache.get(b"q", now).unwrap().authorities[0].ttl, 8);
        assert_eq!(
            cache
                .get(b"q", now + Duration::from_secs(7))
                .unwrap()
                .authorities[0]
                .ttl,
            1
        );
        assert!(cache.get(b"q", now + Duration::from_secs(8)).is_none());
    }
    #[test]
    fn byte_budget_and_oversize_bypass() {
        let mut cache = Cache::default();
        let now = Instant::now();
        let m = response();
        let mut key = vec![0; 60000];
        for i in 0..100u8 {
            key[0] = i;
            cache.insert(&key, &m, 0, now);
        }
        assert!(cache.entries.len() < 100);
        assert!(cache.bytes <= BYTES);
        let count = cache.entries.len();
        cache.insert(&vec![0; BYTES], &m, 0, now);
        assert_eq!(cache.entries.len(), count);
    }
    #[test]
    fn transient_or_truncated_never_cached() {
        let mut cache = Cache::default();
        let mut m = response();
        let now = Instant::now();
        m.metadata.response_code = ResponseCode::ServFail;
        cache.insert(b"q", &m, 0, now);
        m.metadata.response_code = ResponseCode::NoError;
        m.metadata.truncation = true;
        cache.insert(b"q", &m, 0, now);
        assert!(cache.entries.is_empty());
    }
}
