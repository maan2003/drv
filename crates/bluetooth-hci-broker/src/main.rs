use bluetooth_hci_broker::{
    ActiveProcedures, CleanupAction, CommandGate, MAX_HCI_PACKET, Peers, decode_event,
};
use std::env;
use std::fs::OpenOptions;
use std::io::{self, Write};
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

#[cfg(not(target_os = "linux"))]
compile_error!("the temporary HCI user-channel broker is Linux-only");

mod linux {
    use std::ffi::{c_int, c_short, c_ulong, c_void};

    pub const SOCK_RAW: c_int = 3;
    pub const SOCK_NONBLOCK: c_int = 0x800;
    pub const SOCK_CLOEXEC: c_int = 0x80000;
    pub const POLLIN: c_short = 0x001;
    pub const POLLERR: c_short = 0x008;
    pub const POLLHUP: c_short = 0x010;
    pub const POLLNVAL: c_short = 0x020;

    #[repr(C)]
    pub struct PollFd {
        pub fd: c_int,
        pub events: c_short,
        pub revents: c_short,
    }

    unsafe extern "C" {
        pub fn socket(domain: c_int, kind: c_int, protocol: c_int) -> c_int;
        pub fn bind(fd: c_int, address: *const c_void, length: u32) -> c_int;
        pub fn poll(fds: *mut PollFd, count: c_ulong, timeout: c_int) -> c_int;
        pub fn read(fd: c_int, buffer: *mut c_void, length: usize) -> isize;
        pub fn write(fd: c_int, buffer: *const c_void, length: usize) -> isize;
        pub fn ioctl(fd: c_int, request: c_ulong, ...) -> c_int;
    }
}

const AF_BLUETOOTH: i32 = 31;
const BTPROTO_HCI: i32 = 1;
const HCI_CHANNEL_USER: u16 = 1;
const HCI_UP: u32 = 1;
const HCIDEVUP: std::ffi::c_ulong = 0x4004_48c9;
const HCIDEVDOWN: std::ffi::c_ulong = 0x4004_48ca;
const HCIGETDEVINFO: std::ffi::c_ulong = 0x8004_48d3;

const RESET: u16 = 0x0c03;
const SET_EVENT_MASK: u16 = 0x0c01;
const LE_SET_EVENT_MASK: u16 = 0x2001;
const LE_SET_SCAN_PARAMETERS: u16 = 0x200b;
const LE_SET_SCAN_ENABLE: u16 = 0x200c;
const INQUIRY: u16 = 0x0401;
const INQUIRY_CANCEL: u16 = 0x0402;

#[repr(C)]
struct SockAddrHci {
    family: u16,
    device: u16,
    channel: u16,
}

