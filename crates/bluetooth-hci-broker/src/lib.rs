use std::collections::BTreeMap;
use std::fs::OpenOptions;
use std::io;
use std::io::Write;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::os::unix::fs::OpenOptionsExt;
use std::path::Path;
use std::thread;
use std::time::{Duration, Instant};

pub const MAX_HCI_PACKET: usize = 260;
pub const MAX_OUTBOUND_HCI_PACKET: usize = 4096;
pub const H4_EVENT: u8 = 0x04;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum OutboundKind {
    Command,
    Acl,
    Sco,
    Iso,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum InboundKind {
    Event,
    Acl,
    Sco,
    Iso,
}

pub fn validate_outbound(kind: OutboundKind, packet: &[u8]) -> io::Result<()> {
    if packet.len() > MAX_OUTBOUND_HCI_PACKET {
        return Err(invalid("outbound HCI packet exceeds boundary"));
    }
    let (header, payload_len) = match kind {
        OutboundKind::Command => (3, packet.get(2).copied().map(usize::from)),
        OutboundKind::Acl => (
            4,
            packet
                .get(2..4)
                .map(|bytes| usize::from(u16::from_le_bytes([bytes[0], bytes[1]]))),
        ),
        OutboundKind::Sco => (3, packet.get(2).copied().map(usize::from)),
        OutboundKind::Iso => (
            4,
            packet
                .get(2..4)
                .map(|bytes| usize::from(u16::from_le_bytes([bytes[0], bytes[1]]) & 0x3fff)),
        ),
    };
    let payload_len = payload_len.ok_or_else(|| invalid("short outbound HCI packet"))?;
    if packet.len() != header + payload_len {
        return Err(invalid("malformed outbound HCI packet length"));
    }
    Ok(())
}

pub fn validate_inbound(kind: InboundKind, packet: &[u8]) -> io::Result<()> {
    match kind {
        InboundKind::Event => decode_event(packet).map(|_| ()),
        InboundKind::Acl => validate_outbound(OutboundKind::Acl, packet),
        InboundKind::Sco => validate_outbound(OutboundKind::Sco, packet),
        InboundKind::Iso => validate_outbound(OutboundKind::Iso, packet),
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Event<'a> {
    pub code: u8,
    pub parameters: &'a [u8],
}

pub fn decode_event(packet: &[u8]) -> io::Result<Event<'_>> {
    if packet.len() > MAX_HCI_PACKET {
        return Err(invalid("HCI packet exceeds boundary"));
    }
    if packet.len() < 3 || packet[0] != H4_EVENT {
        return Err(invalid("not an HCI event packet"));
    }
    let parameter_len = usize::from(packet[2]);
    if packet.len() != parameter_len + 3 {
        return Err(invalid("malformed HCI event length"));
    }
    Ok(Event {
        code: packet[1],
        parameters: &packet[3..],
    })
}

#[derive(Debug, Default)]
pub struct CommandGate {
    credits: u8,
    pending: Option<u16>,
}

impl CommandGate {
    pub fn new() -> Self {
        Self {
            credits: 1,
            pending: None,
        }
    }

    pub fn begin(&mut self, opcode: u16) -> io::Result<()> {
        if self.credits == 0 || self.pending.is_some() {
            return Err(io::Error::new(
                io::ErrorKind::WouldBlock,
                "no HCI command credit",
            ));
        }
        self.credits -= 1;
        self.pending = Some(opcode);
        Ok(())
    }

    pub fn observe(&mut self, event: &Event<'_>) -> io::Result<Option<u8>> {
        let (credits, opcode, status) = match event.code {
            0x0e if event.parameters.len() >= 4 => (
                event.parameters[0],
                u16::from_le_bytes([event.parameters[1], event.parameters[2]]),
                event.parameters[3],
            ),
            0x0f if event.parameters.len() == 4 => (
                event.parameters[1],
                u16::from_le_bytes([event.parameters[2], event.parameters[3]]),
                event.parameters[0],
            ),
            0x0e | 0x0f => return Err(invalid("malformed HCI command response")),
            _ => return Ok(None),
        };
        self.credits = credits;
        if self.pending == Some(opcode) {
            self.pending = None;
            return Ok(Some(status));
        }
        Ok(None)
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Peer {
    pub address_type: &'static str,
    pub address: [u8; 6],
    pub name: Option<String>,
    pub best_rssi: Option<i8>,
    pub sightings: u32,
}

#[derive(Debug, Default)]
pub struct Peers(BTreeMap<(u8, [u8; 6]), Peer>);

impl Peers {
    pub fn values(&self) -> impl Iterator<Item = &Peer> {
        self.0.values()
    }
    pub fn len(&self) -> usize {
        self.0.len()
    }
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    fn record(&mut self, kind: u8, address: [u8; 6], data: &[u8], rssi: Option<i8>) {
        let address_type = match kind {
            0 => "bredr",
            1 => "le-public",
            _ => "le-random",
        };
        let peer = self.0.entry((kind, address)).or_insert(Peer {
            address_type,
            address,
            name: None,
            best_rssi: None,
            sightings: 0,
        });
        peer.sightings += 1;
        if let Some(rssi) = rssi {
            peer.best_rssi = Some(peer.best_rssi.map_or(rssi, |old| old.max(rssi)));
        }
        if let Some(name) = advertised_name(data) {
            peer.name = Some(name);
        }
    }

    pub fn observe(&mut self, event: &Event<'_>) -> io::Result<bool> {
        match event.code {
            0x01 => return Ok(true),
            0x02 => self.observe_inquiry(event.parameters, false)?,
            0x22 => self.observe_inquiry(event.parameters, true)?,
            0x2f => self.observe_extended_inquiry(event.parameters)?,
            0x3e => self.observe_le(event.parameters)?,
            _ => {}
        }
        Ok(false)
    }

    fn observe_inquiry(&mut self, bytes: &[u8], with_rssi: bool) -> io::Result<()> {
        let (&count, rest) = bytes
            .split_first()
            .ok_or_else(|| invalid("empty inquiry result"))?;
        // Both response forms are 14 bytes. The non-RSSI form has two
        // reserved bytes; the RSSI form replaces one with the signal value.
        let width = 14;
        if rest.len() != usize::from(count) * width {
            return Err(invalid("malformed inquiry result"));
        }
        for entry in rest.chunks_exact(width) {
            let address = entry[0..6].try_into().unwrap();
            self.record(0, address, &[], with_rssi.then(|| entry[13] as i8));
        }
        Ok(())
    }

    fn observe_extended_inquiry(&mut self, bytes: &[u8]) -> io::Result<()> {
        let (&count, rest) = bytes
            .split_first()
            .ok_or_else(|| invalid("empty extended inquiry result"))?;
        if rest.len() != usize::from(count) * 254 {
            return Err(invalid("malformed extended inquiry result"));
        }
        for entry in rest.chunks_exact(254) {
            let address = entry[0..6].try_into().unwrap();
            self.record(0, address, &entry[14..254], Some(entry[13] as i8));
        }
        Ok(())
    }

    fn observe_le(&mut self, bytes: &[u8]) -> io::Result<()> {
        if bytes.first() != Some(&0x02) {
            return Ok(());
        }
        let count = *bytes
            .get(1)
            .ok_or_else(|| invalid("short LE advertising report"))?;
        let mut rest = &bytes[2..];
        for _ in 0..count {
            if rest.len() < 10 {
                return Err(invalid("short LE advertising entry"));
            }
            let kind = match rest[1] {
                0 => 1,
                1 => 2,
                _ => return Err(invalid("invalid LE address type")),
            };
            let address = rest[2..8].try_into().unwrap();
            let data_len = usize::from(rest[8]);
            if rest.len() < 10 + data_len {
                return Err(invalid("truncated LE advertising data"));
            }
            self.record(
                kind,
                address,
                &rest[9..9 + data_len],
                Some(rest[9 + data_len] as i8),
            );
            rest = &rest[10 + data_len..];
        }
        if !rest.is_empty() {
            return Err(invalid("trailing LE advertising bytes"));
        }
        Ok(())
    }
}

fn advertised_name(mut data: &[u8]) -> Option<String> {
    while let Some((&len, tail)) = data.split_first() {
        if len == 0 {
            break;
        }
        if tail.len() < usize::from(len) {
            return None;
        }
        let field = &tail[..usize::from(len)];
        if matches!(field[0], 0x08 | 0x09) {
            return std::str::from_utf8(&field[1..]).ok().map(ToOwned::to_owned);
        }
        data = &tail[usize::from(len)..];
    }
    None
}

fn invalid(message: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}

#[cfg(target_os = "linux")]
mod physical {
    use super::*;
    use std::ffi::{c_int, c_short, c_ulong, c_void};

    const AF_BLUETOOTH: c_int = 31;
    const BTPROTO_HCI: c_int = 1;
    const SOCK_RAW: c_int = 3;
    const SOCK_NONBLOCK: c_int = 0x800;
    const SOCK_CLOEXEC: c_int = 0x80000;
    const HCI_CHANNEL_USER: u16 = 1;
    const HCI_UP: u32 = 1;
    const HCIDEVUP: c_ulong = 0x4004_48c9;
    const HCIDEVDOWN: c_ulong = 0x4004_48ca;
    const HCIGETDEVINFO: c_ulong = 0x8004_48d3;
    const POLLIN: c_short = 0x001;
    const POLLERR: c_short = 0x008;
    const POLLHUP: c_short = 0x010;
    const POLLNVAL: c_short = 0x020;

    unsafe extern "C" {
        fn socket(domain: c_int, kind: c_int, protocol: c_int) -> c_int;
        fn bind(fd: c_int, address: *const c_void, length: u32) -> c_int;
        fn poll(fds: *mut PollFd, count: c_ulong, timeout: c_int) -> c_int;
        fn read(fd: c_int, buffer: *mut c_void, length: usize) -> isize;
        fn write(fd: c_int, buffer: *const c_void, length: usize) -> isize;
        fn ioctl(fd: c_int, request: c_ulong, ...) -> c_int;
    }

    #[repr(C)]
    struct SockAddrHci {
        family: u16,
        device: u16,
        channel: u16,
    }

    #[repr(C)]
    struct PollFd {
        fd: c_int,
        events: c_short,
        revents: c_short,
    }

    #[repr(C)]
    #[derive(Default)]
    struct HciDevStats {
        values: [u32; 10],
    }

    #[repr(C)]
    #[derive(Default)]
    struct HciDevInfo {
        device: u16,
        name: [u8; 8],
        address: [u8; 6],
        flags: u32,
        kind: u8,
        features: [u8; 8],
        packet_type: u32,
        link_policy: u32,
        link_mode: u32,
        stats: HciDevStats,
    }

    fn raw_socket(nonblock: bool) -> io::Result<OwnedFd> {
        let mut kind = SOCK_RAW | SOCK_CLOEXEC;
        if nonblock {
            kind |= SOCK_NONBLOCK;
        }
        // SAFETY: socket returns a new descriptor and all arguments are Linux constants.
        let raw = unsafe { socket(AF_BLUETOOTH, kind, BTPROTO_HCI) };
        if raw < 0 {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: ownership of the newly returned descriptor transfers once.
        Ok(unsafe { OwnedFd::from_raw_fd(raw) })
    }

    fn snapshot(control: &OwnedFd, device: u16) -> io::Result<u32> {
        let mut info = HciDevInfo {
            device,
            ..HciDevInfo::default()
        };
        // SAFETY: HCIGETDEVINFO expects a writable hci_dev_info pointer.
        if unsafe { ioctl(control.as_raw_fd(), HCIGETDEVINFO, &mut info) } < 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(info.flags)
    }

    fn set_up(control: &OwnedFd, device: u16, up: bool) -> io::Result<()> {
        let request = if up { HCIDEVUP } else { HCIDEVDOWN };
        // SAFETY: HCIDEVUP and HCIDEVDOWN take the controller id by value.
        if unsafe { ioctl(control.as_raw_fd(), request, i32::from(device)) } < 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(())
    }

    fn restore_exact(control: &OwnedFd, device: u16, wanted: u32) -> io::Result<()> {
        if (snapshot(control, device)? & HCI_UP != 0) != (wanted & HCI_UP != 0) {
            set_up(control, device, wanted & HCI_UP != 0)?;
        }
        for _ in 0..50 {
            if snapshot(control, device)? == wanted {
                return Ok(());
            }
            thread::sleep(Duration::from_millis(100));
        }
        Err(io::Error::other(format!(
            "controller flags did not restore exactly: initial={wanted:#010x}, final={:#010x}",
            snapshot(control, device)?
        )))
    }

    fn open_user_channel(device: u16) -> io::Result<OwnedFd> {
        let fd = raw_socket(true)?;
        let address = SockAddrHci {
            family: AF_BLUETOOTH as u16,
            device,
            channel: HCI_CHANNEL_USER,
        };
        // SAFETY: address points to a valid sockaddr_hci for the supplied size.
        if unsafe {
            bind(
                fd.as_raw_fd(),
                (&address as *const SockAddrHci).cast(),
                std::mem::size_of::<SockAddrHci>() as u32,
            )
        } < 0
        {
            return Err(io::Error::last_os_error());
        }
        Ok(fd)
    }

    /// Exclusive, native-only Linux HCI user-channel ownership with exact flag restoration.
    pub struct PhysicalHci {
        control: OwnedFd,
        channel: Option<OwnedFd>,
        device: u16,
        initial_flags: u32,
        restored: bool,
    }

    impl PhysicalHci {
        pub fn prepare(device: u16, state_path: &Path) -> io::Result<Self> {
            let control = raw_socket(false)?;
            let initial_flags = snapshot(&control, device)?;
            let mut state = OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(0o600)
                .open(state_path)?;
            writeln!(state, "device={device}\nflags={initial_flags:08x}")?;
            state.sync_all()?;
            if initial_flags & HCI_UP != 0 {
                set_up(&control, device, false)?;
                if snapshot(&control, device)? & HCI_UP != 0 {
                    return Err(io::Error::other("controller remained up after HCIDEVDOWN"));
                }
            }
            let channel = open_user_channel(device).map_err(|error| {
                io::Error::new(
                    error.kind(),
                    format!("cannot open exclusive hci{device} user channel: {error}"),
                )
            })?;
            Ok(Self {
                control,
                channel: Some(channel),
                device,
                initial_flags,
                restored: false,
            })
        }

        pub fn initial_flags(&self) -> u32 {
            self.initial_flags
        }

        pub fn initialize_for_discovery(&self) -> io::Result<()> {
            self.command(0x0c03, &[])?;
            self.command(0x0c01, &[0xff, 0xff, 0xfb, 0xff, 0x07, 0xf8, 0xbf, 0x3d])?;
            self.command(0x2001, &[0x1f, 0, 0, 0, 0, 0, 0, 0])
        }

        pub fn send(&self, kind: OutboundKind, packet: &[u8]) -> io::Result<()> {
            validate_outbound(kind, packet)?;
            let h4 = match kind {
                OutboundKind::Command => 0x01,
                OutboundKind::Acl => 0x02,
                OutboundKind::Sco => 0x03,
                OutboundKind::Iso => 0x05,
            };
            let mut framed = Vec::with_capacity(packet.len() + 1);
            framed.push(h4);
            framed.extend_from_slice(packet);
            self.write_raw(&framed)
        }

        pub fn receive(&self, deadline: Instant) -> io::Result<(InboundKind, Vec<u8>)> {
            let now = Instant::now();
            if now >= deadline {
                return Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    "HCI deadline expired",
                ));
            }
            let timeout = (deadline - now).as_millis().clamp(1, i32::MAX as u128) as i32;
            let fd = self
                .channel
                .as_ref()
                .ok_or_else(|| io::Error::other("HCI closed"))?;
            let mut pollfd = PollFd {
                fd: fd.as_raw_fd(),
                events: POLLIN,
                revents: 0,
            };
            // SAFETY: pollfd points to one initialized entry for the call.
            let ready = unsafe { poll(&mut pollfd, 1, timeout) };
            if ready < 0 {
                return Err(io::Error::last_os_error());
            }
            if ready == 0 {
                return Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    "HCI deadline expired",
                ));
            }
            if pollfd.revents & (POLLERR | POLLHUP | POLLNVAL) != 0 {
                return Err(io::Error::new(
                    io::ErrorKind::BrokenPipe,
                    "HCI user channel closed",
                ));
            }
            let mut framed = vec![0; MAX_OUTBOUND_HCI_PACKET + 1];
            // SAFETY: framed is writable and fd is owned for the call.
            let length = unsafe { read(fd.as_raw_fd(), framed.as_mut_ptr().cast(), framed.len()) };
            if length <= 0 {
                return Err(if length == 0 {
                    io::Error::new(io::ErrorKind::UnexpectedEof, "HCI user channel EOF")
                } else {
                    io::Error::last_os_error()
                });
            }
            framed.truncate(length as usize);
            let (&h4, packet) = framed
                .split_first()
                .ok_or_else(|| invalid("empty HCI packet"))?;
            let (kind, bytes) = match h4 {
                H4_EVENT => {
                    validate_inbound(InboundKind::Event, &framed)?;
                    (InboundKind::Event, packet.to_vec())
                }
                0x02 => (InboundKind::Acl, packet.to_vec()),
                0x03 => (InboundKind::Sco, packet.to_vec()),
                0x05 => (InboundKind::Iso, packet.to_vec()),
                _ => return Err(invalid("unsupported inbound H4 packet type")),
            };
            if kind != InboundKind::Event {
                validate_inbound(kind, &bytes)?;
            }
            Ok((kind, bytes))
        }

        pub fn cleanup_scan(&self) {
            let _ = self.write_raw(&[0x01, 0x0c, 0x20, 0x02, 0x00, 0x01]);
        }

        pub fn restore(&mut self) -> io::Result<()> {
            self.cleanup_scan();
            self.channel.take();
            restore_exact(&self.control, self.device, self.initial_flags)?;
            self.restored = true;
            Ok(())
        }

        fn write_raw(&self, packet: &[u8]) -> io::Result<()> {
            let fd = self
                .channel
                .as_ref()
                .ok_or_else(|| io::Error::other("HCI closed"))?;
            // SAFETY: packet is readable and fd is owned for the call.
            let written = unsafe { write(fd.as_raw_fd(), packet.as_ptr().cast(), packet.len()) };
            if written < 0 {
                return Err(io::Error::last_os_error());
            }
            if written as usize != packet.len() {
                return Err(io::Error::new(io::ErrorKind::WriteZero, "short HCI write"));
            }
            Ok(())
        }

        fn command(&self, opcode: u16, parameters: &[u8]) -> io::Result<()> {
            let mut packet = vec![
                0x01,
                opcode as u8,
                (opcode >> 8) as u8,
                parameters.len() as u8,
            ];
            packet.extend_from_slice(parameters);
            self.write_raw(&packet)?;
            let deadline = Instant::now() + Duration::from_secs(3);
            loop {
                let (kind, bytes) = self.receive(deadline)?;
                if kind != InboundKind::Event {
                    continue;
                }
                let event = decode_event(&bytes)?;
                let (completed, status) = match event.code {
                    0x0e if event.parameters.len() >= 4 => (
                        u16::from_le_bytes([event.parameters[1], event.parameters[2]]),
                        event.parameters[3],
                    ),
                    0x0f if event.parameters.len() == 4 => (
                        u16::from_le_bytes([event.parameters[2], event.parameters[3]]),
                        event.parameters[0],
                    ),
                    _ => continue,
                };
                if completed == opcode {
                    return if status == 0 {
                        Ok(())
                    } else {
                        Err(io::Error::other(format!(
                            "HCI initialization opcode {opcode:#06x} failed: {status:#04x}"
                        )))
                    };
                }
            }
        }
    }

    impl Drop for PhysicalHci {
        fn drop(&mut self) {
            if !self.restored {
                self.cleanup_scan();
                self.channel.take();
                let _ = restore_exact(&self.control, self.device, self.initial_flags);
            }
        }
    }

    pub fn probe_exclusive_user_channel(device: u16) -> io::Result<()> {
        let control = raw_socket(false)?;
        let flags = snapshot(&control, device)?;
        if flags & HCI_UP != 0 {
            set_up(&control, device, false)?;
        }
        let result = open_user_channel(device).map(drop);
        let restore = restore_exact(&control, device, flags);
        result.and(restore)
    }

    pub fn restore_saved_controller_state(path: &Path) -> io::Result<()> {
        let text = std::fs::read_to_string(path)?;
        let mut lines = text.lines();
        let device = lines
            .next()
            .and_then(|line| line.strip_prefix("device="))
            .ok_or_else(|| invalid("invalid saved controller state"))?
            .parse::<u16>()
            .map_err(|_| invalid("invalid saved controller device"))?;
        let flags = u32::from_str_radix(
            lines
                .next()
                .and_then(|line| line.strip_prefix("flags="))
                .ok_or_else(|| invalid("invalid saved controller state"))?,
            16,
        )
        .map_err(|_| invalid("invalid saved controller flags"))?;
        if lines.next().is_some() {
            return Err(invalid("trailing saved controller state"));
        }
        let control = raw_socket(false)?;
        restore_exact(&control, device, flags)
    }
}

