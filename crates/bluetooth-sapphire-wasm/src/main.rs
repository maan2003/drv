use std::collections::{BTreeSet, VecDeque};
use std::env;
use std::error::Error;
use std::fs;
use std::io;
use std::os::fd::{AsRawFd, FromRawFd};
use std::os::unix::net::UnixDatagram;
use std::os::unix::process::CommandExt;
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::thread;
use std::time::{Duration, Instant};

use bluetooth_hci_broker::{
    H4_EVENT, InboundKind, MAX_OUTBOUND_HCI_PACKET, OutboundKind, validate_inbound,
    validate_outbound,
};
use wasmtime::{Caller, Config, Engine, Extern, Linker, Module, Store};

const IPC_FD: i32 = 3;
const MAX_MODULE_BYTES: usize = 128 * 1024 * 1024;
const MAX_PACKET_BYTES: usize = 64 * 1024;
const MAX_LOG_BYTES: usize = 8 * 1024 * 1024;
const MAX_LOG_WRITE: usize = 64 * 1024;
const MAX_CONTROLLER_PACKETS: usize = 4096;
const MAX_CONTROLLER_BYTES: usize = 8 * 1024 * 1024;
const MAX_INBOUND_PACKETS: usize = 64;
const MAX_INBOUND_BYTES: usize = 256 * 1024;
const TEST_FUEL: u64 = 5_000_000_000;
const TEST_TIMEOUT: Duration = Duration::from_secs(120);

const MODULE_LENGTH: u8 = 1;
const MODULE_CHUNK: u8 = 2;
const LOG: u8 = 3;
const EXIT: u8 = 4;
const CLOCK: u8 = 5;
const RESULT: u8 = 6;
const PROGRESS: u8 = 7;
const CONTROLLER_SEND: u8 = 8;
const CONTROLLER_INBOUND: u8 = 9;
const CONTROLLER_INBOUND_ACK: u8 = 10;
const CONTROLLER_DRAIN: u8 = 11;
const CONTROLLER_DRAIN_DONE: u8 = 12;
const ACK: u8 = 128;
const CONTROLLER_MODULE: &str = "drv:bluetooth-sapphire/controller@0.1.0";
static NEXT_SESSION: AtomicU64 = AtomicU64::new(1);

struct TestHost {
    ipc: Arc<UnixDatagram>,
    session: u64,
    next_request_id: u32,
    next_inbound_sequence: u32,
    inbound_bytes: usize,
    inbound: VecDeque<InboundPacket>,
}

#[derive(Debug, Eq, PartialEq)]
struct InboundPacket {
    sequence: u32,
    kind: u8,
    bytes: Vec<u8>,
}

fn send_packet(socket: &UnixDatagram, operation: u8, payload: &[u8]) -> io::Result<()> {
    if payload.len() + 1 > MAX_PACKET_BYTES {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "IPC packet too large",
        ));
    }
    let mut packet = Vec::with_capacity(payload.len() + 1);
    packet.push(operation);
    packet.extend_from_slice(payload);
    if socket.send(&packet)? != packet.len() {
        return Err(io::Error::new(io::ErrorKind::WriteZero, "short IPC packet"));
    }
    Ok(())
}

fn receive_packet(socket: &UnixDatagram) -> io::Result<(u8, Vec<u8>)> {
    let mut packet = vec![0; MAX_PACKET_BYTES + 1];
    let length = loop {
        match socket.recv(&mut packet) {
            Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
            result => break result?,
        }
    };
    if length == 0 || length > MAX_PACKET_BYTES {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "invalid IPC packet",
        ));
    }
    packet.truncate(length);
    Ok((packet[0], packet[1..].to_vec()))
}

fn split_request(payload: &[u8], session: u64) -> io::Result<(u32, &[u8])> {
    if payload.len() < 12 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "missing request session or correlation id",
        ));
    }
    if u64::from_le_bytes(payload[..8].try_into().unwrap()) != session {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "request belongs to another worker session",
        ));
    }
    Ok((
        u32::from_le_bytes(payload[8..12].try_into().unwrap()),
        &payload[12..],
    ))
}

fn send_reply(
    socket: &UnixDatagram,
    session: u64,
    request_id: u32,
    payload: &[u8],
) -> io::Result<()> {
    let mut reply = Vec::with_capacity(payload.len() + 12);
    reply.extend_from_slice(&session.to_le_bytes());
    reply.extend_from_slice(&request_id.to_le_bytes());
    reply.extend_from_slice(payload);
    send_packet(socket, ACK, &reply)
}

fn request(host: &mut TestHost, operation: u8, payload: &[u8]) -> wasmtime::Result<Vec<u8>> {
    let request_id = host.next_request_id;
    host.next_request_id = host.next_request_id.wrapping_add(1);
    let mut request = Vec::with_capacity(payload.len() + 12);
    request.extend_from_slice(&host.session.to_le_bytes());
    request.extend_from_slice(&request_id.to_le_bytes());
    request.extend_from_slice(payload);
    send_packet(&host.ipc, operation, &request)?;
    loop {
        let (reply, payload) = receive_packet(&host.ipc)?;
        if reply == CONTROLLER_INBOUND {
            if host.enqueue_inbound(&payload) != 0 {
                return Err(wasmtime::Error::msg(
                    "invalid or over-quota inbound controller packet",
                ));
            }
            continue;
        }
        if reply != ACK || payload.len() < 12 {
            return Err(wasmtime::Error::msg("unexpected supervisor reply"));
        }
        let reply_session = u64::from_le_bytes(payload[..8].try_into().unwrap());
        let reply_id = u32::from_le_bytes(payload[8..12].try_into().unwrap());
        if reply_session != host.session {
            return Err(wasmtime::Error::msg("reply belongs to another session"));
        }
        if reply_id != request_id {
            return Err(wasmtime::Error::msg("mismatched supervisor reply"));
        }
        return Ok(payload[12..].to_vec());
    }
}

