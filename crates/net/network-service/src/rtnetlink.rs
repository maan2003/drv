// SPDX-License-Identifier: GPL-2.0-only
//! Read-only rtnetlink adapter over real interface observations, not Linux IP.
//! The private endpoint supplies authenticated identity; nlmsg_pid is not authority.
use netstack3_port_integration::interfaces::{Address, AddressState, InterfaceSnapshot, PreferredUntil};
use std::collections::{HashMap, VecDeque};
use std::net::IpAddr;
use std::os::fd::{AsRawFd, BorrowedFd, FromRawFd, OwnedFd};
use rustix::{event::epoll, io::Errno};

pub(crate) const REGISTRATION_FD: i32 = 10;
const TAG: u64 = 1 << 63;
pub(crate) const TOKEN: u64 = TAG;
const MAX_MESSAGE: usize = 65536;
const MAX_BYTES: usize = 256 * 1024;
const MAX_RECORDS: usize = 512;
const CLAIM: libc::c_ulong = 0x8008B401;
const LINK: u16 = 16;
const ADDRESS: u16 = 20;
const MULTI: u16 = 2;

fn u16_at(b: &[u8], n: usize) -> u16 { u16::from_ne_bytes(b[n..n + 2].try_into().unwrap()) }
fn u32_at(b: &[u8], n: usize) -> u32 { u32::from_ne_bytes(b[n..n + 4].try_into().unwrap()) }
fn align(n: usize) -> usize { (n + 3) & !3 }
fn attribute(b: &mut Vec<u8>, kind: u16, value: &[u8]) {
    b.extend_from_slice(&((value.len() + 4) as u16).to_ne_bytes());
    b.extend_from_slice(&kind.to_ne_bytes());
    b.extend_from_slice(value);
    b.resize(align(b.len()), 0);
}
fn message(kind: u16, flags: u16, seq: u32, pid: u32, data: &[u8]) -> Vec<u8> {
    let mut b = Vec::with_capacity(align(16 + data.len()));
    b.extend_from_slice(&((16 + data.len()) as u32).to_ne_bytes());
    b.extend_from_slice(&kind.to_ne_bytes());
    b.extend_from_slice(&flags.to_ne_bytes());
    b.extend_from_slice(&seq.to_ne_bytes());
    b.extend_from_slice(&pid.to_ne_bytes());
    b.extend_from_slice(data);
    b.resize(align(b.len()), 0);
    b
}
fn error(request: &[u8], pid: u32, errno: i32) -> Vec<u8> {
    let mut data = (-errno).to_ne_bytes().to_vec();
    let capped = request.len() + 20 > MAX_MESSAGE;
    data.extend_from_slice(if capped { &request[..16] } else { request });
    message(2, if capped { 0x100 } else { 0 }, u32_at(request, 8), pid, &data)
}
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct View {
    interfaces: Vec<InterfaceSnapshot>,
    online: bool,
    mac: [u8; 6],
}
impl View {
    pub(crate) fn new(interfaces: Vec<InterfaceSnapshot>, online: bool, mac: [u8; 6]) -> Self {
        Self { interfaces, online, mac }
    }
    fn index(i: &InterfaceSnapshot) -> Result<u32, i32> {
        if i.id == u64::MAX { Ok(1) }
        else { i.id.checked_add(1).and_then(|id| i32::try_from(id).ok()).map(|id| id as u32).ok_or(libc::EOVERFLOW) }
    }
    fn link(&self, i: &InterfaceSnapshot, seq: u32, pid: u32, flags: u16) -> Result<Vec<u8>, i32> {
        let loopback = i.id == u64::MAX;
        let mut b = vec![0, 0];
        b.extend_from_slice(&(if loopback { 772u16 } else { 1u16 }).to_ne_bytes());
        b.extend_from_slice(&Self::index(i)?.to_ne_bytes());
        let iflags = if loopback { 0x8u32 | if i.ipv4_enabled || i.ipv6_enabled { 0x10041 } else { 0 } } else { 0x1002 | if self.online { 0x10041 } else { 0 } };
        b.extend_from_slice(&iflags.to_ne_bytes());
        b.extend_from_slice(&0u32.to_ne_bytes());
        let name = if loopback { "lo".to_string() } else { format!("netstack{}", i.id - 1) };
        attribute(&mut b, 3, &[name.as_bytes(), &[0]].concat());
        let mtu = if loopback { 65536u32 } else { u32::from(crate::SOFTMAC_ETHERNET_MTU) };
        attribute(&mut b, 4, &mtu.to_ne_bytes());
        attribute(&mut b, 1, &if loopback { [0; 6] } else { self.mac });
        attribute(&mut b, 16, &[if loopback { 0 } else if self.online { 6 } else { 2 }]); // operstate
        Ok(message(LINK, flags, seq, pid, &b))
    }
    fn address(i: &InterfaceSnapshot, a: &Address, kind: u16, seq: u32, pid: u32, flags: u16) -> Result<Vec<u8>, i32> {
        let addr_flags = match a.state {
            AddressState::Tentative => 0x40,
            AddressState::Unavailable => 0x08,
            AddressState::Assigned => 0,
        } | if a.preferred_until == PreferredUntil::Deprecated { 0x20 } else { 0 };
        let scope = if a.address.is_loopback() { 254 } else {
            match a.address {
                IpAddr::V4(ip) if ip.is_link_local() => 253,
                IpAddr::V6(ip) if ip.is_unicast_link_local() => 253,
                _ => 0,
            }
        };
        let mut b = vec![if a.address.is_ipv4() { 2 } else { 10 }, a.prefix, addr_flags, scope];
        b.extend_from_slice(&Self::index(i)?.to_ne_bytes());
        let ip = match a.address { IpAddr::V4(a) => a.octets().to_vec(), IpAddr::V6(a) => a.octets().to_vec() };
        attribute(&mut b, 1, &ip);
        if a.address.is_ipv4() { attribute(&mut b, 2, &ip); }
        attribute(&mut b, 8, &u32::from(addr_flags).to_ne_bytes());
        Ok(message(kind, flags, seq, pid, &b))
    }
    fn request(&self, request: &[u8], pid: u32) -> Result<Vec<Vec<u8>>, i32> {
        let kind = u16_at(request, 4);
        let flags = u16_at(request, 6);
        if flags & 1 == 0 { return Err(libc::EINVAL); }
        if !matches!(kind, 18 | 22) { return Err(libc::EOPNOTSUPP); }
        if request.len() < 17 { return Err(libc::EINVAL); }
        let family = request[16];
        if !matches!(family, 0 | 2 | 10 | 17) { return Err(libc::EAFNOSUPPORT); }
        let dump = flags & 0x300 == 0x300;
        if !dump && (kind != 18 || request.len() != 32) { return Err(libc::EOPNOTSUPP); }
        if self.interfaces.iter().any(|i| i.incomplete) { return Err(libc::ENOBUFS); }
        let index = if dump { 0 } else { u32_at(request, 20) };
        let seq = u32_at(request, 8);
        let mut records = Vec::new();
        for interface in &self.interfaces {
            if index != 0 && index != Self::index(interface)? { continue; }
            if kind == 18 {
                records.push(self.link(interface, seq, pid, if dump { MULTI } else { 0 })?);
            } else {
                for address in &interface.addresses {
                    if family != 0 && family != if address.address.is_ipv4() { 2 } else { 10 } { continue; }
                    records.push(Self::address(interface, address, ADDRESS, seq, pid, MULTI)?);
                }
            }
        }
        if !dump && records.is_empty() { return Err(libc::ENODEV); }
        if dump { records.push(message(3, MULTI, seq, pid, &0i32.to_ne_bytes())); }
        else if flags & 4 != 0 { records.push(error(request, pid, 0)); }
        Ok(records)
    }
    fn changes(&self, old: &Self) -> Result<Vec<(u32, Vec<u8>)>, i32> {
        let mut records = Vec::new();
        for i in &self.interfaces {
            let before = old.interfaces.iter().find(|b| b.id == i.id);
            if before.is_none_or(|before| before.ipv4_enabled != i.ipv4_enabled || before.ipv6_enabled != i.ipv6_enabled)
                || self.online != old.online || self.mac != old.mac {
                records.push((1, self.link(i, 0, 0, 0)?));
            }
            if let Some(before) = before {
                for a in &before.addresses {
                    if !i.addresses.iter().any(|b| b.address == a.address && b.prefix == a.prefix) {
                        records.push((if a.address.is_ipv4() { 5 } else { 9 }, Self::address(before, a, ADDRESS + 1, 0, 0, 0)?));
                    }
                }
            }
            for a in &i.addresses {
                if before.is_none_or(|b| !b.addresses.contains(a)) {
                    records.push((if a.address.is_ipv4() { 5 } else { 9 }, Self::address(i, a, ADDRESS, 0, 0, 0)?));
                }
            }
        }
        for i in &old.interfaces {
            if !self.interfaces.iter().any(|new| new.id == i.id) {
                for a in &i.addresses {
                    records.push((if a.address.is_ipv4() { 5 } else { 9 }, Self::address(i, a, ADDRESS + 1, 0, 0, 0)?));
                }
                let mut deleted = old.link(i, 0, 0, 0)?;
                deleted[4..6].copy_from_slice(&17u16.to_ne_bytes());
                records.push((1, deleted));
            }
        }
        Ok(records)
    }
}
/// The supported mutation is administrative loopback state, not arbitrary
/// link creation. Credentials come from the kernel record, never nlmsg_pid.
fn loopback_change(request: &[u8], capable: bool) -> Result<bool, i32> {
    if !capable { return Err(libc::EPERM); }
    if request.len() < 32 { return Err(libc::EINVAL); }
    if u16_at(request, 6) & 1 == 0 || request[16] != 0 || request[17] != 0 {
        return Err(libc::EINVAL);
    }
    if u16_at(request, 6) & !5 != 0 || u32_at(request, 28) != libc::IFF_UP as u32 {
        return Err(libc::EOPNOTSUPP);
    }
    let index = u32_at(request, 20);
    if index > 1 { return Err(libc::ENODEV); }
    let mut named = false;
    let mut attrs = &request[32..];
    while !attrs.is_empty() {
        if attrs.len() < 4 { return Err(libc::EINVAL); }
        let len = u16_at(attrs, 0) as usize;
        if len < 4 || len > attrs.len() { return Err(libc::EINVAL); }
        if u16_at(attrs, 2) != 3 { return Err(libc::EOPNOTSUPP); }
        if &attrs[4..len] != b"lo\0" { return Err(libc::ENODEV); }
        if named { return Err(libc::EINVAL); }
        named = true;
        attrs = &attrs[align(len).min(attrs.len())..];
    }
    if index == 0 && !named { return Err(libc::ENODEV); }
    Ok(u32_at(request, 24) & libc::IFF_UP as u32 != 0)
}
struct Client {
    fd: OwnedFd,
    output: VecDeque<Vec<u8>>,
    bytes: usize,
}
impl Client {
    fn queue(&mut self, group: u32, data: Vec<u8>) -> Result<(), Errno> {
        if data.len() > MAX_MESSAGE || self.bytes + data.len() + 8 > MAX_BYTES || self.output.len() >= MAX_RECORDS {
            return Err(Errno::NOBUFS);
        }
        let mut record = group.to_le_bytes().to_vec();
        record.extend_from_slice(&0u32.to_le_bytes());
        record.extend_from_slice(&data);
        self.bytes += record.len();
        self.output.push_back(record);
        Ok(())
    }
    fn read(&mut self, view: &mut View, set_up: &mut impl FnMut(bool) -> Result<View, Errno>) -> Result<bool, Errno> {
        let mut buf = [0u8; MAX_MESSAGE + 40];
        let n = match rustix::io::read(&self.fd, &mut buf) {
            Ok(n) => n,
            Err(Errno::AGAIN) => return Ok(false),
            Err(e) => return Err(e),
        };
        if n < 40 || u32::from_le_bytes(buf[..4].try_into().unwrap()) != 1 { return Err(Errno::PROTO); }
        match u32::from_le_bytes(buf[4..8].try_into().unwrap()) {
            1 => {
                let pid = u32::from_le_bytes(buf[16..20].try_into().unwrap());
                let mut payload = &buf[40..n];
                let mut count = 0;
                while !payload.is_empty() {
                    if payload.len() < 16 { return Err(Errno::PROTO); }
                    let len = u32_at(payload, 0) as usize;
                    if len < 16 || len > payload.len() { return Err(Errno::PROTO); }
                    count += 1;
                    if count > 32 { return Err(Errno::NOBUFS); }
                    let request = &payload[..len];
                    if u16_at(request, 4) == LINK {
                        let result = loopback_change(request, u32_at(&buf, 32) != 0)
                            .and_then(|up| set_up(up).map_err(|error| error.raw_os_error()));
                        match result {
                            Ok(updated) => {
                                *view = updated;
                                if u16_at(request, 6) & 4 != 0 { self.queue(0, error(request, pid, 0))?; }
                            }
                            Err(errno) => self.queue(0, error(request, pid, errno))?,
                        }
                    } else { match view.request(request, pid) {
                        Ok(records) => for record in records { self.queue(0, record)?; },
                        Err(errno) => self.queue(0, error(request, pid, errno))?,
                    } }
                    payload = &payload[align(len).min(payload.len())..];
                }
            }
            _ => return Err(Errno::PROTO),
        }
        Ok(true)
    }
    fn flush(&mut self) -> Result<bool, Errno> {
        let mut progress = false;
        while let Some(record) = self.output.front() {
            match rustix::io::write(&self.fd, record) {
                Ok(n) if n == record.len() => {}
                Err(Errno::NOENT) if record[..4] != [0; 4] => {} // unsubscribed
                Err(Errno::AGAIN) => break,
                Ok(_) => return Err(Errno::IO),
                Err(e) => return Err(e),
            }
            self.bytes -= record.len();
            self.output.pop_front();
            progress = true;
        }
        Ok(progress)
    }
}
pub(crate) struct Adapter {
    registration: OwnedFd,
    clients: HashMap<i32, Client>,
    view: Option<View>,
}
impl Adapter {
    pub(crate) fn owns_token(token: u64) -> bool { token & !0xffff_ffff == TAG }
    pub(crate) fn new(registration: OwnedFd, poll: BorrowedFd<'_>) -> Result<Self, Errno> {
        epoll::add(poll, &registration, epoll::EventData::new_u64(TOKEN), epoll::EventFlags::IN)?;
        Ok(Self { registration, clients: HashMap::new(), view: None })
    }
    pub(crate) fn advance(&mut self, poll: BorrowedFd<'_>, events: &[libc::epoll_event], update: Option<View>, set_up: &mut impl FnMut(bool) -> Result<View, Errno>) -> Result<bool, Errno> {
        let mut progress = false;
        let mut ready: Vec<i32> = events.iter().filter_map(|e| {
            let token = e.u64;
            (Self::owns_token(token) && token != TOKEN).then_some(token as i32)
        }).collect();
        if events.iter().any(|e| e.u64 == TOKEN) {
            loop {
                let mut id = 0u64;
                // SAFETY: fixed CLAIM writes exactly one u64; returned FD is a fresh owner.
                let fd = unsafe { libc::ioctl(self.registration.as_raw_fd(), CLAIM, &mut id) };
                if fd < 0 {
                    let error = Errno::from_raw_os_error(std::io::Error::last_os_error().raw_os_error().unwrap());
                    if error == Errno::AGAIN { break; }
                    return Err(error);
                }
                let owned = unsafe { OwnedFd::from_raw_fd(fd) };
                epoll::add(poll, &owned, epoll::EventData::new_u64(TAG | fd as u64), epoll::EventFlags::IN)?;
                self.clients.insert(fd, Client { fd: owned, output: VecDeque::new(), bytes: 0 });
                ready.push(fd);
                progress = true;
            }
        }
        let old_view = self.view.clone();
        if let Some(view) = update { self.view = Some(view); }
        ready.sort_unstable(); ready.dedup();
        for fd in ready {
            let Some(client) = self.clients.get_mut(&fd) else { continue };
            let result = (|| {
                // Don't accumulate queries behind a blocked dump.
                progress |= client.flush()?;
                if client.output.is_empty() {
                    for _ in 0..32 {
                        if !client.read(self.view.as_mut().unwrap(), set_up)? { break; }
                        progress = true;
                        progress |= client.flush()?;
                        if !client.output.is_empty() { break; }
                    }
                }
                epoll::modify(poll, &client.fd, epoll::EventData::new_u64(TAG | fd as u64),
                    if client.output.is_empty() { epoll::EventFlags::IN } else { epoll::EventFlags::OUT })
            })();
            if result.is_err() { self.clients.remove(&fd); progress = true; }
        }
        if let (Some(old), Some(view)) = (&old_view, &self.view)
            && old != view
        {
            if old.interfaces.iter().any(|i| i.incomplete) || view.interfaces.iter().any(|i| i.incomplete) {
                // A lost observation is not a complete event stream.
                self.clients.clear();
            } else {
                let changes = view.changes(old).map_err(Errno::from_raw_os_error)?;
                for (group, data) in changes {
                    let mut record = group.to_le_bytes().to_vec();
                    record.extend_from_slice(&0u32.to_le_bytes());
                    record.extend_from_slice(&data);
                    let n = rustix::io::write(&self.registration, &record)?;
                    if n != record.len() { return Err(Errno::IO); }
                    progress = true;
                }
            }
        }
        Ok(progress)
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    fn view() -> View {
        View::new(vec![InterfaceSnapshot {
            id: 1, ipv4_enabled: true, addresses: vec![Address {
                address: "192.0.2.7".parse().unwrap(), prefix: 24,
                state: AddressState::Assigned, valid_until: None,
                preferred_until: PreferredUntil::Preferred(None),
            }], ..Default::default()
        }], true, [2,0,0,0,0,1])
    }
    #[test]
    fn dumps_use_real_addresses_and_authenticated_port_id() {
        let v = view();
        let request = message(22, 0x301, 93, 999, &[0]);
        let records = v.request(&request[..17], 42).unwrap();
        assert_eq!(records.len(), 2);
        assert_eq!(u32_at(&records[0], 12), 42);
        assert_eq!(u32_at(&records[0], 8), 93);
        assert!(records[0].windows(4).any(|v| v == [192,0,2,7]));
        assert_eq!(u16_at(&records[1], 4), 3);
        let mut incomplete = v.clone(); incomplete.interfaces[0].incomplete = true;
        assert_eq!(incomplete.request(&request[..17], 42), Err(libc::ENOBUFS));
    }
    #[test]
    fn address_removal_and_link_loss_emit_scoped_events() {
        let old = view(); let mut new = old.clone();
        new.online = false; new.interfaces[0].addresses.clear();
        let changes = new.changes(&old).unwrap();
        assert_eq!(changes.iter().map(|(group,m)| (*group,u16_at(m,4))).collect::<Vec<_>>(), vec![(1,16),(5,21)]);
    }
    #[test]
    fn unsupported_mutations_are_not_successful_empty_dumps() {
        assert_eq!(view().request(&message(20,0x301,1,1,&[0]),1), Err(libc::EOPNOTSUPP));
    }
    #[test]
    fn loopback_mutation_requires_authenticated_authority_and_exact_scope() {
        let mut body = [0u8; 16];
        body[4..8].copy_from_slice(&1u32.to_ne_bytes());
        body[8..12].copy_from_slice(&1u32.to_ne_bytes());
        body[12..16].copy_from_slice(&1u32.to_ne_bytes());
        let mut request = message(LINK, 5, 1, 0, &body);
        assert_eq!(loopback_change(&request, false), Err(libc::EPERM));
        assert_eq!(loopback_change(&request, true), Ok(true));
        request[24..28].copy_from_slice(&0u32.to_ne_bytes());
        assert_eq!(loopback_change(&request, true), Ok(false));
        request[20..24].copy_from_slice(&2u32.to_ne_bytes());
        assert_eq!(loopback_change(&request, true), Err(libc::ENODEV));
        request[20..24].copy_from_slice(&1u32.to_ne_bytes());
        request[28..32].copy_from_slice(&9u32.to_ne_bytes());
        assert_eq!(loopback_change(&request, true), Err(libc::EOPNOTSUPP));
        assert_eq!(loopback_change(&request[..31], true), Err(libc::EINVAL));
    }

    #[test]
    fn loopback_link_events_follow_core_enablement_not_ethernet() {
        let old = View::new(vec![InterfaceSnapshot { id: u64::MAX, ..Default::default() }],
            false, [0; 6]);
        let mut new = old.clone();
        new.interfaces[0].ipv4_enabled = true;
        let changes = new.changes(&old).unwrap();
        assert_eq!(changes.len(), 1);
        assert_eq!(u32_at(&old.link(&old.interfaces[0], 0, 0, 0).unwrap(), 24) & 1, 0);
        assert_eq!(u32_at(&changes[0].1, 24) & 1, 1);
        assert_eq!(changes[0].0, 1);
    }

}
