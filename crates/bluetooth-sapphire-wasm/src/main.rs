use std::collections::BTreeSet;
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
use std::thread;
use std::time::{Duration, Instant};

use bluetooth_hci_broker::{MAX_OUTBOUND_HCI_PACKET, OutboundKind, validate_outbound};
use wasmtime::{Caller, Config, Engine, Extern, Linker, Module, Store};

const IPC_FD: i32 = 3;
const MAX_MODULE_BYTES: usize = 128 * 1024 * 1024;
const MAX_PACKET_BYTES: usize = 64 * 1024;
const MAX_LOG_BYTES: usize = 8 * 1024 * 1024;
const MAX_LOG_WRITE: usize = 64 * 1024;
const MAX_CONTROLLER_PACKETS: usize = 4096;
const MAX_CONTROLLER_BYTES: usize = 8 * 1024 * 1024;
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
const ACK: u8 = 128;
const CONTROLLER_MODULE: &str = "drv:bluetooth-sapphire/controller@0.1.0";

struct TestHost {
    ipc: Arc<UnixDatagram>,
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

fn request(socket: &UnixDatagram, operation: u8, payload: &[u8]) -> wasmtime::Result<Vec<u8>> {
    send_packet(socket, operation, payload)?;
    let (reply, payload) = receive_packet(socket)?;
    if reply != ACK {
        return Err(wasmtime::Error::msg("unexpected supervisor reply"));
    }
    Ok(payload)
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

fn receive_module(socket: &UnixDatagram) -> io::Result<Vec<u8>> {
    let (operation, payload) = receive_packet(socket)?;
    if operation != MODULE_LENGTH || payload.len() != 8 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "missing module length",
        ));
    }
    let length = usize::try_from(u64::from_le_bytes(payload.try_into().unwrap()))
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
    Ok(module)
}