impl TestHost {
    fn enqueue_inbound(&mut self, payload: &[u8]) -> u8 {
        if payload.len() < 13 {
            return 1;
        }
        if u64::from_le_bytes(payload[..8].try_into().unwrap()) != self.session {
            return 1;
        }
        let sequence = u32::from_le_bytes(payload[8..12].try_into().unwrap());
        if sequence != self.next_inbound_sequence {
            return 1;
        }
        let kind = payload[12];
        let bytes = &payload[13..];
        let kind_value = match kind {
            0 => InboundKind::Event,
            1 => InboundKind::Acl,
            2 => InboundKind::Sco,
            3 => InboundKind::Iso,
            _ => return 1,
        };
        if validate_inbound(kind_value, bytes).is_err() {
            return 1;
        }
        if self.inbound.len() >= MAX_INBOUND_PACKETS
            || self.inbound_bytes.saturating_add(bytes.len()) > MAX_INBOUND_BYTES
        {
            return 2;
        }
        self.next_inbound_sequence = self.next_inbound_sequence.wrapping_add(1);
        self.inbound_bytes += bytes.len();
        self.inbound.push_back(InboundPacket {
            sequence,
            kind,
            bytes: bytes.to_vec(),
        });
        0
    }
}

fn guest_memory(caller: &mut Caller<'_, TestHost>) -> wasmtime::Result<wasmtime::Memory> {
    match caller.get_export("memory") {
        Some(Extern::Memory(memory)) => Ok(memory),
        _ => Err(wasmtime::Error::msg("guest did not export memory")),
    }
}

fn checked_range(
    pointer: i32,
    length: i32,
    limit: usize,
) -> wasmtime::Result<std::ops::Range<usize>> {
    let start = usize::try_from(pointer).map_err(|_| wasmtime::Error::msg("negative pointer"))?;
    let length = usize::try_from(length).map_err(|_| wasmtime::Error::msg("negative length"))?;
    if length > limit {
        return Err(wasmtime::Error::msg("guest transfer exceeds bound"));
    }
    let end = start
        .checked_add(length)
        .ok_or_else(|| wasmtime::Error::msg("guest range overflow"))?;
    Ok(start..end)
}

fn require_test_imports(module: &Module) -> wasmtime::Result<()> {
    let actual = module
        .imports()
        .map(|import| (import.module().to_owned(), import.name().to_owned()))
        .collect::<BTreeSet<_>>();
    let expected = [
        ("drv:test".to_owned(), "exit".to_owned()),
        ("drv:test".to_owned(), "log".to_owned()),
        (
            "wasi_snapshot_preview1".to_owned(),
            "clock_time_get".to_owned(),
        ),
    ]
    .into_iter()
    .collect::<BTreeSet<_>>();
    let mut expected_with_controller = expected.clone();
    expected_with_controller.insert((CONTROLLER_MODULE.to_owned(), "send".to_owned()));
    if actual != expected && actual != expected_with_controller {
        return Err(wasmtime::Error::msg(format!(
            "unexpected Sapphire test imports: {actual:?}"
        )));
    }
    Ok(())
}

fn receive_module(socket: &UnixDatagram) -> io::Result<(u64, Vec<u8>)> {
    let (operation, payload) = receive_packet(socket)?;
    if operation != MODULE_LENGTH || payload.len() != 16 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "missing module length",
        ));
    }
    let session = u64::from_le_bytes(payload[..8].try_into().unwrap());
    let length = usize::try_from(u64::from_le_bytes(payload[8..].try_into().unwrap()))
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "invalid module length"))?;
    if length > MAX_MODULE_BYTES {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "module exceeds bound",
        ));
    }
    let mut module = Vec::with_capacity(length);
    while module.len() < length {
        let (operation, chunk) = receive_packet(socket)?;
        if operation != MODULE_CHUNK || chunk.len() > length - module.len() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "invalid module chunk",
            ));
        }
        module.extend_from_slice(&chunk);
    }
    Ok((session, module))
}