#[repr(C)]
#[derive(Default)]
struct HciDevStats {
    err_rx: u32,
    err_tx: u32,
    cmd_tx: u32,
    evt_rx: u32,
    acl_tx: u32,
    acl_rx: u32,
    sco_tx: u32,
    sco_rx: u32,
    byte_rx: u32,
    byte_tx: u32,
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

#[derive(Clone, Copy)]
struct ControllerSnapshot {
    device: u16,
    flags: u32,
}

struct ControllerControl(OwnedFd);

impl ControllerControl {
    fn open() -> io::Result<Self> {
        // SAFETY: socket returns a new descriptor; arguments are Linux HCI constants.
        let raw = unsafe {
            linux::socket(
                AF_BLUETOOTH,
                linux::SOCK_RAW | linux::SOCK_CLOEXEC,
                BTPROTO_HCI,
            )
        };
        if raw < 0 {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: ownership of the newly created descriptor transfers exactly once.
        Ok(Self(unsafe { OwnedFd::from_raw_fd(raw) }))
    }

    fn snapshot(&self, device: u16) -> io::Result<ControllerSnapshot> {
        let mut info = HciDevInfo {
            device,
            ..HciDevInfo::default()
        };
        // SAFETY: HCIGETDEVINFO expects a writable hci_dev_info pointer.
        let result = unsafe { linux::ioctl(self.0.as_raw_fd(), HCIGETDEVINFO, &mut info) };
        if result < 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(ControllerSnapshot {
            device,
            flags: info.flags,
        })
    }

    fn set_up(&self, device: u16, up: bool) -> io::Result<()> {
        let request = if up { HCIDEVUP } else { HCIDEVDOWN };
        // SAFETY: HCIDEVUP/HCIDEVDOWN take the controller id by value.
        let result = unsafe { linux::ioctl(self.0.as_raw_fd(), request, i32::from(device)) };
        if result < 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(())
    }

    fn restore_exact(&self, wanted: ControllerSnapshot) -> io::Result<()> {
        let current = self.snapshot(wanted.device)?;
        let wanted_up = wanted.flags & HCI_UP != 0;
        if (current.flags & HCI_UP != 0) != wanted_up {
            self.set_up(wanted.device, wanted_up)?;
        }
        for _ in 0..50 {
            let current = self.snapshot(wanted.device)?;
            if current.flags == wanted.flags {
                return Ok(());
            }
            thread::sleep(Duration::from_millis(100));
        }
        let current = self.snapshot(wanted.device)?;
        Err(io::Error::other(format!(
            "controller flags did not restore exactly: initial={:#010x}, final={:#010x}",
            wanted.flags, current.flags
        )))
    }
}

struct ControllerGuard {
    control: ControllerControl,
    snapshot: ControllerSnapshot,
    restored: bool,
}

impl ControllerGuard {
    fn prepare(device: u16, state_path: &Path) -> io::Result<Self> {
        let control = ControllerControl::open()?;
        let snapshot = control.snapshot(device)?;
        write_state(state_path, snapshot)?;
        if snapshot.flags & HCI_UP != 0 {
            control.set_up(device, false)?;
            if control.snapshot(device)?.flags & HCI_UP != 0 {
                return Err(io::Error::other(
                    "controller remained powered after HCIDEVDOWN",
                ));
            }
        }
        Ok(Self {
            control,
            snapshot,
            restored: false,
        })
    }