#[cfg(target_os = "linux")]
pub use physical::{PhysicalHci, probe_exclusive_user_channel, restore_saved_controller_state};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CleanupAction {
    DisableLeScan,
    CancelInquiry,
}

#[derive(Debug, Default)]
pub struct ActiveProcedures(u8);

impl ActiveProcedures {
    const LE_SCAN: u8 = 1;
    const INQUIRY: u8 = 2;

    pub fn start_le_scan(&mut self) {
        self.0 |= Self::LE_SCAN;
    }

    pub fn finish_le_scan(&mut self) {
        self.0 &= !Self::LE_SCAN;
    }

    pub fn start_inquiry(&mut self) {
        self.0 |= Self::INQUIRY;
    }

    pub fn finish_inquiry(&mut self) {
        self.0 &= !Self::INQUIRY;
    }

    pub fn cleanup_actions(&self) -> impl Iterator<Item = CleanupAction> {
        [
            (self.0 & Self::LE_SCAN != 0).then_some(CleanupAction::DisableLeScan),
            (self.0 & Self::INQUIRY != 0).then_some(CleanupAction::CancelInquiry),
        ]
        .into_iter()
        .flatten()
    }

    pub fn clear(&mut self) {
        self.0 = 0;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn event(code: u8, parameters: &[u8]) -> Vec<u8> {
        let mut out = vec![H4_EVENT, code, parameters.len() as u8];
        out.extend_from_slice(parameters);
        out
    }

    #[test]
    fn rejects_malformed_and_oversize_packets() {
        assert!(decode_event(&[4, 1, 2, 0]).is_err());
        assert!(decode_event(&vec![0; MAX_HCI_PACKET + 1]).is_err());
        assert!(decode_event(&[2, 0, 0]).is_err());
    }

    #[test]
    fn validates_bounded_outbound_transport_frames() {
        validate_outbound(OutboundKind::Command, &[0x0c, 0x20, 1, 1]).unwrap();
        validate_outbound(OutboundKind::Acl, &[1, 0, 2, 0, 0xaa, 0xbb]).unwrap();
        validate_outbound(OutboundKind::Sco, &[1, 0, 1, 0xaa]).unwrap();
        validate_outbound(OutboundKind::Iso, &[1, 0, 2, 0, 0xaa, 0xbb]).unwrap();

        assert!(validate_outbound(OutboundKind::Command, &[0x0c, 0x20, 2, 1]).is_err());
        assert!(validate_outbound(OutboundKind::Acl, &[1, 0, 3, 0, 0xaa]).is_err());
        assert!(
            validate_outbound(OutboundKind::Iso, &vec![0; MAX_OUTBOUND_HCI_PACKET + 1]).is_err()
        );
    }

    #[test]
    fn validates_all_inbound_transport_frames() {
        validate_inbound(InboundKind::Event, &[H4_EVENT, 0x0e, 4, 1, 3, 0x0c, 0]).unwrap();
        validate_inbound(InboundKind::Acl, &[1, 0, 2, 0, 0xaa, 0xbb]).unwrap();
        validate_inbound(InboundKind::Sco, &[1, 0, 1, 0xaa]).unwrap();
        validate_inbound(InboundKind::Iso, &[1, 0, 2, 0, 0xaa, 0xbb]).unwrap();
        assert!(validate_inbound(InboundKind::Event, &[H4_EVENT, 0x0e, 4]).is_err());
    }

    #[test]
    fn command_gate_tracks_credits_and_matching_completion() {
        let mut gate = CommandGate::new();
        gate.begin(0x200c).unwrap();
        assert_eq!(
            gate.begin(0x0401).unwrap_err().kind(),
            io::ErrorKind::WouldBlock
        );
        let packet = event(0x0e, &[2, 0x0c, 0x20, 0]);
        assert_eq!(
            gate.observe(&decode_event(&packet).unwrap()).unwrap(),
            Some(0)
        );
        gate.begin(0x0401).unwrap();
    }

    #[test]
    fn unrelated_response_does_not_complete_pending_command() {
        let mut gate = CommandGate::new();
        gate.begin(0x0401).unwrap();

        let unrelated = event(0x0e, &[1, 0x03, 0x0c, 0]);
        assert_eq!(
            gate.observe(&decode_event(&unrelated).unwrap()).unwrap(),
            None
        );
        assert_eq!(
            gate.begin(0x200c).unwrap_err().kind(),
            io::ErrorKind::WouldBlock
        );

        let matching_status = event(0x0f, &[0x0c, 2, 0x01, 0x04]);
        assert_eq!(
            gate.observe(&decode_event(&matching_status).unwrap())
                .unwrap(),
            Some(0x0c)
        );
        gate.begin(0x200c).unwrap();
    }

    #[test]
    fn deduplicates_le_reports_and_keeps_best_signal_and_name() {
        let address = [1, 2, 3, 4, 5, 6];
        let mut peers = Peers::default();
        for rssi in [-70i8, -45] {
            let mut parameters = vec![0x02, 1, 0, 0];
            parameters.extend_from_slice(&address);
            parameters.extend_from_slice(&[5, 4, 0x09, b't', b'a', b'g', rssi as u8]);
            peers
                .observe(&decode_event(&event(0x3e, &parameters)).unwrap())
                .unwrap();
        }
        let peer = peers.values().next().unwrap();
        assert_eq!(peer.sightings, 2);
        assert_eq!(peer.best_rssi, Some(-45));
        assert_eq!(peer.name.as_deref(), Some("tag"));
    }

    #[test]
    fn rejects_truncated_advertising_and_inquiry() {
        let mut peers = Peers::default();
        assert!(
            peers
                .observe(&decode_event(&event(0x3e, &[2, 1, 0])).unwrap())
                .is_err()
        );
        assert!(
            peers
                .observe(&decode_event(&event(0x22, &[1, 0])).unwrap())
                .is_err()
        );
    }

    #[test]
    fn decodes_sapphire_rssi_inquiry_fixture_shape() {
        // Ported byte-for-byte in shape from Sapphire's pinned
        // bredr_discovery_manager_test.cc kRSSIInquiryResult fixture.
        let packet = event(
            0x22,
            &[
                1, // response count
                2, 0, 0, 0, 0, 0, // address
                0, // page scan repetition mode
                0, // reserved
                0, 0x1f, 0, // class of device
                0, 0,    // clock offset
                0xec, // -20 dBm
            ],
        );
        let mut peers = Peers::default();
        peers.observe(&decode_event(&packet).unwrap()).unwrap();
        let peer = peers.values().next().unwrap();
        assert_eq!(peer.address, [2, 0, 0, 0, 0, 0]);
        assert_eq!(peer.best_rssi, Some(-20));
    }

    #[test]
    fn cleanup_plan_cancels_only_procedures_that_may_be_active() {
        let mut active = ActiveProcedures::default();
        active.start_le_scan();
        active.start_inquiry();
        assert_eq!(
            active.cleanup_actions().collect::<Vec<_>>(),
            vec![CleanupAction::DisableLeScan, CleanupAction::CancelInquiry]
        );
        active.finish_le_scan();
        assert_eq!(
            active.cleanup_actions().collect::<Vec<_>>(),
            vec![CleanupAction::CancelInquiry]
        );
        active.clear();
        assert!(active.cleanup_actions().next().is_none());
    }
}