fn run_worker(socket: Arc<UnixDatagram>) -> wasmtime::Result<i32> {
    let (session, bytes) = receive_module(&socket)?;
    send_packet(&socket, PROGRESS, b"module-received")?;
    let mut config = Config::new();
    config
        .strategy(wasmtime::Strategy::Winch)
        .consume_fuel(true)
        .epoch_interruption(true)
        .memory_init_cow(false);
    let engine = Engine::new(&config)?;
    let module = Module::from_binary(&engine, &bytes)?;
    require_test_imports(&module)?;
    send_packet(&socket, PROGRESS, b"module-compiled-imports-valid")?;

    let mut linker = Linker::<TestHost>::new(&engine);
    linker.func_wrap(
        "drv:test",
        "log",
        |mut caller: Caller<'_, TestHost>, fd: i32, pointer: i32, length: i32| {
            let range = checked_range(pointer, length, MAX_LOG_WRITE)?;
            let memory = guest_memory(&mut caller)?;
            let bytes = memory
                .data(&caller)
                .get(range)
                .ok_or_else(|| wasmtime::Error::msg("guest log range is outside memory"))?
                .to_vec();
            let mut payload = Vec::with_capacity(bytes.len() + 4);
            payload.extend_from_slice(&fd.to_le_bytes());
            payload.extend_from_slice(&bytes);
            request(caller.data_mut(), LOG, &payload)?;
            Ok(())
        },
    )?;
    linker.func_wrap(
        "drv:test",
        "exit",
        |mut caller: Caller<'_, TestHost>, status: i32| -> wasmtime::Result<()> {
            request(caller.data_mut(), EXIT, &status.to_le_bytes())?;
            Err(wasmtime::Error::msg(format!(
                "guest called proc_exit({status})"
            )))
        },
    )?;
    linker.func_wrap(
        "wasi_snapshot_preview1",
        "clock_time_get",
        |mut caller: Caller<'_, TestHost>, _clock: i32, _precision: i64, result: i32| {
            let reply = request(caller.data_mut(), CLOCK, &[])?;
            let now: [u8; 8] = reply
                .try_into()
                .map_err(|_| wasmtime::Error::msg("invalid clock reply"))?;
            let range = checked_range(result, 8, 8)?;
            guest_memory(&mut caller)?
                .data_mut(&mut caller)
                .get_mut(range)
                .ok_or_else(|| wasmtime::Error::msg("clock result is outside guest memory"))?
                .copy_from_slice(&now);
            Ok(0_i32)
        },
    )?;
    // Transitional core-Wasm lowering of the WIT controller.send capability.
    // The final component adapter retains the same copied packet and result
    // semantics; neither form exposes a native controller descriptor.
    linker.func_wrap(
        CONTROLLER_MODULE,
        "send",
        |mut caller: Caller<'_, TestHost>, kind: i32, pointer: i32, length: i32| {
            let kind = u8::try_from(kind)
                .map_err(|_| wasmtime::Error::msg("invalid outbound HCI kind"))?;
            let range = checked_range(pointer, length, MAX_OUTBOUND_HCI_PACKET)?;
            let memory = guest_memory(&mut caller)?;
            let bytes = memory
                .data(&caller)
                .get(range)
                .ok_or_else(|| wasmtime::Error::msg("HCI packet range is outside memory"))?;
            let mut payload = Vec::with_capacity(bytes.len() + 1);
            payload.push(kind);
            payload.extend_from_slice(bytes);
            let reply = request(caller.data_mut(), CONTROLLER_SEND, &payload)?;
            match reply.as_slice() {
                [status] => Ok(i32::from(*status)),
                _ => Err(wasmtime::Error::msg("invalid controller broker reply")),
            }
        },
    )?;

    let mut store = Store::new(
        &engine,
        TestHost {
            ipc: Arc::clone(&socket),
            session,
            next_request_id: 0,
            next_inbound_sequence: 0,
            inbound_bytes: 0,
            inbound: VecDeque::new(),
        },
    );
    store.set_fuel(TEST_FUEL)?;
    store.set_epoch_deadline(1);
    store.epoch_deadline_trap();
    let deadline_engine = engine.clone();
    thread::spawn(move || {
        thread::sleep(TEST_TIMEOUT);
        deadline_engine.increment_epoch();
    });
    let instance = linker.instantiate(&mut store, &module)?;
    send_packet(&socket, PROGRESS, b"instance-created")?;
    instance
        .get_typed_func::<(), ()>(&mut store, "_initialize")?
        .call(&mut store, ())?;
    send_packet(&socket, PROGRESS, b"initialized-entering-main")?;
    let status = instance
        .get_typed_func::<(), i32>(&mut store, "drv_test_entry")?
        .call(&mut store, ())?;
    loop {
        if store.data().inbound.is_empty() {
            send_packet(&socket, CONTROLLER_DRAIN, &session.to_le_bytes())?;
            let (operation, payload) = receive_packet(&socket)?;
            match operation {
                CONTROLLER_INBOUND => {
                    if store.data_mut().enqueue_inbound(&payload) != 0 {
                        return Err(wasmtime::Error::msg(
                            "invalid controller packet while draining",
                        ));
                    }
                }
                CONTROLLER_DRAIN_DONE if payload == session.to_le_bytes() => break,
                _ => return Err(wasmtime::Error::msg("invalid controller drain reply")),
            }
        }
        let Some(packet) = store.data_mut().inbound.pop_front() else {
            continue;
        };
        store.data_mut().inbound_bytes -= packet.bytes.len();
        let callback =
            instance.get_typed_func::<(i32, i32, i32), i32>(&mut store, "drv_controller_packet")?;
        let memory = instance
            .get_memory(&mut store, "memory")
            .ok_or_else(|| wasmtime::Error::msg("guest did not export memory"))?;
        memory.write(&mut store, 0, &packet.bytes)?;
        let callback_result = callback.call(
            &mut store,
            (packet.kind.into(), 0, packet.bytes.len() as i32),
        );
        let callback_status = callback_result.as_ref().copied().unwrap_or(1);
        let mut delivered = Vec::with_capacity(13);
        delivered.extend_from_slice(&session.to_le_bytes());
        delivered.extend_from_slice(&packet.sequence.to_le_bytes());
        delivered.push(u8::from(callback_result.is_err() || callback_status != 0));
        send_packet(&socket, CONTROLLER_INBOUND_ACK, &delivered)?;
        callback_result?;
        if callback_status != 0 {
            return Err(wasmtime::Error::msg("guest rejected controller packet"));
        }
    }
    Ok(status)
}