    fn restore(&mut self) -> io::Result<()> {
        self.control.restore_exact(self.snapshot)?;
        self.restored = true;
        Ok(())
    }
}

impl Drop for ControllerGuard {
    fn drop(&mut self) {
        if !self.restored {
            let _ = self.control.restore_exact(self.snapshot);
        }
    }
}

struct Session {
    fd: OwnedFd,
    gate: CommandGate,
    peers: Peers,
    active: ActiveProcedures,
}

impl Session {
    fn open(device: u16) -> io::Result<Self> {
        // SAFETY: socket returns a new descriptor; arguments are Linux HCI constants.
        let raw = unsafe {
            linux::socket(
                AF_BLUETOOTH,
                linux::SOCK_RAW | linux::SOCK_CLOEXEC | linux::SOCK_NONBLOCK,
                BTPROTO_HCI,
            )
        };
        if raw < 0 {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: ownership of the newly created descriptor transfers exactly once.
        let fd = unsafe { OwnedFd::from_raw_fd(raw) };
        let address = SockAddrHci {
            family: AF_BLUETOOTH as _,
            device,
            channel: HCI_CHANNEL_USER,
        };
        // SAFETY: address points to an initialized sockaddr_hci of the supplied size.
        let result = unsafe {
            linux::bind(
                fd.as_raw_fd(),
                (&address as *const SockAddrHci).cast(),
                std::mem::size_of::<SockAddrHci>() as u32,
            )
        };
        if result < 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(Self {
            fd,
            gate: CommandGate::new(),
            peers: Peers::default(),
            active: ActiveProcedures::default(),
        })
    }

    fn initialize(&mut self) -> io::Result<()> {
        self.command(RESET, &[], Duration::from_secs(3))?;
        self.command(
            SET_EVENT_MASK,
            &[0xff, 0xff, 0xfb, 0xff, 0x07, 0xf8, 0xbf, 0x3d],
            Duration::from_secs(3),
        )?;
        self.command(
            LE_SET_EVENT_MASK,
            &[0x1f, 0, 0, 0, 0, 0, 0, 0],
            Duration::from_secs(3),
        )?;
        Ok(())
    }

    fn command(&mut self, opcode: u16, parameters: &[u8], timeout: Duration) -> io::Result<()> {
        self.gate.begin(opcode)?;
        let mut packet = Vec::with_capacity(parameters.len() + 4);
        packet.extend_from_slice(&[
            0x01,
            opcode as u8,
            (opcode >> 8) as u8,
            parameters.len() as u8,
        ]);
        packet.extend_from_slice(parameters);
        self.write_packet(&packet)?;
        let deadline = Instant::now() + timeout;
        loop {
            let bytes = self.read_packet(deadline)?;
            let event = decode_event(&bytes)?;
            self.peers.observe(&event)?;
            if let Some(status) = self.gate.observe(&event)? {
                if status == 0 {
                    return Ok(());
                }
                return Err(io::Error::other(format!(
                    "HCI opcode {opcode:#06x} failed with status {status:#04x}"
                )));
            }
        }
    }

    fn scan_le(&mut self, duration: Duration) -> io::Result<()> {
        // Active scanning, 10 ms interval/window, public local address, accept all.
        self.command(
            LE_SET_SCAN_PARAMETERS,
            &[1, 0x10, 0, 0x10, 0, 0, 0],
            Duration::from_secs(3),
        )?;
        // Mark active before sending: if the controller accepts the command but
        // its completion is lost, Drop still emits the bounded stop command.
        self.active.start_le_scan();
        self.command(LE_SET_SCAN_ENABLE, &[1, 1], Duration::from_secs(3))?;
        self.collect_until(Instant::now() + duration, false)?;
        self.command(LE_SET_SCAN_ENABLE, &[0, 1], Duration::from_secs(3))?;
        self.active.finish_le_scan();
        Ok(())
    }

    fn inquire(&mut self, duration: Duration) -> io::Result<()> {
        let units = ((duration.as_millis() + 1279) / 1280).clamp(1, 0x30) as u8;
        // As with scanning, assume the procedure may have started as soon as
        // its command is written, even if the response is malformed or lost.
        self.active.start_inquiry();
        self.command(
            INQUIRY,
            &[0x33, 0x8b, 0x9e, units, 0],
            Duration::from_secs(3),
        )?;
        let complete =
            self.collect_until(Instant::now() + duration + Duration::from_secs(3), true)?;
        if !complete {
            self.command(INQUIRY_CANCEL, &[], Duration::from_secs(3))?;
        }
        self.active.finish_inquiry();
        Ok(())
    }

    fn collect_until(&mut self, deadline: Instant, inquiry: bool) -> io::Result<bool> {
        loop {
            match self.read_packet(deadline) {
                Ok(bytes) => {
                    let event = decode_event(&bytes)?;
                    let complete = self.peers.observe(&event)?;
                    self.gate.observe(&event)?;
                    if inquiry && complete {
                        return Ok(true);
                    }
                }
                Err(error) if error.kind() == io::ErrorKind::TimedOut => return Ok(false),
                Err(error) => return Err(error),
            }
        }
    }

    fn write_packet(&self, packet: &[u8]) -> io::Result<()> {
        // SAFETY: packet is a valid readable slice for its length and fd is owned.
        let written =
            unsafe { linux::write(self.fd.as_raw_fd(), packet.as_ptr().cast(), packet.len()) };
        if written < 0 {
            return Err(io::Error::last_os_error());
        }
        if written as usize != packet.len() {
            return Err(io::Error::new(io::ErrorKind::WriteZero, "short HCI write"));
        }
        Ok(())
    }

    fn read_packet(&self, deadline: Instant) -> io::Result<Vec<u8>> {
        let now = Instant::now();
        if now >= deadline {
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "HCI deadline expired",
            ));
        }
        let timeout = (deadline - now).as_millis().clamp(1, i32::MAX as u128) as i32;
        let mut pollfd = linux::PollFd {
            fd: self.fd.as_raw_fd(),
            events: linux::POLLIN,
            revents: 0,
        };
        // SAFETY: pollfd points to one initialized entry for the duration of the call.
        let ready = unsafe { linux::poll(&mut pollfd, 1, timeout) };
        if ready < 0 {
            return Err(io::Error::last_os_error());
        }
        if ready == 0 {
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "HCI deadline expired",
            ));
        }
        if pollfd.revents & (linux::POLLERR | linux::POLLHUP | linux::POLLNVAL) != 0 {
            return Err(io::Error::new(
                io::ErrorKind::BrokenPipe,
                "HCI user channel closed",
            ));
        }
        let mut bytes = vec![0; MAX_HCI_PACKET + 1];
        // SAFETY: bytes is a valid writable allocation and fd is owned.
        let read =
            unsafe { linux::read(self.fd.as_raw_fd(), bytes.as_mut_ptr().cast(), bytes.len()) };
        if read < 0 {
            return Err(io::Error::last_os_error());
        }
        if read == 0 {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "HCI user channel EOF",
            ));
        }
        bytes.truncate(read as usize);
        Ok(bytes)
    }

    fn cleanup(&mut self) {
        for action in self.active.cleanup_actions() {
            match action {
                CleanupAction::DisableLeScan => {
                    self.best_effort(LE_SET_SCAN_ENABLE, &[0, 1]);
                }
                CleanupAction::CancelInquiry => self.best_effort(INQUIRY_CANCEL, &[]),
            }
        }
        self.active.clear();
    }

    fn best_effort(&self, opcode: u16, parameters: &[u8]) {
        let mut packet = vec![
            0x01,
            opcode as u8,
            (opcode >> 8) as u8,
            parameters.len() as u8,
        ];
        packet.extend_from_slice(parameters);
        let _ = self.write_packet(&packet);
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        self.cleanup();
    }
}

