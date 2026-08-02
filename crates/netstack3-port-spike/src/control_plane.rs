//! Sans-I/O DHCPv4 and DNS protocol adapters for Netstack3 UDP sockets.
//!
//! Callers supply entropy, elapsed time, retransmission policy, and datagram
//! transport explicitly. This module never opens a socket or reads host state.

use std::error::Error;
use std::fmt;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};

use edge_dhcp::{DhcpOption, MessageType as DhcpMessageType, Options, Packet, Settings, client};
use hickory_proto::op::{Message, MessageType, OpCode, Query, ResponseCode};
use hickory_proto::rr::{Name, RData, RecordType};
use rand_core_06::RngCore;

/// Maximum UDP payload accepted at the control-plane boundary. This fits the
/// IPv6 minimum MTU without fragmentation (1280 - IPv6 header - UDP header).
pub const MAX_CONTROL_DATAGRAM_LEN: usize = 1232;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ControlDatagram(Box<[u8]>);

impl ControlDatagram {
    pub fn copy_from_slice(bytes: &[u8]) -> Result<Self, ControlPlaneError> {
        if bytes.len() > MAX_CONTROL_DATAGRAM_LEN {
            return Err(ControlPlaneError::DatagramTooLarge { len: bytes.len() });
        }
        Ok(Self(bytes.into()))
    }