fn set_limit(resource: libc::__rlimit_resource_t, value: libc::rlim_t) -> io::Result<()> {
    let limit = libc::rlimit {
        rlim_cur: value,
        rlim_max: value,
    };
    // SAFETY: `limit` points to a valid `rlimit` for the duration of the call.
    if unsafe { libc::setrlimit(resource, &limit) } == 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}

fn spawn_worker(socket: &UnixDatagram) -> io::Result<Child> {
    let source_fd = socket.as_raw_fd();
    let mut command = Command::new(env::current_exe()?);
    command
        .arg("--worker")
        .env_clear()
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    // SAFETY: The closure only invokes async-signal-safe syscalls before exec.
    unsafe {
        command.pre_exec(move || {
            if libc::dup2(source_fd, IPC_FD) < 0 {
                return Err(io::Error::last_os_error());
            }
            if libc::fcntl(IPC_FD, libc::F_SETFD, 0) < 0 {
                return Err(io::Error::last_os_error());
            }
            if libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGKILL) < 0 {
                return Err(io::Error::last_os_error());
            }
            libc::close(0);
            libc::close(1);
            libc::close(2);
            if libc::syscall(libc::SYS_close_range, 4_u32, u32::MAX, 0_u32) < 0 {
                let error = io::Error::last_os_error();
                if error.raw_os_error() != Some(libc::ENOSYS) {
                    return Err(error);
                }
                for fd in 4..1024 {
                    libc::close(fd);
                }
            }
            set_limit(libc::RLIMIT_NOFILE, 4)?;
            set_limit(libc::RLIMIT_CORE, 0)?;
            set_limit(libc::RLIMIT_FSIZE, 0)?;
            set_limit(libc::RLIMIT_AS, 8 * 1024 * 1024 * 1024)?;
            set_limit(libc::RLIMIT_CPU, TEST_TIMEOUT.as_secs())?;
            Ok(())
        });
    }
    command.spawn()
}

struct WorkerGuard {
    child: Child,
    reaped: bool,
}

impl WorkerGuard {
    fn new(child: Child) -> Self {
        Self {
            child,
            reaped: false,
        }
    }

    fn kill(&mut self) -> io::Result<()> {
        self.child.kill()
    }

    fn wait(&mut self) -> io::Result<std::process::ExitStatus> {
        let status = self.child.wait()?;
        self.reaped = true;
        Ok(status)
    }

    fn try_wait(&mut self) -> io::Result<Option<std::process::ExitStatus>> {
        let status = self.child.try_wait()?;
        self.reaped |= status.is_some();
        Ok(status)
    }
}

impl Drop for WorkerGuard {
    fn drop(&mut self) {
        if !self.reaped {
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
    }
}

#[derive(Debug, Default, Eq, PartialEq)]
struct FakeControllerBroker {
    packets: usize,
    bytes: usize,
    next_inbound_sequence: u32,
    inbound_bytes: usize,
    inbound: VecDeque<InboundPacket>,
    in_flight: Option<(u32, usize)>,
}

#[derive(Debug)]
struct ControllerSend {
    status: u8,
}

impl FakeControllerBroker {
    fn send(&mut self, payload: &[u8]) -> ControllerSend {
        let Some((&kind, packet)) = payload.split_first() else {
            return ControllerSend { status: 1 };
        };
        let kind = match kind {
            0 => OutboundKind::Command,
            1 => OutboundKind::Acl,
            2 => OutboundKind::Sco,
            3 => OutboundKind::Iso,
            _ => {
                return ControllerSend { status: 1 };
            }
        };
        if validate_outbound(kind, packet).is_err() {
            return ControllerSend { status: 1 };
        }
        if self.packets >= MAX_CONTROLLER_PACKETS
            || self.bytes.saturating_add(packet.len()) > MAX_CONTROLLER_BYTES
        {
            return ControllerSend { status: 2 };
        }
        let inbound = match kind {
            OutboundKind::Command => InboundPacket {
                sequence: 0,
                kind: 0,
                bytes: vec![H4_EVENT, 0x0e, 4, 1, packet[0], packet[1], 0],
            },
            OutboundKind::Acl => InboundPacket {
                sequence: 0,
                kind: 1,
                bytes: packet.to_vec(),
            },
            OutboundKind::Sco => InboundPacket {
                sequence: 0,
                kind: 2,
                bytes: packet.to_vec(),
            },
            OutboundKind::Iso => InboundPacket {
                sequence: 0,
                kind: 3,
                bytes: packet.to_vec(),
            },
        };
        let outstanding = self.inbound.len() + usize::from(self.in_flight.is_some());
        if outstanding >= MAX_INBOUND_PACKETS
            || self.inbound_bytes.saturating_add(inbound.bytes.len()) > MAX_INBOUND_BYTES
        {
            return ControllerSend { status: 2 };
        }
        self.packets += 1;
        self.bytes += packet.len();
        self.inbound_bytes += inbound.bytes.len();
        self.inbound.push_back(inbound);
        ControllerSend { status: 0 }
    }