fn main() -> io::Result<()> {
    if let Some(action) = special_argument()? {
        let control = ControllerControl::open()?;
        match action {
            SpecialAction::Snapshot(path) => write_state(&path, control.snapshot(0)?)?,
            SpecialAction::Restore(path) => control.restore_exact(read_state(&path)?)?,
            SpecialAction::ProbeUserChannel => drop(Session::open(0)?),
        }
        return Ok(());
    }
    let (device, seconds, report, state) = arguments()?;
    let mut controller = ControllerGuard::prepare(device, &state)?;
    let peer_count;
    {
        let mut session = Session::open(device).map_err(|error| {
            io::Error::new(
                error.kind(),
                format!("cannot open exclusive hci{device} user channel: {error}"),
            )
        })?;
        session.initialize()?;
        session.scan_le(Duration::from_secs(seconds))?;
        session.inquire(Duration::from_secs(seconds))?;
        session.cleanup();
        write_report(&report, device, seconds, &session.peers)?;
        peer_count = session.peers.len();
    }
    controller.restore()?;
    println!(
        "discovery complete: {} unique peers; report written",
        peer_count
    );
    Ok(())
}

enum SpecialAction {
    Snapshot(PathBuf),
    Restore(PathBuf),
    ProbeUserChannel,
}

fn special_argument() -> io::Result<Option<SpecialAction>> {
    let args = env::args().skip(1).collect::<Vec<_>>();
    if args.as_slice() == ["--probe-user-channel"] {
        return Ok(Some(SpecialAction::ProbeUserChannel));
    }
    let action = match args.first().map(String::as_str) {
        Some("--snapshot-state") => SpecialAction::Snapshot,
        Some("--restore-state") => SpecialAction::Restore,
        _ => return Ok(None),
    };
    if args.len() != 2 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "--snapshot-state/--restore-state requires exactly one path",
        ));
    }
    let path = PathBuf::from(&args[1]);
    if !path.is_absolute() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "state path must be absolute",
        ));
    }
    Ok(Some(action(path)))
}