fn run_worker(socket: Arc<UnixDatagram>) -> wasmtime::Result<i32> {
    let bytes = receive_module(&socket)?;
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
            request(&caller.data().ipc, LOG, &payload)?;
            Ok(())
        },
    )?;
    linker.func_wrap(
        "drv:test",
        "exit",
        |caller: Caller<'_, TestHost>, status: i32| -> wasmtime::Result<()> {
            request(&caller.data().ipc, EXIT, &status.to_le_bytes())?;
            Err(wasmtime::Error::msg(format!(
                "guest called proc_exit({status})"
            )))
        },
    )?;
    linker.func_wrap(
        "wasi_snapshot_preview1",
        "clock_time_get",
        |mut caller: Caller<'_, TestHost>, _clock: i32, _precision: i64, result: i32| {
            let reply = request(&caller.data().ipc, CLOCK, &[])?;
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
            let reply = request(&caller.data().ipc, CONTROLLER_SEND, &payload)?;
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
    instance
        .get_typed_func::<(), i32>(&mut store, "drv_test_entry")?
        .call(&mut store, ())
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

#[derive(Debug, Default)]
struct FakeControllerBroker {
    packets: usize,
    bytes: usize,
}

impl FakeControllerBroker {
    fn send(&mut self, payload: &[u8]) -> u8 {
        let Some((&kind, packet)) = payload.split_first() else {
            return 1;
        };
        let kind = match kind {
            0 => OutboundKind::Command,
            1 => OutboundKind::Acl,
            2 => OutboundKind::Sco,
            3 => OutboundKind::Iso,
            _ => return 1,
        };
        if validate_outbound(kind, packet).is_err() {
            return 1;
        }
        if self.packets >= MAX_CONTROLLER_PACKETS
            || self.bytes.saturating_add(packet.len()) > MAX_CONTROLLER_BYTES
        {
            return 2;
        }
        self.packets += 1;
        self.bytes += packet.len();
        0
    }
}

fn run_supervisor(path: impl AsRef<Path>) -> Result<String, Box<dyn Error>> {
    let module = fs::read(path)?;
    if module.len() > MAX_MODULE_BYTES {
        return Err("module exceeds bound".into());
    }
    let (supervisor, worker) = UnixDatagram::pair()?;
    supervisor.set_read_timeout(Some(Duration::from_secs(1)))?;
    let mut child = spawn_worker(&worker)?;
    drop(worker);

    send_packet(
        &supervisor,
        MODULE_LENGTH,
        &(module.len() as u64).to_le_bytes(),
    )?;
    for chunk in module.chunks(MAX_PACKET_BYTES - 1) {
        send_packet(&supervisor, MODULE_CHUNK, chunk)?;
    }

    let started = Instant::now();
    let mut log = Vec::new();
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
            Ok((LOG, payload)) if payload.len() >= 4 => {
                if log.len().saturating_add(payload.len() - 4) > MAX_LOG_BYTES {
                    child.kill()?;
                    return Err("Sapphire worker exceeded log quota".into());
                }
                log.extend_from_slice(&payload[4..]);
                send_packet(&supervisor, ACK, &[])?;
            }
            Ok((CLOCK, payload)) if payload.is_empty() => {
                let now = now_nanoseconds;
                now_nanoseconds = now.saturating_add(1_000_000);
                clock_requests += 1;
                send_packet(&supervisor, ACK, &now.to_le_bytes())?;
            }
            Ok((PROGRESS, payload)) if payload.len() <= 64 && payload.is_ascii() => {
                log.extend_from_slice(b"[worker: ");
                log.extend_from_slice(&payload);
                log.extend_from_slice(b"]\n");
            }
            Ok((CONTROLLER_SEND, payload)) => {
                send_packet(&supervisor, ACK, &[controller.send(&payload)])?;
            }
            Ok((EXIT, payload)) if payload.len() == 4 => {
                send_packet(&supervisor, ACK, &[])?;
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
                    (func (param i32 i64 i32) (result i32)))
                (import "{CONTROLLER_MODULE}" "send"
                    (func $send (param i32 i32 i32) (result i32)))
                (memory (export "memory") 1)
                (data (i32.const 0) "\0c\20\01\01")
                (func (export "_initialize"))
                (func (export "drv_test_entry") (result i32)
                    i32.const 0
                    i32.const 0
                    i32.const 4
                    call $send))"#
        ))
        .unwrap();
        let (supervisor, worker) = UnixDatagram::pair().unwrap();
        supervisor
            .set_read_timeout(Some(Duration::from_secs(2)))
            .unwrap();
        let worker = Arc::new(worker);
        let worker_thread = thread::spawn(move || run_worker(worker).unwrap());

        send_packet(
            &supervisor,
            MODULE_LENGTH,
            &(module.len() as u64).to_le_bytes(),
        )
        .unwrap();
        for chunk in module.chunks(MAX_PACKET_BYTES - 1) {
            send_packet(&supervisor, MODULE_CHUNK, chunk).unwrap();
        }

        let mut progress = 0;
        loop {
            match receive_packet(&supervisor).unwrap() {
                (PROGRESS, _) => progress += 1,
                (CONTROLLER_SEND, payload) => {
                    assert_eq!(payload, [0, 0x0c, 0x20, 1, 1]);
                    send_packet(&supervisor, ACK, &[0]).unwrap();
                    break;
                }
                packet => panic!("unexpected worker packet: {packet:?}"),
            }
        }
        assert_eq!(progress, 4);
        assert_eq!(worker_thread.join().unwrap(), 0);
    }

    #[test]
    fn fake_controller_broker_bounds_and_validates_packets() {
        let mut broker = FakeControllerBroker::default();
        assert_eq!(broker.send(&[0, 0x0c, 0x20, 1, 1]), 0);
        assert_eq!(broker.packets, 1);
        assert_eq!(broker.bytes, 4);
        assert_eq!(broker.send(&[0, 0x0c, 0x20, 2, 1]), 1);
        assert_eq!(broker.send(&[4, 0, 0, 0]), 1);
        broker.packets = MAX_CONTROLLER_PACKETS;
        assert_eq!(broker.send(&[2, 1, 0, 1, 0xaa]), 2);
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