    fn acknowledge_inbound(&mut self, sequence: u32) -> bool {
        let Some((expected, bytes)) = self.in_flight else {
            return false;
        };
        if sequence != expected {
            return false;
        }
        self.in_flight = None;
        self.inbound_bytes -= bytes;
        self.next_inbound_sequence = self.next_inbound_sequence.wrapping_add(1);
        true
    }

    fn cleanup(&mut self) {
        self.packets = 0;
        self.bytes = 0;
        self.next_inbound_sequence = 0;
        self.inbound_bytes = 0;
        self.inbound.clear();
        self.in_flight = None;
    }
}

impl Drop for FakeControllerBroker {
    fn drop(&mut self) {
        self.cleanup();
    }
}

fn schedule_inbound(
    socket: &UnixDatagram,
    session: u64,
    controller: &mut FakeControllerBroker,
) -> io::Result<()> {
    if controller.in_flight.is_some() {
        return Ok(());
    }
    let Some(packet) = controller.inbound.pop_front() else {
        return Ok(());
    };
    let sequence = controller.next_inbound_sequence;
    let mut payload = Vec::with_capacity(packet.bytes.len() + 13);
    payload.extend_from_slice(&session.to_le_bytes());
    payload.extend_from_slice(&sequence.to_le_bytes());
    payload.push(packet.kind);
    payload.extend_from_slice(&packet.bytes);
    send_packet(socket, CONTROLLER_INBOUND, &payload)?;
    controller.in_flight = Some((sequence, packet.bytes.len()));
    Ok(())
}

fn run_supervisor_attempt(module: &[u8], generation: u8) -> Result<String, Box<dyn Error>> {
    let session = NEXT_SESSION.fetch_add(1, Ordering::Relaxed);
    let (supervisor, worker) = UnixDatagram::pair()?;
    supervisor.set_read_timeout(Some(Duration::from_secs(1)))?;
    let mut child = WorkerGuard::new(spawn_worker(&worker)?);
    drop(worker);

    let mut module_header = Vec::with_capacity(16);
    module_header.extend_from_slice(&session.to_le_bytes());
    module_header.extend_from_slice(&(module.len() as u64).to_le_bytes());
    send_packet(&supervisor, MODULE_LENGTH, &module_header)?;
    for chunk in module.chunks(MAX_PACKET_BYTES - 1) {
        send_packet(&supervisor, MODULE_CHUNK, chunk)?;
    }

    let started = Instant::now();
    let mut log = format!("[supervisor: worker generation {generation}]\n").into_bytes();
    let mut now_nanoseconds = 0_u64;
    let mut clock_requests = 0_u64;
    let mut controller = FakeControllerBroker::default();
    let status = loop {
        if started.elapsed() > TEST_TIMEOUT {
            child.kill()?;
            let child_status = child.wait()?;
            return Err(format!(
                "Sapphire worker timed out ({child_status}, {clock_requests} clock requests)\n{}",
                String::from_utf8_lossy(&log)
            )
            .into());
        }
        match receive_packet(&supervisor) {
            Ok((LOG, payload)) => {
                let (request_id, payload) = split_request(&payload, session)?;
                if payload.len() < 4 {
                    child.kill()?;
                    return Err("invalid Sapphire log request".into());
                }
                if log.len().saturating_add(payload.len() - 4) > MAX_LOG_BYTES {
                    child.kill()?;
                    return Err("Sapphire worker exceeded log quota".into());
                }
                log.extend_from_slice(&payload[4..]);
                send_reply(&supervisor, session, request_id, &[])?;
            }
            Ok((CLOCK, payload)) => {
                let (request_id, payload) = split_request(&payload, session)?;
                if !payload.is_empty() {
                    child.kill()?;
                    return Err("invalid Sapphire clock request".into());
                }
                let now = now_nanoseconds;
                now_nanoseconds = now.saturating_add(1_000_000);
                clock_requests += 1;
                send_reply(&supervisor, session, request_id, &now.to_le_bytes())?;
            }
            Ok((PROGRESS, payload)) if payload.len() <= 64 && payload.is_ascii() => {
                log.extend_from_slice(b"[worker: ");
                log.extend_from_slice(&payload);
                log.extend_from_slice(b"]\n");
            }
            Ok((CONTROLLER_SEND, payload)) => {
                let (request_id, payload) = split_request(&payload, session)?;
                let sent = controller.send(payload);
                send_reply(&supervisor, session, request_id, &[sent.status])?;
                schedule_inbound(&supervisor, session, &mut controller)?;
            }
            Ok((CONTROLLER_INBOUND_ACK, payload)) => {
                if payload.len() != 13
                    || u64::from_le_bytes(payload[..8].try_into().unwrap()) != session
                    || payload[12] != 0
                    || !controller
                        .acknowledge_inbound(u32::from_le_bytes(payload[8..12].try_into().unwrap()))
                {
                    return Err("invalid controller delivery completion".into());
                }
            }
            Ok((CONTROLLER_DRAIN, payload)) => {
                if payload != session.to_le_bytes() {
                    return Err("controller drain belongs to another session".into());
                }
                if controller.in_flight.is_none() && controller.inbound.is_empty() {
                    send_packet(&supervisor, CONTROLLER_DRAIN_DONE, &session.to_le_bytes())?;
                } else {
                    schedule_inbound(&supervisor, session, &mut controller)?;
                }
            }
            Ok((EXIT, payload)) => {
                let (request_id, payload) = split_request(&payload, session)?;
                if payload.len() != 4 {
                    child.kill()?;
                    return Err("invalid Sapphire exit request".into());
                }
                send_reply(&supervisor, session, request_id, &[])?;
                break i32::from_le_bytes(payload.try_into().unwrap());
            }
            Ok((RESULT, payload)) if payload.len() >= 4 => {
                let status = i32::from_le_bytes(payload[..4].try_into().unwrap());
                if payload.len() > 4 {
                    log.extend_from_slice(&payload[4..]);
                }
                break status;
            }
            Ok(_) => {
                child.kill()?;
                return Err("invalid Sapphire worker IPC message".into());
            }
            Err(error)
                if matches!(
                    error.kind(),
                    io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut
                ) =>
            {
                if let Some(status) = child.try_wait()? {
                    return Err(format!(
                        "Sapphire worker exited early: {status}\n{}",
                        String::from_utf8_lossy(&log)
                    )
                    .into());
                }
            }
            Err(error) => return Err(error.into()),
        }
    };
    let child_status = child.wait()?;
    if status != 0 || !child_status.success() {
        return Err(format!(
            "Sapphire GAP test failed ({status}, {child_status})\n{}",
            String::from_utf8_lossy(&log)
        )
        .into());
    }
    log.extend_from_slice(
        format!(
            "[supervisor: {clock_requests} clock requests, final={now_nanoseconds}ns; {} controller packets, {} bytes]\n",
            controller.packets, controller.bytes
        )
        .as_bytes(),
    );
    Ok(String::from_utf8_lossy(&log).into_owned())
}

fn retry_worker<T>(mut attempt: impl FnMut(u8) -> Result<T, String>) -> Result<T, String> {
    let first = match attempt(0) {
        Ok(value) => return Ok(value),
        Err(error) => error,
    };
    attempt(1).map_err(|second| {
        format!("worker failed before and after one bounded restart: {first}; {second}")
    })
}

fn run_supervisor(path: impl AsRef<Path>) -> Result<String, Box<dyn Error>> {
    let module = fs::read(path)?;
    if module.len() > MAX_MODULE_BYTES {
        return Err("module exceeds bound".into());
    }
    retry_worker(|generation| {
        run_supervisor_attempt(&module, generation).map_err(|error| error.to_string())
    })
    .map_err(Into::into)
}

fn worker_main() -> Result<(), Box<dyn Error>> {
    // SAFETY: The supervisor installs the connected socket at this fixed fd and
    // transfers ownership to the worker across exec.
    let socket = Arc::new(unsafe { UnixDatagram::from_raw_fd(IPC_FD) });
    let status = match run_worker(Arc::clone(&socket)) {
        Ok(status) => status,
        Err(error) => {
            let message = error.to_string();
            let mut payload = 2_i32.to_le_bytes().to_vec();
            payload.extend_from_slice(message.as_bytes());
            let _ = send_packet(&socket, RESULT, &payload);
            return Err(error.into());
        }
    };
    send_packet(&socket, RESULT, &status.to_le_bytes())?;
    Ok(())
}

fn main() -> Result<(), Box<dyn Error>> {
    let mut args = env::args_os();
    let program = args.next().unwrap_or_default();
    if args.next().as_deref() == Some(std::ffi::OsStr::new("--worker")) {
        if args.next().is_some() {
            return Err("worker accepts no arguments".into());
        }
        return worker_main();
    }
    let mut args = env::args_os().skip(1);
    let Some(module) = args.next() else {
        return Err(format!("usage: {} GAP_TEST.wasm", program.to_string_lossy()).into());
    };
    if args.next().is_some() {
        return Err("expected exactly one WebAssembly module".into());
    }
    print!("{}", run_supervisor(module)?);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const LOOP_MODULE: &[u8] = b"\0asm\x01\0\0\0\x01\x04\x01\x60\0\0\x03\x02\x01\0\x07\x07\x01\x03run\0\0\x0a\x09\x01\x07\0\x03\x40\x0c\0\x0b\x0b";

    #[test]
    fn controller_send_round_trips_over_framed_ipc() {
        let module = wat::parse_str(format!(
            r#"(module
                (import "drv:test" "log" (func (param i32 i32 i32)))
                (import "drv:test" "exit" (func (param i32)))
                (import "wasi_snapshot_preview1" "clock_time_get"
                    (func $clock (param i32 i64 i32) (result i32)))
                (import "{CONTROLLER_MODULE}" "send"
                    (func $send (param i32 i32 i32) (result i32)))
                (memory (export "memory") 1)
                (data (i32.const 0) "\0c\20\01\01")
                (data (i32.const 8) "\01\00\02\00\aa\bb")
                (data (i32.const 16) "\01\00\01\aa")
                (data (i32.const 24) "\01\00\02\00\aa\bb")
                (global $next-kind (mut i32) (i32.const 0))
                (func (export "_initialize"))
                (func (export "drv_test_entry") (result i32)
                    i32.const 0 i32.const 0 i32.const 4 call $send drop
                    i32.const 1 i32.const 8 i32.const 6 call $send drop
                    i32.const 2 i32.const 16 i32.const 4 call $send drop
                    i32.const 3 i32.const 24 i32.const 6 call $send drop
                    i32.const 0)
                (func (export "drv_controller_packet")
                    (param $kind i32) (param i32) (param i32) (result i32)
                    local.get $kind
                    global.get $next-kind
                    i32.ne
                    if (result i32)
                        i32.const 1
                    else
                        global.get $next-kind
                        i32.const 1
                        i32.add
                        global.set $next-kind
                        local.get $kind
                        i32.eqz
                        if
                            i32.const 0 i64.const 0 i32.const 100 call $clock drop
                        end
                        i32.const 0
                    end))"#
        ))
        .unwrap();
        let (supervisor, worker) = UnixDatagram::pair().unwrap();
        supervisor
            .set_read_timeout(Some(Duration::from_secs(2)))
            .unwrap();
        let worker = Arc::new(worker);
        let worker_thread = thread::spawn(move || run_worker(worker).unwrap());

        let session = 42_u64;
        let mut module_header = Vec::new();
        module_header.extend_from_slice(&session.to_le_bytes());
        module_header.extend_from_slice(&(module.len() as u64).to_le_bytes());
        send_packet(&supervisor, MODULE_LENGTH, &module_header).unwrap();
        for chunk in module.chunks(MAX_PACKET_BYTES - 1) {
            send_packet(&supervisor, MODULE_CHUNK, chunk).unwrap();
        }

        let mut progress = 0;
        let expected = [
            vec![0, 0x0c, 0x20, 1, 1],
            vec![1, 1, 0, 2, 0, 0xaa, 0xbb],
            vec![2, 1, 0, 1, 0xaa],
            vec![3, 1, 0, 2, 0, 0xaa, 0xbb],
        ];
        let mut sent = 0;
        while sent < expected.len() {
            match receive_packet(&supervisor).unwrap() {
                (PROGRESS, _) => progress += 1,
                (CONTROLLER_SEND, payload) => {
                    let (request_id, payload) = split_request(&payload, session).unwrap();
                    assert_eq!(payload, expected[sent]);
                    send_reply(&supervisor, session, request_id, &[0]).unwrap();
                    if sent == 0 {
                        let mut delivery = Vec::new();
                        delivery.extend_from_slice(&session.to_le_bytes());
                        delivery.extend_from_slice(&0_u32.to_le_bytes());
                        delivery.push(0);
                        delivery.extend_from_slice(&[H4_EVENT, 0x0e, 4, 1, 0x0c, 0x20, 0]);
                        send_packet(&supervisor, CONTROLLER_INBOUND, &delivery).unwrap();
                    }
                    sent += 1;
                }
                packet => panic!("unexpected worker packet: {packet:?}"),
            }
        }
        assert_eq!(progress, 4);
        for sequence in 0..expected.len() {
            if sequence == 0 {
                let (operation, payload) = receive_packet(&supervisor).unwrap();
                assert_eq!(operation, CLOCK);
                let (request_id, payload) = split_request(&payload, session).unwrap();
                assert!(payload.is_empty());
                send_reply(&supervisor, session, request_id, &0_u64.to_le_bytes()).unwrap();
            }
            let (operation, payload) = receive_packet(&supervisor).unwrap();
            assert_eq!(operation, CONTROLLER_INBOUND_ACK);
            assert_eq!(&payload[..8], &session.to_le_bytes());
            assert_eq!(
                u32::from_le_bytes(payload[8..12].try_into().unwrap()),
                sequence as u32
            );
            assert_eq!(payload[12], 0);

            let (operation, payload) = receive_packet(&supervisor).unwrap();
            assert_eq!(
                (operation, payload),
                (CONTROLLER_DRAIN, session.to_le_bytes().to_vec())
            );
            if sequence + 1 < expected.len() {
                let next = sequence + 1;
                let mut delivery = Vec::new();
                delivery.extend_from_slice(&session.to_le_bytes());
                delivery.extend_from_slice(&(next as u32).to_le_bytes());
                delivery.push(next as u8);
                delivery.extend_from_slice(&expected[next][1..]);
                send_packet(&supervisor, CONTROLLER_INBOUND, &delivery).unwrap();
            } else {
                send_packet(&supervisor, CONTROLLER_DRAIN_DONE, &session.to_le_bytes()).unwrap();
            }
        }
        assert_eq!(worker_thread.join().unwrap(), 0);
    }