fn arguments() -> io::Result<(u16, u64, PathBuf, PathBuf)> {
    let mut device = 0;
    let mut seconds = 8;
    let mut report = None;
    let mut state = None;
    let mut args = env::args().skip(1);
    while let Some(arg) = args.next() {
        let value = || {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("missing value for {arg}"),
            )
        };
        match arg.as_str() {
            "--device" => {
                device = args
                    .next()
                    .ok_or_else(value)?
                    .parse()
                    .map_err(|_| value())?
            }
            "--seconds" => {
                seconds = args
                    .next()
                    .ok_or_else(value)?
                    .parse()
                    .map_err(|_| value())?
            }
            "--report" => report = Some(PathBuf::from(args.next().ok_or_else(value)?)),
            "--state" => state = Some(PathBuf::from(args.next().ok_or_else(value)?)),
            _ => {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "usage: bluetooth-hci-broker --report ABSOLUTE_PATH --state ABSOLUTE_PATH [--device N] [--seconds N]",
                ));
            }
        }
    }
    let report = report
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "--report is required"))?;
    let state =
        state.ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "--state is required"))?;
    if !report.is_absolute() || !state.is_absolute() || seconds == 0 || seconds > 60 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "report/state must be absolute and seconds must be 1..=60",
        ));
    }
    Ok((device, seconds, report, state))
}

fn write_state(path: &Path, snapshot: ControllerSnapshot) -> io::Result<()> {
    let mut output = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)?;
    writeln!(
        output,
        "device={}\nflags={:08x}",
        snapshot.device, snapshot.flags
    )?;
    output.sync_all()
}

fn read_state(path: &Path) -> io::Result<ControllerSnapshot> {
    parse_state(&std::fs::read_to_string(path)?)
}

fn parse_state(text: &str) -> io::Result<ControllerSnapshot> {
    let mut lines = text.lines();
    let device = lines
        .next()
        .and_then(|line| line.strip_prefix("device="))
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "invalid controller state"))?
        .parse()
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "invalid controller id"))?;
    let flags = u32::from_str_radix(
        lines
            .next()
            .and_then(|line| line.strip_prefix("flags="))
            .ok_or_else(|| {
                io::Error::new(io::ErrorKind::InvalidData, "invalid controller state")
            })?,
        16,
    )
    .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "invalid controller flags"))?;
    if lines.next().is_some() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "trailing controller state",
        ));
    }
    Ok(ControllerSnapshot { device, flags })
}

fn write_report(path: &Path, device: u16, seconds: u64, peers: &Peers) -> io::Result<()> {
    let mut output = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)?;
    writeln!(output, "{{")?;
    writeln!(output, "  \"schema\": 1,")?;
    writeln!(
        output,
        "  \"created_unix_seconds\": {},",
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs()
    )?;
    writeln!(output, "  \"controller\": \"hci{}\",", device)?;
    writeln!(output, "  \"scan_seconds_per_mode\": {},", seconds)?;
    writeln!(output, "  \"peers\": [")?;
    for (index, peer) in peers.values().enumerate() {
        let comma = if index + 1 == peers.len() { "" } else { "," };
        let address = peer
            .address
            .iter()
            .rev()
            .map(|byte| format!("{byte:02X}"))
            .collect::<Vec<_>>()
            .join(":");
        let name = peer
            .name
            .as_deref()
            .map(json_string)
            .unwrap_or_else(|| "null".into());
        let rssi = peer
            .best_rssi
            .map(|value| value.to_string())
            .unwrap_or_else(|| "null".into());
        writeln!(
            output,
            "    {{\"transport\":{},\"address\":{},\"name\":{},\"best_rssi\":{},\"sightings\":{}}}{}",
            json_string(peer.address_type),
            json_string(&address),
            name,
            rssi,
            peer.sightings,
            comma
        )?;
    }
    writeln!(output, "  ]\n}}")?;
    output.sync_all()
}

fn json_string(value: &str) -> String {
    let mut out = String::from("\"");
    for character in value.chars() {
        match character {
            '\"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if c < ' ' => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('\"');
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn linux_hci_device_info_layout_matches_uapi() {
        assert_eq!(std::mem::size_of::<HciDevInfo>(), 84);
    }

    #[test]
    fn parses_only_exact_controller_snapshots() {
        let snapshot = parse_state("device=0\nflags=00000005\n").unwrap();
        assert_eq!(snapshot.device, 0);
        assert_eq!(snapshot.flags, 5);
        assert!(parse_state("device=0\nflags=1\ntrailing=true\n").is_err());
        assert!(parse_state("device=x\nflags=1\n").is_err());
    }
}