    pub fn as_bytes(&self) -> &[u8] {
        &self.0
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DhcpOffer {
    pub address: Ipv4Addr,
    pub server: Ipv4Addr,
    pub lease_seconds: Option<u32>,
    pub renewal_seconds: Option<u32>,
    pub rebinding_seconds: Option<u32>,
    pub gateway: Option<Ipv4Addr>,
    pub subnet_mask: Option<Ipv4Addr>,
    pub dns_servers: [Option<Ipv4Addr>; 2],
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DhcpLeaseTiming {
    pub renew_after_seconds: u32,
    pub rebind_after_seconds: u32,
    pub expire_after_seconds: u32,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DhcpLeasePhase {
    Bound,
    Renew,
    Rebind,
    Expired,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DhcpLeaseSchedule {
    renew_at_seconds: u64,
    rebind_at_seconds: u64,
    expire_at_seconds: u64,
}

impl DhcpLeaseSchedule {
    pub fn new(acquired_at_seconds: u64, timing: DhcpLeaseTiming) -> Self {
        Self {
            renew_at_seconds: acquired_at_seconds
                .saturating_add(u64::from(timing.renew_after_seconds)),
            rebind_at_seconds: acquired_at_seconds
                .saturating_add(u64::from(timing.rebind_after_seconds)),
            expire_at_seconds: acquired_at_seconds
                .saturating_add(u64::from(timing.expire_after_seconds)),
        }
    }

    pub fn phase(self, now_seconds: u64) -> DhcpLeasePhase {
        if now_seconds >= self.expire_at_seconds {
            DhcpLeasePhase::Expired
        } else if now_seconds >= self.rebind_at_seconds {
            DhcpLeasePhase::Rebind
        } else if now_seconds >= self.renew_at_seconds {
            DhcpLeasePhase::Renew
        } else {
            DhcpLeasePhase::Bound
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DhcpReply {
    Ack(DhcpOffer),
    Nak,
}

impl DhcpOffer {
    pub fn prefix_len(self) -> Option<u8> {
        let mask = u32::from(self.subnet_mask?);
        let prefix = mask.leading_ones();
        let expected = if prefix == 0 {
            0
        } else {
            u32::MAX << (32 - prefix)
        };
        (mask == expected).then_some(prefix as u8)
    }

    /// Returns RFC 2131 lease deadlines, using T1=0.5 and T2=0.875 of the
    /// lease when the server omitted or supplied inconsistent values.
    pub fn timing(self) -> Option<DhcpLeaseTiming> {
        let lease = self.lease_seconds?;
        let defaults = (lease / 2, (u64::from(lease) * 7 / 8) as u32);
        let renewal = self.renewal_seconds.unwrap_or(defaults.0);
        let rebinding = self.rebinding_seconds.unwrap_or(defaults.1);
        let (renewal, rebinding) = if renewal < rebinding && rebinding < lease {
            (renewal, rebinding)
        } else {
            defaults
        };
        Some(DhcpLeaseTiming {
            renew_after_seconds: renewal,
            rebind_after_seconds: rebinding,
            expire_after_seconds: lease,
        })
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DhcpTransmission {
    pub transaction_id: u32,
    /// Seconds elapsed since acquisition began, supplied by the caller.
    pub elapsed_seconds: u16,
    /// Server selected by a DHCPREQUEST, if this is not a broadcast discovery.
    pub server: Option<Ipv4Addr>,
    /// Rebinding accepts an ACK from any server identifier.
    pub accept_any_server: bool,
}

/// Pure DHCPv4 message state. UDP ports 68 -> 67, retry deadlines, and applying
/// a lease to Netstack3 are responsibilities of the explicit service adapter.
pub struct Dhcpv4Client<R> {
    inner: client::Client<R>,
}

impl<R: RngCore> Dhcpv4Client<R> {
    pub const fn new(random: R, mac: [u8; 6]) -> Self {
        Self {
            inner: client::Client::new(random, mac),
        }
    }

    pub fn discover(
        &mut self,
        elapsed_seconds: u16,
    ) -> Result<(DhcpTransmission, ControlDatagram), ControlPlaneError> {
        let mut options = Options::buf();
        let (packet, transaction_id) = self.inner.discover(&mut options, elapsed_seconds, None);
        let bytes = encode_dhcp(&packet)?;
        Ok((
            DhcpTransmission {
                transaction_id,
                elapsed_seconds,
                server: None,
                accept_any_server: false,
            },
            bytes,
        ))
    }

    pub fn accept_offer(
        &self,
        transaction: DhcpTransmission,
        bytes: &[u8],
    ) -> Result<Option<DhcpOffer>, ControlPlaneError> {
        ensure_bounded(bytes)?;
        let packet = Packet::decode(bytes).map_err(ControlPlaneError::Dhcp)?;
        if !self.inner.is_offer(&packet, transaction.transaction_id) {
            return Ok(None);
        }
        packet_to_offer(&packet).map(Some)
    }

    pub fn request(
        &mut self,
        elapsed_seconds: u16,
        offer: DhcpOffer,
    ) -> Result<(DhcpTransmission, ControlDatagram), ControlPlaneError> {
        const REQUEST_PARAMETERS: &[u8] = &[
            DhcpOption::CODE_ROUTER,
            DhcpOption::CODE_SUBNET,
            DhcpOption::CODE_DNS,
        ];
        let options = [
            DhcpOption::MessageType(DhcpMessageType::Request),
            DhcpOption::RequestedIpAddress(offer.address),
            DhcpOption::ServerIdentifier(offer.server),
            DhcpOption::ParameterRequestList(REQUEST_PARAMETERS),
        ];
        let (packet, transaction_id) =
            self.inner
                .bootp_request(elapsed_seconds, None, true, Options::new(&options));
        let bytes = encode_dhcp(&packet)?;
        Ok((
            DhcpTransmission {
                transaction_id,
                elapsed_seconds,
                server: Some(offer.server),
                accept_any_server: false,
            },
            bytes,
        ))
    }

    /// Builds a DHCPREQUEST for T1 renewal or T2 rebinding. The configured
    /// address is carried in `ciaddr`; rebinding uses broadcast delivery and
    /// accepts a response from any server.
    pub fn renew(
        &mut self,
        elapsed_seconds: u16,
        lease: DhcpOffer,
        rebinding: bool,
    ) -> Result<(DhcpTransmission, ControlDatagram), ControlPlaneError> {
        const REQUEST_PARAMETERS: &[u8] = &[
            DhcpOption::CODE_ROUTER,
            DhcpOption::CODE_SUBNET,
            DhcpOption::CODE_DNS,
        ];
        let options = [
            DhcpOption::MessageType(DhcpMessageType::Request),
            DhcpOption::ParameterRequestList(REQUEST_PARAMETERS),
        ];
        let (packet, transaction_id) = self.inner.bootp_request(
            elapsed_seconds,
            Some(lease.address),
            rebinding,
            Options::new(&options),
        );
        let bytes = encode_dhcp(&packet)?;
        Ok((
            DhcpTransmission {
                transaction_id,
                elapsed_seconds,
                server: (!rebinding).then_some(lease.server),
                accept_any_server: rebinding,
            },
            bytes,
        ))
    }

    pub fn accept_ack(
        &self,
        transaction: DhcpTransmission,
        bytes: &[u8],
    ) -> Result<Option<DhcpOffer>, ControlPlaneError> {
        Ok(match self.accept_reply(transaction, bytes)? {
            Some(DhcpReply::Ack(offer)) => Some(offer),
            Some(DhcpReply::Nak) | None => None,
        })
    }

    pub fn accept_reply(
        &self,
        transaction: DhcpTransmission,
        bytes: &[u8],
    ) -> Result<Option<DhcpReply>, ControlPlaneError> {
        ensure_bounded(bytes)?;
        let packet = Packet::decode(bytes).map_err(ControlPlaneError::Dhcp)?;
        if self.inner.is_nak(&packet, transaction.transaction_id) {
            let server = Settings::new(&packet).server_ip;
            return Ok((transaction.accept_any_server
                || (transaction.server.is_some() && transaction.server == server))
                .then_some(DhcpReply::Nak));
        }
        if !self.inner.is_ack(&packet, transaction.transaction_id) {
            return Ok(None);
        }
        let offer = packet_to_offer(&packet)?;
        Ok(
            (transaction.accept_any_server || transaction.server == Some(offer.server))
                .then_some(DhcpReply::Ack(offer)),
        )
    }
}

fn encode_dhcp(packet: &Packet<'_>) -> Result<ControlDatagram, ControlPlaneError> {
    let mut storage = [0; MAX_CONTROL_DATAGRAM_LEN];
    let bytes = packet
        .encode(&mut storage)
        .map_err(ControlPlaneError::Dhcp)?;
    ControlDatagram::copy_from_slice(bytes)
}

fn ensure_bounded(bytes: &[u8]) -> Result<(), ControlPlaneError> {
    if bytes.len() > MAX_CONTROL_DATAGRAM_LEN {
        return Err(ControlPlaneError::DatagramTooLarge { len: bytes.len() });
    }
    Ok(())
}

fn packet_to_offer(packet: &Packet<'_>) -> Result<DhcpOffer, ControlPlaneError> {
    let settings = Settings::new(packet);
    let server = settings
        .server_ip
        .ok_or(ControlPlaneError::DhcpMissingServer)?;
    Ok(DhcpOffer {
        address: settings.ip,
        server,
        lease_seconds: settings.lease_time_secs,
        renewal_seconds: dhcp_u32_option(packet, 58),
        rebinding_seconds: dhcp_u32_option(packet, 59),
        gateway: settings.gateway,
        subnet_mask: settings.subnet,
        dns_servers: [settings.dns1, settings.dns2],
    })
}

fn dhcp_u32_option(packet: &Packet<'_>, code: u8) -> Option<u32> {
    packet.options.iter().find_map(|option| match option {
        DhcpOption::Unrecognized(found, bytes) if found == code => {
            <[u8; 4]>::try_from(bytes).ok().map(u32::from_be_bytes)
        }
        _ => None,
    })
}

/// Builds and validates ordinary A/AAAA DNS exchanges. Send the returned bytes
/// to an explicitly configured DNS server on UDP port 53 through Netstack3.
pub struct DnsCodec;

#[derive(Clone, Debug)]
pub struct DnsQuery {
    transaction_id: u16,
    name: Name,
    record_type: RecordType,
    datagram: ControlDatagram,
}

impl DnsQuery {
    pub fn transaction_id(&self) -> u16 {
        self.transaction_id
    }

    pub fn datagram(&self) -> &ControlDatagram {
        &self.datagram
    }
}

impl DnsCodec {
    pub fn query(
        transaction_id: u16,
        name: &str,
        record_type: RecordType,
    ) -> Result<DnsQuery, ControlPlaneError> {
        if !matches!(record_type, RecordType::A | RecordType::AAAA) {
            return Err(ControlPlaneError::UnsupportedDnsRecord);
        }
        let name =
            Name::from_ascii(name).map_err(|error| ControlPlaneError::Dns(error.to_string()))?;
        let mut message = Message::new(transaction_id, MessageType::Query, OpCode::Query);
        message.metadata.recursion_desired = true;
        message.add_query(Query::query(name.clone(), record_type));
        let bytes = message
            .to_vec()
            .map_err(|error| ControlPlaneError::Dns(error.to_string()))?;
        Ok(DnsQuery {
            transaction_id,
            name,
            record_type,
            datagram: ControlDatagram::copy_from_slice(&bytes)?,
        })
    }

    pub fn response(query: &DnsQuery, bytes: &[u8]) -> Result<Vec<IpAddr>, ControlPlaneError> {
        let message = validated_dns_response(query, bytes, MAX_CONTROL_DATAGRAM_LEN)?;
        Ok(dns_addresses(&message))
    }
}

fn validated_dns_response(
    query: &DnsQuery,
    bytes: &[u8],
    maximum_len: usize,
) -> Result<Message, ControlPlaneError> {
    if bytes.len() > maximum_len {
        return Err(ControlPlaneError::DatagramTooLarge { len: bytes.len() });
    }
    let message =
        Message::from_vec(bytes).map_err(|error| ControlPlaneError::Dns(error.to_string()))?;
    if message.metadata.id != query.transaction_id
        || message.metadata.message_type != MessageType::Response
        || message.queries.as_slice()
            != [Query::query(query.name.clone(), query.record_type)].as_slice()
    {
        return Err(ControlPlaneError::DnsTransactionMismatch);
    }
    if message.metadata.response_code != ResponseCode::NoError {
        return Err(ControlPlaneError::DnsResponse(
            message.metadata.response_code,
        ));
    }
    if message.metadata.truncation {
        return Err(ControlPlaneError::DnsTruncated);
    }
    Ok(message)
}

fn dns_addresses(message: &Message) -> Vec<IpAddr> {
    message
        .answers
        .iter()
        .filter_map(|record| match &record.data {
            RData::A(address) => Some(IpAddr::V4(address.0)),
            RData::AAAA(address) => Some(IpAddr::V6(address.0)),
            _ => None,
        })
        .collect()
}

fn dns_addresses_for_type(message: &Message, record_type: RecordType) -> Vec<IpAddr> {
    dns_addresses(message)
        .into_iter()
        .filter(|address| {
            matches!(
                (address, record_type),
                (IpAddr::V4(_), RecordType::A) | (IpAddr::V6(_), RecordType::AAAA)
            )
        })
        .collect()
}

/// Hard lifecycle bounds. Responses beyond [`MAX_DNS_CACHE_ADDRESSES`] are
/// accepted, but only the first addresses are retained and returned.
pub const MAX_DNS_SERVERS: usize = 4;
pub const MAX_OUTSTANDING_DNS_QUERIES: usize = 8;
pub const MAX_DNS_CACHE_ENTRIES: usize = 32;
pub const MAX_DNS_CACHE_ADDRESSES: usize = 16;
pub const MAX_DNS_TCP_MESSAGE_LEN: usize = u16::MAX as usize;

/// The two endpoints of a connected DNS transport. Keeping both endpoints in
/// every transmission and response prevents a packet received by another
/// local socket or from another configured server from completing a query.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DnsAssociation {
    pub source: SocketAddr,
    pub server: SocketAddr,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DnsTransport {
    Udp,
    Tcp,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DnsResolverConfig {
    pub timeout_millis: u64,
    /// Additional transmissions to one server before moving to the next.
    pub retries_per_server: u8,
}

impl Default for DnsResolverConfig {
    fn default() -> Self {
        Self {
            timeout_millis: 1_000,
            retries_per_server: 1,
        }
    }
}

/// A transport operation emitted by [`DnsResolver`]. TCP payloads are DNS
/// messages without the two-byte stream length prefix; framing stays with the
/// caller that owns the stream.
#[derive(Clone, Debug)]
pub struct DnsRequest {
    lookup_id: u64,
    association: DnsAssociation,
    transport: DnsTransport,
    query: DnsQuery,
}

impl DnsRequest {
    pub fn lookup_id(&self) -> u64 {
        self.lookup_id
    }

    pub fn association(&self) -> DnsAssociation {
        self.association
    }

    pub fn transport(&self) -> DnsTransport {
        self.transport
    }

    pub fn query(&self) -> &DnsQuery {
        &self.query
    }
}

#[derive(Clone, Debug)]
pub enum DnsResolverEvent {
    Transmit(DnsRequest),
    Answer {
        lookup_id: u64,
        addresses: Vec<IpAddr>,
    },
    Failed {
        lookup_id: u64,
    },
}

#[derive(Clone, Debug)]
struct PendingDnsQuery {
    lookup_id: u64,
    name: Name,
    record_type: RecordType,
    query: DnsQuery,
    server_index: usize,
    transport: DnsTransport,
    transmissions_on_server: u16,
    deadline_millis: u64,
}

#[derive(Clone, Debug)]
struct DnsCacheEntry {
    name: Name,
    record_type: RecordType,
    addresses: Vec<IpAddr>,
    expires_at_millis: u64,
    inserted_at_millis: u64,
}

/// A bounded, sans-I/O DNS query lifecycle. The caller supplies monotonic time,
/// transaction-ID entropy, configured connected endpoints, and all transport.
pub struct DnsResolver<R> {
    random: R,
    associations: Vec<DnsAssociation>,
    config: DnsResolverConfig,
    outstanding: Vec<PendingDnsQuery>,
    cache: Vec<DnsCacheEntry>,
    next_lookup_id: u64,
}

impl<R: RngCore> DnsResolver<R> {
    pub fn new(
        random: R,
        associations: &[DnsAssociation],
        config: DnsResolverConfig,
    ) -> Result<Self, ControlPlaneError> {
        if associations.is_empty() {
            return Err(ControlPlaneError::NoDnsServers);
        }
        if associations.len() > MAX_DNS_SERVERS {
            return Err(ControlPlaneError::TooManyDnsServers {
                len: associations.len(),
            });
        }
        if config.timeout_millis == 0 {
            return Err(ControlPlaneError::InvalidDnsResolverConfig);
        }
        Ok(Self {
            random,
            associations: associations.to_vec(),
            config,
            outstanding: Vec::new(),
            cache: Vec::new(),
            next_lookup_id: 0,
        })
    }

    pub fn outstanding_len(&self) -> usize {
        self.outstanding.len()
    }

    pub fn cache_len(&self) -> usize {
        self.cache.len()
    }

    /// Starts a lookup or returns a non-expired cached answer. `now_millis`
    /// comes from the caller's monotonic clock.
    pub fn lookup(
        &mut self,
        now_millis: u64,
        name: &str,
        record_type: RecordType,
    ) -> Result<DnsResolverEvent, ControlPlaneError> {
        if !matches!(record_type, RecordType::A | RecordType::AAAA) {
            return Err(ControlPlaneError::UnsupportedDnsRecord);
        }
        let name =
            Name::from_ascii(name).map_err(|error| ControlPlaneError::Dns(error.to_string()))?;
        self.expire_cache(now_millis);
        let lookup_id = self.allocate_lookup_id();
        if let Some(entry) = self
            .cache
            .iter()
            .find(|entry| entry.name == name && entry.record_type == record_type)
        {
            return Ok(DnsResolverEvent::Answer {
                lookup_id,
                addresses: entry.addresses.clone(),
            });
        }
        if self.outstanding.len() == MAX_OUTSTANDING_DNS_QUERIES {
            return Err(ControlPlaneError::TooManyDnsQueries);
        }
        let transaction_id = self.next_transaction_id(None);
        let query = DnsCodec::query(transaction_id, &name.to_ascii(), record_type)?;
        let pending = PendingDnsQuery {
            lookup_id,
            name,
            record_type,
            query,
            server_index: 0,
            transport: DnsTransport::Udp,
            transmissions_on_server: 1,
            deadline_millis: now_millis.saturating_add(self.config.timeout_millis),
        };
        let request = self.request_for(&pending);
        self.outstanding.push(pending);
        Ok(DnsResolverEvent::Transmit(request))
    }

    /// Accepts an unframed DNS response from a specific connected transport.
    /// Responses not associated with the current server, source, transport,
    /// and transaction are ignored.
    pub fn receive(
        &mut self,
        now_millis: u64,
        association: DnsAssociation,
        transport: DnsTransport,
        bytes: &[u8],
    ) -> Result<Option<DnsResolverEvent>, ControlPlaneError> {
        let Some(transaction_id) = bytes
            .get(..2)
            .and_then(|bytes| <[u8; 2]>::try_from(bytes).ok())
            .map(u16::from_be_bytes)
        else {
            return Ok(None);
        };
        let Some(index) = self.outstanding.iter().position(|pending| {
            pending.query.transaction_id == transaction_id
                && self.associations[pending.server_index] == association
                && pending.transport == transport
        }) else {
            return Ok(None);
        };

        let maximum_len = match transport {
            DnsTransport::Udp => MAX_CONTROL_DATAGRAM_LEN,
            DnsTransport::Tcp => MAX_DNS_TCP_MESSAGE_LEN,
        };
        match validated_dns_response(&self.outstanding[index].query, bytes, maximum_len) {
            Err(ControlPlaneError::DnsTruncated) if transport == DnsTransport::Udp => {
                let pending = &mut self.outstanding[index];
                pending.transport = DnsTransport::Tcp;
                pending.transmissions_on_server = 1;
                pending.deadline_millis = now_millis.saturating_add(self.config.timeout_millis);
                let request = self.request_for(&self.outstanding[index]);
                Ok(Some(DnsResolverEvent::Transmit(request)))
            }
            Err(error) => Err(error),
            Ok(message) => {
                let pending = self.outstanding.remove(index);
                let mut addresses = dns_addresses_for_type(&message, pending.record_type);
                addresses.truncate(MAX_DNS_CACHE_ADDRESSES);
                if let Some(ttl) = dns_ttl(&message, pending.record_type)
                    && ttl != 0
                    && !addresses.is_empty()
                {
                    self.insert_cache(
                        now_millis,
                        pending.name,
                        pending.record_type,
                        addresses.clone(),
                        ttl,
                    );
                }
                Ok(Some(DnsResolverEvent::Answer {
                    lookup_id: pending.lookup_id,
                    addresses,
                }))
            }
        }
    }

    /// Advances expired queries by one retry/failover step each. Exhausted
    /// queries emit `Failed` and release their outstanding-query slot.
    pub fn poll_timeouts(
        &mut self,
        now_millis: u64,
    ) -> Result<Vec<DnsResolverEvent>, ControlPlaneError> {
        self.expire_cache(now_millis);
        let mut events = Vec::new();
        let mut index = 0;
        while index < self.outstanding.len() {
            if now_millis < self.outstanding[index].deadline_millis {
                index += 1;
                continue;
            }
            let retry_limit = u16::from(self.config.retries_per_server) + 1;
            if self.outstanding[index].transmissions_on_server >= retry_limit {
                if self.outstanding[index].server_index + 1 == self.associations.len() {
                    let pending = self.outstanding.remove(index);
                    events.push(DnsResolverEvent::Failed {
                        lookup_id: pending.lookup_id,
                    });
                    continue;
                }
                self.outstanding[index].server_index += 1;
                self.outstanding[index].transport = DnsTransport::Udp;
                self.outstanding[index].transmissions_on_server = 0;
            }

            let lookup_id = self.outstanding[index].lookup_id;
            let transaction_id = self.next_transaction_id(Some(lookup_id));
            let name = self.outstanding[index].name.to_ascii();
            let record_type = self.outstanding[index].record_type;
            let query = DnsCodec::query(transaction_id, &name, record_type)?;
            let pending = &mut self.outstanding[index];
            pending.query = query;
            pending.transmissions_on_server += 1;
            pending.deadline_millis = now_millis.saturating_add(self.config.timeout_millis);
            let request = self.request_for(&self.outstanding[index]);
            events.push(DnsResolverEvent::Transmit(request));
            index += 1;
        }
        Ok(events)
    }

    fn allocate_lookup_id(&mut self) -> u64 {
        let id = self.next_lookup_id;
        self.next_lookup_id = self.next_lookup_id.wrapping_add(1);
        id
    }

    fn next_transaction_id(&mut self, exclude_lookup_id: Option<u64>) -> u16 {
        let mut candidate = self.random.next_u32() as u16;
        while self.outstanding.iter().any(|pending| {
            Some(pending.lookup_id) != exclude_lookup_id
                && pending.query.transaction_id == candidate
        }) {
            candidate = candidate.wrapping_add(1);
        }
        candidate
    }

    fn request_for(&self, pending: &PendingDnsQuery) -> DnsRequest {
        DnsRequest {
            lookup_id: pending.lookup_id,
            association: self.associations[pending.server_index],
            transport: pending.transport,
            query: pending.query.clone(),
        }
    }

    fn expire_cache(&mut self, now_millis: u64) {
        self.cache
            .retain(|entry| now_millis < entry.expires_at_millis);
    }

    fn insert_cache(
        &mut self,
        now_millis: u64,
        name: Name,
        record_type: RecordType,
        addresses: Vec<IpAddr>,
        ttl_seconds: u32,
    ) {
        self.expire_cache(now_millis);
        self.cache
            .retain(|entry| entry.name != name || entry.record_type != record_type);
        if self.cache.len() == MAX_DNS_CACHE_ENTRIES {
            let oldest = self
                .cache
                .iter()
                .enumerate()
                .min_by_key(|(_, entry)| entry.inserted_at_millis)
                .map(|(index, _)| index)
                .expect("a full cache has an oldest entry");
            self.cache.remove(oldest);
        }
        self.cache.push(DnsCacheEntry {
            name,
            record_type,
            addresses,
            expires_at_millis: now_millis
                .saturating_add(u64::from(ttl_seconds).saturating_mul(1_000)),
            inserted_at_millis: now_millis,
        });
    }
}

fn dns_ttl(message: &Message, record_type: RecordType) -> Option<u32> {
    message
        .answers
        .iter()
        .filter_map(|record| match (&record.data, record_type) {
            (RData::A(_), RecordType::A) | (RData::AAAA(_), RecordType::AAAA) => Some(record.ttl),
            _ => None,
        })
        .min()
}

#[derive(Debug)]
pub enum ControlPlaneError {
    DatagramTooLarge { len: usize },
    Dhcp(edge_dhcp::Error),
    DhcpMissingServer,
    Dns(String),
    DnsTransactionMismatch,
    DnsResponse(ResponseCode),
    DnsTruncated,
    UnsupportedDnsRecord,
    NoDnsServers,
    TooManyDnsServers { len: usize },
    TooManyDnsQueries,
    InvalidDnsResolverConfig,
}

impl fmt::Display for ControlPlaneError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::DatagramTooLarge { len } => write!(f, "control datagram is {len} bytes"),
            Self::Dhcp(error) => write!(f, "DHCP packet error: {error}"),
            Self::DhcpMissingServer => write!(f, "DHCP response omitted server identifier"),
            Self::Dns(error) => write!(f, "DNS packet error: {error}"),
            Self::DnsTransactionMismatch => write!(f, "DNS response does not match the query"),
            Self::DnsResponse(code) => write!(f, "DNS server returned {code:?}"),
            Self::DnsTruncated => write!(f, "DNS response requires TCP fallback"),
            Self::UnsupportedDnsRecord => write!(f, "only A and AAAA queries are supported"),
            Self::NoDnsServers => write!(f, "at least one DNS server is required"),
            Self::TooManyDnsServers { len } => {
                write!(
                    f,
                    "{len} DNS servers exceed the {MAX_DNS_SERVERS}-server limit"
                )
            }
            Self::TooManyDnsQueries => write!(
                f,
                "DNS queries exceed the {MAX_OUTSTANDING_DNS_QUERIES}-query limit"
            ),
            Self::InvalidDnsResolverConfig => {
                write!(f, "DNS resolver timeout must be non-zero")
            }
        }
    }
}

impl Error for ControlPlaneError {}

#[cfg(test)]
mod tests {
    use super::*;
    use edge_dhcp::{DhcpOption, MessageType as DhcpMessageType};
    use hickory_proto::rr::Record;
    use hickory_proto::rr::rdata::A;

    #[derive(Clone, Copy)]
    struct FixedRandom(u32);

    impl rand_core_06::RngCore for FixedRandom {
        fn next_u32(&mut self) -> u32 {
            self.0
        }
        fn next_u64(&mut self) -> u64 {
            u64::from(self.next_u32())
        }
        fn fill_bytes(&mut self, dest: &mut [u8]) {
            for chunk in dest.chunks_mut(4) {
                let bytes = self.next_u32().to_ne_bytes();
                chunk.copy_from_slice(&bytes[..chunk.len()]);
            }
        }
        fn try_fill_bytes(&mut self, dest: &mut [u8]) -> Result<(), rand_core_06::Error> {
            self.fill_bytes(dest);
            Ok(())
        }
    }

    struct IncrementingRandom(u32);

    impl rand_core_06::RngCore for IncrementingRandom {
        fn next_u32(&mut self) -> u32 {
            let value = self.0;
            self.0 = self.0.wrapping_add(1);
            value
        }
        fn next_u64(&mut self) -> u64 {
            u64::from(self.next_u32())
        }
        fn fill_bytes(&mut self, dest: &mut [u8]) {
            for chunk in dest.chunks_mut(4) {
                let bytes = self.next_u32().to_ne_bytes();
                chunk.copy_from_slice(&bytes[..chunk.len()]);
            }
        }
        fn try_fill_bytes(&mut self, dest: &mut [u8]) -> Result<(), rand_core_06::Error> {
            self.fill_bytes(dest);
            Ok(())
        }
    }

    #[test]
    fn dhcp_is_sans_io_and_rejects_other_transactions() {
        let mac = [2, 0, 0, 0, 0, 1];
        let mut client = Dhcpv4Client::new(FixedRandom(0x1234_5678), mac);
        let (transaction, discover) = client.discover(3).unwrap();
        assert_eq!(transaction.transaction_id, 0x1234_5678);
        let decoded = Packet::decode(discover.as_bytes()).unwrap();
        assert!(!decoded.reply);

        let gateways = [Ipv4Addr::new(192, 0, 2, 1)];
        let dns = [Ipv4Addr::new(192, 0, 2, 53)];
        let mut options = Options::buf();
        let reply_options = decoded.options.reply(
            DhcpMessageType::Offer,
            Ipv4Addr::new(192, 0, 2, 254),
            3600,
            &gateways,
            Some(Ipv4Addr::new(255, 255, 255, 0)),
            &dns,
            None,
            &mut options,
        );
        let offer_packet = decoded.new_reply(Some(Ipv4Addr::new(192, 0, 2, 10)), reply_options);
        let offer = encode_dhcp(&offer_packet).unwrap();
        let lease = client
            .accept_offer(transaction, offer.as_bytes())
            .unwrap()
            .unwrap();
        assert_eq!(lease.address, Ipv4Addr::new(192, 0, 2, 10));

        let wrong = DhcpTransmission {
            transaction_id: 1,
            elapsed_seconds: 3,
            server: None,
            accept_any_server: false,
        };
        assert_eq!(client.accept_offer(wrong, offer.as_bytes()).unwrap(), None);

        let (request_transaction, request) = client.request(4, lease).unwrap();
        let request = Packet::decode(request.as_bytes()).unwrap();
        assert!(request.options.iter().any(|option| {
            option == DhcpOption::ServerIdentifier(Ipv4Addr::new(192, 0, 2, 254))
        }));
        let mut options = Options::buf();
        let reply_options = request.options.reply(
            DhcpMessageType::Ack,
            Ipv4Addr::new(192, 0, 2, 254),
            3600,
            &gateways,
            Some(Ipv4Addr::new(255, 255, 255, 0)),
            &dns,
            None,
            &mut options,
        );
        let ack = request.new_reply(Some(lease.address), reply_options);
        let ack = encode_dhcp(&ack).unwrap();
        let lease = client
            .accept_ack(request_transaction, ack.as_bytes())
            .unwrap()
            .unwrap();
        assert_eq!(lease.gateway, Some(gateways[0]));
        assert_eq!(lease.dns_servers[0], Some(dns[0]));
        assert_eq!(lease.prefix_len(), Some(24));
        assert_eq!(
            lease.timing(),
            Some(DhcpLeaseTiming {
                renew_after_seconds: 1800,
                rebind_after_seconds: 3150,
                expire_after_seconds: 3600,
            })
        );
        let schedule = DhcpLeaseSchedule::new(100, lease.timing().unwrap());
        assert_eq!(schedule.phase(1899), DhcpLeasePhase::Bound);
        assert_eq!(schedule.phase(1900), DhcpLeasePhase::Renew);
        assert_eq!(schedule.phase(3250), DhcpLeasePhase::Rebind);
        assert_eq!(schedule.phase(3700), DhcpLeasePhase::Expired);
        let (renew_tx, renew) = client.renew(1800, lease, false).unwrap();
        let renew = Packet::decode(renew.as_bytes()).unwrap();
        assert_eq!(renew.ciaddr, lease.address);
        assert!(!renew.broadcast);
        assert_eq!(renew_tx.server, Some(lease.server));
        let (rebind_tx, rebind) = client.renew(3150, lease, true).unwrap();
        assert!(Packet::decode(rebind.as_bytes()).unwrap().broadcast);
        assert!(rebind_tx.accept_any_server);

        let mut options = Options::buf();
        let nak_options = request.options.reply(
            DhcpMessageType::Nak,
            Ipv4Addr::new(192, 0, 2, 254),
            0,
            &[],
            None,
            &[],
            None,
            &mut options,
        );
        let nak = request.new_reply(None, nak_options);
        let nak = encode_dhcp(&nak).unwrap();
        assert_eq!(
            client
                .accept_reply(request_transaction, nak.as_bytes())
                .unwrap(),
            Some(DhcpReply::Nak)
        );

        assert!(
            decoded
                .options
                .iter()
                .any(|option| { option == DhcpOption::MessageType(DhcpMessageType::Discover) })
        );
    }

    #[test]
    fn dns_codec_matches_transaction_and_extracts_addresses() {
        let query = DnsCodec::query(9, "example.test.", RecordType::A).unwrap();
        let request = Message::from_vec(query.datagram().as_bytes()).unwrap();
        let mut response = Message::response(9, OpCode::Query);
        response.add_query(request.queries[0].clone());
        response.answers.push(Record::from_rdata(
            Name::from_ascii("example.test.").unwrap(),
            60,
            RData::A(A(Ipv4Addr::new(192, 0, 2, 99))),
        ));
        let bytes = response.to_vec().unwrap();
        assert_eq!(
            DnsCodec::response(&query, &bytes).unwrap(),
            [IpAddr::V4(Ipv4Addr::new(192, 0, 2, 99))]
        );
        assert!(matches!(
            DnsCodec::response(
                &DnsCodec::query(10, "example.test.", RecordType::A).unwrap(),
                &bytes
            ),
            Err(ControlPlaneError::DnsTransactionMismatch)
        ));
        assert!(matches!(
            DnsCodec::response(&query, &vec![0; MAX_CONTROL_DATAGRAM_LEN + 1]),
            Err(ControlPlaneError::DatagramTooLarge { .. })
        ));
        response.metadata.truncation = true;
        assert!(matches!(
            DnsCodec::response(&query, &response.to_vec().unwrap()),
            Err(ControlPlaneError::DnsTruncated)
        ));
    }

    fn dns_association(host: u8, source_port: u16) -> DnsAssociation {
        DnsAssociation {
            source: SocketAddr::from((Ipv4Addr::new(192, 0, 2, 10), source_port)),
            server: SocketAddr::from((Ipv4Addr::new(192, 0, 2, host), 53)),
        }
    }

    fn transmission(event: DnsResolverEvent) -> DnsRequest {
        match event {
            DnsResolverEvent::Transmit(request) => request,
            other => panic!("expected transmission, got {other:?}"),
        }
    }

    fn dns_response(request: &DnsRequest, ttl: u32, address: Ipv4Addr, truncated: bool) -> Vec<u8> {
        let request_message = Message::from_vec(request.query().datagram().as_bytes()).unwrap();
        let mut response = Message::response(request.query().transaction_id(), OpCode::Query);
        response.add_query(request_message.queries[0].clone());
        response.metadata.truncation = truncated;
        if !truncated {
            response.answers.push(Record::from_rdata(
                request_message.queries[0].name.clone(),
                ttl,
                RData::A(A(address)),
            ));
        }
        response.to_vec().unwrap()
    }

    #[test]
    fn dns_resolver_retries_then_fails_over_with_new_injected_ids() {
        let servers = [dns_association(53, 53000), dns_association(54, 53001)];
        let mut resolver = DnsResolver::new(
            IncrementingRandom(0x1234_5678),
            &servers,
            DnsResolverConfig {
                timeout_millis: 100,
                retries_per_server: 1,
            },
        )
        .unwrap();

        let first = transmission(resolver.lookup(0, "retry.test.", RecordType::A).unwrap());
        assert_eq!(first.query().transaction_id(), 0x5678);
        assert_eq!(first.association(), servers[0]);
        assert!(resolver.poll_timeouts(99).unwrap().is_empty());

        let retry = transmission(resolver.poll_timeouts(100).unwrap().remove(0));
        assert_eq!(retry.association(), servers[0]);
        assert_eq!(retry.query().transaction_id(), 0x5679);

        let failover = transmission(resolver.poll_timeouts(200).unwrap().remove(0));
        assert_eq!(failover.association(), servers[1]);
        assert_eq!(failover.transport(), DnsTransport::Udp);
        assert!(
            resolver
                .receive(
                    201,
                    servers[0],
                    DnsTransport::Udp,
                    &dns_response(&failover, 60, Ipv4Addr::new(203, 0, 113, 7), false),
                )
                .unwrap()
                .is_none()
        );

        let answer = resolver
            .receive(
                202,
                servers[1],
                DnsTransport::Udp,
                &dns_response(&failover, 60, Ipv4Addr::new(203, 0, 113, 7), false),
            )
            .unwrap()
            .unwrap();
        assert!(matches!(
            answer,
            DnsResolverEvent::Answer { addresses, .. }
                if addresses == [IpAddr::V4(Ipv4Addr::new(203, 0, 113, 7))]
        ));
        assert_eq!(resolver.outstanding_len(), 0);
    }

    #[test]
    fn dns_resolver_expires_ttl_cache_at_the_deadline() {
        let server = dns_association(53, 53000);
        let mut resolver =
            DnsResolver::new(FixedRandom(7), &[server], DnsResolverConfig::default()).unwrap();
        let request = transmission(resolver.lookup(0, "cache.test.", RecordType::A).unwrap());
        resolver
            .receive(
                10,
                server,
                DnsTransport::Udp,
                &dns_response(&request, 2, Ipv4Addr::new(192, 0, 2, 99), false),
            )
            .unwrap();
        assert_eq!(resolver.cache_len(), 1);
        assert!(matches!(
            resolver
                .lookup(2_009, "cache.test.", RecordType::A)
                .unwrap(),
            DnsResolverEvent::Answer { .. }
        ));
        assert!(matches!(
            resolver
                .lookup(2_010, "cache.test.", RecordType::A)
                .unwrap(),
            DnsResolverEvent::Transmit(_)
        ));
        assert_eq!(resolver.cache_len(), 0);
    }

    #[test]
    fn dns_resolver_uses_tcp_after_truncation_and_recovers() {
        let server = dns_association(53, 53000);
        let mut resolver =
            DnsResolver::new(FixedRandom(11), &[server], DnsResolverConfig::default()).unwrap();
        let udp = transmission(
            resolver
                .lookup(0, "truncated.test.", RecordType::A)
                .unwrap(),
        );
        let tcp = transmission(
            resolver
                .receive(
                    1,
                    server,
                    DnsTransport::Udp,
                    &dns_response(&udp, 0, Ipv4Addr::UNSPECIFIED, true),
                )
                .unwrap()
                .unwrap(),
        );
        assert_eq!(tcp.transport(), DnsTransport::Tcp);
        assert_eq!(tcp.association(), server);
        assert_eq!(tcp.lookup_id(), udp.lookup_id());
        assert_eq!(resolver.outstanding_len(), 1);

        // Once TCP fallback is active, a late UDP answer cannot complete it.
        assert!(
            resolver
                .receive(
                    2,
                    server,
                    DnsTransport::Udp,
                    &dns_response(&udp, 60, Ipv4Addr::new(192, 0, 2, 44), false),
                )
                .unwrap()
                .is_none()
        );
        let answer = resolver
            .receive(
                3,
                server,
                DnsTransport::Tcp,
                &dns_response(&tcp, 60, Ipv4Addr::new(192, 0, 2, 44), false),
            )
            .unwrap()
            .unwrap();
        assert!(matches!(answer, DnsResolverEvent::Answer { .. }));
        assert_eq!(resolver.outstanding_len(), 0);
        assert!(matches!(
            resolver
                .lookup(4, "truncated.test.", RecordType::A)
                .unwrap(),
            DnsResolverEvent::Answer { .. }
        ));
    }

    #[test]
    fn dns_resolver_enforces_outstanding_and_cache_bounds() {
        let server = dns_association(53, 53000);
        let mut resolver =
            DnsResolver::new(FixedRandom(17), &[server], DnsResolverConfig::default()).unwrap();
        for index in 0..MAX_OUTSTANDING_DNS_QUERIES {
            let request = transmission(
                resolver
                    .lookup(0, &format!("outstanding-{index}.test."), RecordType::A)
                    .unwrap(),
            );
            assert_eq!(request.query().transaction_id(), 17 + index as u16);
        }
        assert!(matches!(
            resolver.lookup(0, "one-too-many.test.", RecordType::A),
            Err(ControlPlaneError::TooManyDnsQueries)
        ));

        // Use a fresh resolver to fill one more cache entry than can be kept.
        let mut resolver =
            DnsResolver::new(FixedRandom(23), &[server], DnsResolverConfig::default()).unwrap();
        for index in 0..=MAX_DNS_CACHE_ENTRIES {
            let request = transmission(
                resolver
                    .lookup(index as u64, &format!("cache-{index}.test."), RecordType::A)
                    .unwrap(),
            );
            resolver
                .receive(
                    index as u64,
                    server,
                    DnsTransport::Udp,
                    &dns_response(&request, 60, Ipv4Addr::new(192, 0, 2, 1), false),
                )
                .unwrap();
            assert!(resolver.cache_len() <= MAX_DNS_CACHE_ENTRIES);
        }
        assert_eq!(resolver.cache_len(), MAX_DNS_CACHE_ENTRIES);
    }
}