    #[test]
    fn fake_controller_broker_bounds_and_validates_packets() {
        let mut broker = FakeControllerBroker::default();
        let sent = broker.send(&[0, 0x0c, 0x20, 1, 1]);
        assert_eq!(sent.status, 0);
        assert_eq!(broker.inbound.front().unwrap().kind, 0);
        assert_eq!(broker.packets, 1);
        assert_eq!(broker.bytes, 4);
        assert_eq!(broker.send(&[0, 0x0c, 0x20, 2, 1]).status, 1);
        assert_eq!(broker.send(&[4, 0, 0, 0]).status, 1);
        broker.packets = MAX_CONTROLLER_PACKETS;
        assert_eq!(broker.send(&[2, 1, 0, 1, 0xaa]).status, 2);
        broker.cleanup();
        assert_eq!(broker, FakeControllerBroker::default());
    }

    #[test]
    fn inbound_queue_enforces_order_and_backpressure() {
        let (socket, _peer) = UnixDatagram::pair().unwrap();
        let mut host = TestHost {
            ipc: Arc::new(socket),
            session: 11,
            next_request_id: 0,
            next_inbound_sequence: 0,
            inbound_bytes: 0,
            inbound: VecDeque::new(),
        };
        let payload = |sequence: u32| {
            [
                &11_u64.to_le_bytes()[..],
                &sequence.to_le_bytes()[..],
                &[0, H4_EVENT, 0x01, 0][..],
            ]
            .concat()
        };
        assert_eq!(host.enqueue_inbound(&payload(0)), 0);
        let mut wrong_session = payload(1);
        wrong_session[..8].copy_from_slice(&12_u64.to_le_bytes());
        assert_eq!(host.enqueue_inbound(&wrong_session), 1);
        assert_eq!(host.enqueue_inbound(&payload(0)), 1);
        for sequence in 1..MAX_INBOUND_PACKETS as u32 {
            assert_eq!(host.enqueue_inbound(&payload(sequence)), 0);
        }
        assert_eq!(
            host.enqueue_inbound(&payload(MAX_INBOUND_PACKETS as u32)),
            2
        );
        assert_eq!(host.inbound.len(), MAX_INBOUND_PACKETS);
    }

    #[test]
    fn fake_controller_delivery_is_stop_and_wait() {
        let (socket, peer) = UnixDatagram::pair().unwrap();
        peer.set_read_timeout(Some(Duration::from_millis(20)))
            .unwrap();
        let session = 17_u64;
        let mut broker = FakeControllerBroker::default();
        assert_eq!(broker.send(&[2, 1, 0, 1, 0xaa]).status, 0);
        assert_eq!(broker.send(&[2, 1, 0, 1, 0xbb]).status, 0);

        schedule_inbound(&socket, session, &mut broker).unwrap();
        let (operation, first) = receive_packet(&peer).unwrap();
        assert_eq!(operation, CONTROLLER_INBOUND);
        assert_eq!(&first[..8], &session.to_le_bytes());
        assert_eq!(u32::from_le_bytes(first[8..12].try_into().unwrap()), 0);

        schedule_inbound(&socket, session, &mut broker).unwrap();
        assert!(matches!(
            receive_packet(&peer).unwrap_err().kind(),
            io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut
        ));
        assert!(!broker.acknowledge_inbound(1));
        assert!(broker.acknowledge_inbound(0));

        schedule_inbound(&socket, session, &mut broker).unwrap();
        let (operation, second) = receive_packet(&peer).unwrap();
        assert_eq!(operation, CONTROLLER_INBOUND);
        assert_eq!(u32::from_le_bytes(second[8..12].try_into().unwrap()), 1);
    }

    #[test]
    fn correlated_request_rejects_mismatched_reply() {
        let (socket, peer) = UnixDatagram::pair().unwrap();
        let session = 12_u64;
        let peer = thread::spawn(move || {
            let (operation, payload) = receive_packet(&peer).unwrap();
            assert_eq!(operation, CLOCK);
            let (request_id, payload) = split_request(&payload, session).unwrap();
            assert!(payload.is_empty());
            send_reply(&peer, session, request_id + 1, &[]).unwrap();
        });
        let mut host = TestHost {
            ipc: Arc::new(socket),
            session,
            next_request_id: 7,
            next_inbound_sequence: 0,
            inbound_bytes: 0,
            inbound: VecDeque::new(),
        };
        assert!(request(&mut host, CLOCK, &[]).is_err());
        peer.join().unwrap();
    }

    #[test]
    fn worker_restart_is_bounded_and_resets_broker_state() {
        let mut broker = FakeControllerBroker::default();
        let mut generations = Vec::new();
        let result = retry_worker(|generation| {
            generations.push(generation);
            if generation == 0 {
                assert_eq!(broker.send(&[0, 0x0c, 0x20, 0]).status, 0);
                broker.cleanup();
                Err("simulated worker crash".to_owned())
            } else {
                assert_eq!(broker, FakeControllerBroker::default());
                Ok("restarted")
            }
        });
        assert_eq!(result.unwrap(), "restarted");
        assert_eq!(generations, [0, 1]);

        let mut attempts = 0;
        assert!(
            retry_worker::<()>(|_| {
                attempts += 1;
                Err("crash".to_owned())
            })
            .is_err()
        );
        assert_eq!(attempts, 2);
    }

    #[test]
    fn fuel_interrupts_guest_execution() {
        let mut config = Config::new();
        config
            .strategy(wasmtime::Strategy::Winch)
            .consume_fuel(true);
        let engine = Engine::new(&config).unwrap();
        let module = Module::from_binary(&engine, LOOP_MODULE).unwrap();
        let mut store = Store::new(&engine, ());
        store.set_fuel(1_000).unwrap();
        let instance = wasmtime::Instance::new(&mut store, &module, &[]).unwrap();
        assert!(
            instance
                .get_typed_func::<(), ()>(&mut store, "run")
                .unwrap()
                .call(&mut store, ())
                .is_err()
        );
    }

    #[test]
    fn epoch_interrupts_guest_execution() {
        let mut config = Config::new();
        config
            .strategy(wasmtime::Strategy::Winch)
            .epoch_interruption(true);
        let engine = Engine::new(&config).unwrap();
        let module = Module::from_binary(&engine, LOOP_MODULE).unwrap();
        let mut store = Store::new(&engine, ());
        store.set_epoch_deadline(1);
        store.epoch_deadline_trap();
        let deadline_engine = engine.clone();
        thread::spawn(move || {
            thread::sleep(Duration::from_millis(10));
            deadline_engine.increment_epoch();
        });
        let instance = wasmtime::Instance::new(&mut store, &module, &[]).unwrap();
        assert!(
            instance
                .get_typed_func::<(), ()>(&mut store, "run")
                .unwrap()
                .call(&mut store, ())
                .is_err()
        );
    }
}
