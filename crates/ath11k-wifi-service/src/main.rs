// SPDX-License-Identifier: GPL-2.0-only

//! WCN6750 production process composition. Files and descriptors are adopted
//! before lockdown; VFIO/QMI/firmware/MLME activation starts only afterwards.

use ath11k_core::{
    HardwareMemoryProvider, NoWmiTrace, WCN6750, WCN6750_INTERRUPT_ROUTES, Wcn6750FirmwareAssets,
    Wcn6750Interrupts, Wcn6750QmiSession, Wcn6750Subsystems,
};
use ath11k_qmi_qrtr::QrtrTransport;
use ath11k_softmac_adapter::Ath11kClientDevice;
use drv_hardware::Device as HardwareDevice;
use drv_hardware_backends::{LinuxVfio, LinuxVfioPlatformCapabilities};
use linux_self_sandbox::{Profile, Sandbox, WCN6750_IRQ_EVENTFD_COUNT, Wcn6750Dma};
use qrtr_socket::QrtrSocket;
use std::fs::{self, File, OpenOptions};
use std::io::{Read as _, Seek as _, Write as _};
use std::os::fd::{AsRawFd as _, FromRawFd as _, OwnedFd};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use wifi_control_service::PreparedServerEndpoints;
use wlan_softmac_host::WlanSoftmac as _;
use wlan_softmac_host::runtime::{ClientRuntime, PreparedRuntimeResources};

const POLICY_FD: i32 = 3;
const SUPERVISOR_FD: i32 = 4;
const CONTROL_TIMEOUT_NS: u64 = 10_000_000_000;
const CE_POLL_NS: u64 = 10_000_000;

fn main() {
    if let Err(error) = start() {
        eprintln!("ath11k_wifi_service=REFUSED detail={error}");
        std::process::exit(1);
    }
}

fn start() -> Result<(), String> {
    let config = Config::parse()?;
    eprintln!("ath11k_wifi_startup=ENTER");
    // Adopt fixed launcher capabilities before any open can reuse a missing
    // inherited descriptor number.
    let endpoints = PreparedServerEndpoints::new(
        unsafe { OwnedFd::from_raw_fd(POLICY_FD) },
        unsafe { OwnedFd::from_raw_fd(SUPERVISOR_FD) },
        config.generation,
    )
    .map_err(|error| format!("validate inherited IPC: {error}"))?;
    eprintln!("ath11k_wifi_startup=IPC_READY");
    let [control_fd, supervisor_fd] = endpoints.fd_identities();
    if WCN6750_INTERRUPT_ROUTES.len() != WCN6750_IRQ_EVENTFD_COUNT {
        return Err("sandbox and WCN6750 interrupt inventories differ".into());
    }
    let firmware = Wcn6750FirmwareAssets::new_redwood(
        fs::read(&config.board).map_err(|error| format!("read board.bin: {error}"))?,
        fs::read(&config.regdb).map_err(|error| format!("read regdb.bin: {error}"))?,
    )
    .map_err(|error| format!("validate Redwood firmware assets: {error:?}"))?;
    eprintln!("ath11k_wifi_startup=FIRMWARE_READY");
    let platform = if config.broker {
        LinuxVfioPlatformCapabilities::open_broker(&config.vfio, WCN6750_INTERRUPT_ROUTES.len())
    } else {
        LinuxVfioPlatformCapabilities::open_coherent(&config.vfio, WCN6750_INTERRUPT_ROUTES.len())
    }
    .map_err(|error| error.to_string())?;
    eprintln!("ath11k_wifi_startup=VFIO_PREPARED");
    let platform_fds = platform.fd_identities();
    let irq_eventfds: [i32; WCN6750_IRQ_EVENTFD_COUNT] = platform_fds
        .irq_eventfds
        .clone()
        .try_into()
        .map_err(|_| "prepared WCN6750 IRQ inventory is not exactly 16")?;
    let qrtr = QrtrSocket::open().map_err(|error| format!("open AF_QIPCRTR: {error}"))?;
    eprintln!("ath11k_wifi_startup=QRTR_READY");
    let qrtr_fd = qrtr.raw_fd();
    let runtime_resources = PreparedRuntimeResources::new(config.mac)
        .map_err(|error| format!("prepare host runtime: {error}"))?;
    eprintln!("ath11k_wifi_startup=RUNTIME_RESOURCES_READY");
    let ethernet_fds = runtime_resources.fd_identities();
    let runtime_fds = runtime_resources.runtime_fd_identities().to_vec();
    let mut remoteproc_state = OpenOptions::new()
        .read(true)
        .write(true)
        .open(config.remoteproc.join("state"))
        .map_err(|error| format!("open remoteproc state: {error}"))?;
    verify_remoteproc_running(&mut remoteproc_state)?;
    eprintln!("ath11k_wifi_startup=REMOTEPROC_RUNNING");
    let remoteproc_firmware = fs::read_to_string(config.remoteproc.join("firmware"))
        .map_err(|error| format!("read remoteproc firmware identity: {error}"))?;
    if remoteproc_firmware.trim().is_empty() {
        return Err("remoteproc firmware identity is empty".into());
    }
    let remoteproc_state_fd = remoteproc_state.as_raw_fd();

    let mut retained = vec![control_fd, supervisor_fd, platform_fds.vfio, qrtr_fd];
    retained.extend(irq_eventfds);
    retained.extend(&ethernet_fds);
    retained.extend(&runtime_fds);
    retained.push(remoteproc_state_fd);
    if let Some(iommufd) = platform_fds.iommufd {
        retained.push(iommufd);
    }
    let dma = match platform_fds.iommufd {
        Some(iommufd) => Wcn6750Dma::Coherent { iommufd },
        None => Wcn6750Dma::Broker,
    };
    let diagnostic_unsandboxed = config.diagnostic_unsandboxed;
    let run = move || {
        activate_and_run(
            config,
            endpoints,
            runtime_resources,
            platform,
            qrtr,
            firmware,
            remoteproc_state,
        )
    };
    if diagnostic_unsandboxed {
        eprintln!("ath11k_wifi_service=DIAGNOSTIC_UNSANDBOXED confinement=false production=false");
        return run();
    }

    let locked = Sandbox::new()
        .setup(&retained, None)
        .map_err(|error| format!("sandbox setup: {error}"))?
        .lockdown(Profile::Ath11kWcn6750 {
            control_fd,
            supervisor_fd,
            vfio_fd: platform_fds.vfio,
            dma,
            qrtr_fd,
            irq_eventfds,
            ethernet_fds,
            runtime_fds,
            remoteproc_state_fd: Some(remoteproc_state_fd),
        })
        .map_err(|error| format!("sandbox lockdown: {error}"))?;
    locked.run(run)
}

fn activate_and_run(
    config: Config,
    endpoints: PreparedServerEndpoints,
    runtime_resources: PreparedRuntimeResources,
    platform: LinuxVfioPlatformCapabilities,
    qrtr: QrtrSocket,
    firmware: Wcn6750FirmwareAssets,
    mut remoteproc_state: File,
) -> Result<(), String> {
    eprintln!("ath11k_wifi_startup=VFIO_ACTIVATE_ENTER");
    let vfio = LinuxVfio::activate_platform(platform).map_err(|error| error.to_string())?;
    eprintln!("ath11k_wifi_startup=VFIO_ACTIVE");
    // Keep one VFIO owner outside every fallible post-activation operation.
    // The inner owners may unwind, but mappings cannot be released until WPSS
    // has synchronously reached offline below.
    let hardware_guard = HardwareDevice::from_backend(vfio);
    let operation = (|| -> Result<(), String> {
        let hardware = hardware_guard.clone();
        let (waiter, dp_interrupts) = Wcn6750Interrupts::configure_with_ce_polling(
            hardware.clone(),
            monotonic_now,
            CE_POLL_NS,
        )
        .map_err(|error| format!("configure interrupts: {error:?}"))?
        .with_ce_trace(trace_ce_runtime)
        .split();
        eprintln!("ath11k_wifi_startup=INTERRUPTS_READY");
        let memory = HardwareMemoryProvider::new(hardware.clone(), config.register_region);
        let mut qmi = Wcn6750QmiSession::new(QrtrTransport::from_socket(qrtr), firmware, memory);
        qmi.discover_device_bar()
            .map_err(|error| format!("QMI device BAR discovery: {error:?}"))?;
        eprintln!("ath11k_wifi_startup=QMI_BAR_READY");
        if qmi.memory().device_bar().is_none() {
            return Err("selected VFIO region did not map the QMI device BAR".into());
        }
        let subsystems = Wcn6750Subsystems::new(
            qmi,
            hardware,
            waiter,
            dp_interrupts,
            control_deadline as fn() -> u64,
            NoWmiTrace,
        );
        let device = WCN6750.device(subsystems);
        let regulatory = ath11k_core::redwood_india_domain();
        let mut adapter =
            Ath11kClientDevice::new(device, config.mac).with_regulatory_domain(regulatory);
        let query = adapter
            .query()
            .map_err(|status| format!("query SoftMAC: {status}"))?;
        eprintln!("ath11k_wifi_startup=SOFTMAC_QUERY_READY");
        let device_info = wlan_mlme::mlme_device_info_from_softmac(query)
            .map_err(|error| format!("convert SoftMAC query: {error}"))?;
        let security = adapter
            .query_security_support()
            .map_err(|status| format!("query security support: {status}"))?;
        let spectrum = adapter
            .query_spectrum_management_support()
            .map_err(|status| format!("query spectrum support: {status}"))?;
        let mut sme_config = wlan_sme::client::ClientConfig::default();
        sme_config.wpa3_supported = true;
        eprintln!("ath11k_wifi_startup=SOFTMAC_START_ENTER");
        let runtime = futures::executor::block_on(ClientRuntime::new_with_prepared_resources(
            adapter,
            sme_config,
            device_info,
            security,
            spectrum,
            fuchsia_inspect::Inspector::default(),
            runtime_resources,
        ))
        .map_err(|error| format!("activate pinned client runtime: {error}"))?;
        eprintln!("ath11k_wifi_startup=CLIENT_RUNTIME_READY");
        let mut server = endpoints
            .bind_runtime(runtime)
            .post_lockdown_open_complete()
            .map_err(|error| format!("open control generation: {error}"))?;
        eprintln!("ath11k_wifi_startup=CONTROL_READY");
        TRACE_CE_SEQUENCE.store(0, Ordering::Release);
        TRACE_CE_RUNTIME.store(true, Ordering::Release);
        let result = server.run_to_terminal();
        TRACE_CE_RUNTIME.store(false, Ordering::Release);
        eprintln!("ath11k_wifi_cleanup=CONTROL_TERMINAL");
        let mut runtime = server.into_runtime();
        let stop = runtime.stop();
        eprintln!(
            "ath11k_wifi_cleanup=RUNTIME_STOP_RETURNED success={}",
            stop.is_ok()
        );
        result.map_err(|error| format!("control service: {error}"))?;
        stop.map_err(|error| format!("stop physical runtime: {error}"))
    })();
    eprintln!("ath11k_wifi_cleanup=REMOTEPROC_STOP_ENTER");
    let containment = stop_and_verify_remoteproc(&mut remoteproc_state);
    if let Err(error) = containment {
        eprintln!(
            "ath11k_wifi_service=CONTAINED reason=remoteproc_quiesce_failed detail={error} manual_recovery_required=true"
        );
        loop {
            // Do not issue another syscall under the fatal production filter:
            // retaining the live runtime and VFIO mappings is safer than
            // releasing them while WPSS may still be running.
            std::hint::spin_loop();
        }
    }
    eprintln!("ath11k_wifi_cleanup=REMOTEPROC_OFFLINE");
    drop(hardware_guard);
    eprintln!("ath11k_wifi_cleanup=VFIO_GUARD_RELEASED");
    operation
}

static TRACE_CE_RUNTIME: AtomicBool = AtomicBool::new(false);
static TRACE_CE_SEQUENCE: AtomicUsize = AtomicUsize::new(0);

fn trace_ce_runtime(stage: &'static str, value: usize) {
    if TRACE_CE_RUNTIME.load(Ordering::Acquire) {
        let sequence = TRACE_CE_SEQUENCE.fetch_add(1, Ordering::Relaxed);
        if sequence < 256 {
            eprintln!("ath11k_ce_runtime sequence={sequence} stage={stage} value={value}");
        }
    }
}

fn verify_remoteproc_running(state: &mut File) -> Result<(), String> {
    state
        .rewind()
        .map_err(|error| format!("seek remoteproc state: {error}"))?;
    let mut value = String::new();
    state
        .read_to_string(&mut value)
        .map_err(|error| format!("read remoteproc state: {error}"))?;
    if value.trim() == "running" {
        Ok(())
    } else {
        Err(format!(
            "remoteproc is {:?}, expected running",
            value.trim()
        ))
    }
}

fn stop_and_verify_remoteproc(state: &mut File) -> Result<(), String> {
    state
        .rewind()
        .map_err(|error| format!("seek remoteproc state: {error}"))?;
    state
        .write_all(b"stop\n")
        .map_err(|error| format!("stop remoteproc: {error}"))?;
    let deadline = monotonic_now().saturating_add(CONTROL_TIMEOUT_NS);
    loop {
        state
            .rewind()
            .map_err(|error| format!("seek remoteproc state: {error}"))?;
        let mut value = String::new();
        state
            .read_to_string(&mut value)
            .map_err(|error| format!("verify remoteproc state: {error}"))?;
        if value.trim() == "offline" {
            return Ok(());
        }
        if monotonic_now() >= deadline {
            return Err(format!(
                "remoteproc did not become offline; last state {:?}; manual recovery required",
                value.trim()
            ));
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
}

fn monotonic_now() -> u64 {
    userspace_vfio::monotonic_time_ns().unwrap_or(0)
}
fn control_deadline() -> u64 {
    monotonic_now().saturating_add(CONTROL_TIMEOUT_NS)
}

struct Config {
    generation: [u8; 16],
    vfio: PathBuf,
    board: PathBuf,
    regdb: PathBuf,
    register_region: u8,
    mac: [u8; 6],
    remoteproc: PathBuf,
    broker: bool,
    diagnostic_unsandboxed: bool,
}

impl Config {
    fn parse() -> Result<Self, String> {
        let mut args = std::env::args().skip(1);
        let generation = parse_generation(&args.next().ok_or_else(usage)?)?;
        let vfio = args.next().map(PathBuf::from).ok_or_else(usage)?;
        let board = args.next().map(PathBuf::from).ok_or_else(usage)?;
        let regdb = args.next().map(PathBuf::from).ok_or_else(usage)?;
        let register_region = args
            .next()
            .ok_or_else(usage)?
            .parse()
            .map_err(|_| "register region must be a u8".to_string())?;
        let mac = parse_mac(&args.next().ok_or_else(usage)?)?;
        let remoteproc = args.next().map(PathBuf::from).ok_or_else(usage)?;
        let mut broker = false;
        let mut diagnostic_unsandboxed = false;
        for flag in args {
            match flag.as_str() {
                "--broker" if !broker => broker = true,
                "--diagnostic-unsandboxed" if !diagnostic_unsandboxed => {
                    diagnostic_unsandboxed = true
                }
                _ => return Err(usage()),
            }
        }
        Ok(Self {
            generation,
            vfio,
            board,
            regdb,
            register_region,
            mac,
            remoteproc,
            broker,
            diagnostic_unsandboxed,
        })
    }
}

fn usage() -> String {
    "usage: ath11k-wifi-service <generation-hex> <vfio-cdev> <board.bin> <regdb.bin> <register-region> <mac> <remoteproc-directory> [--broker] [--diagnostic-unsandboxed]".into()
}

fn parse_generation(value: &str) -> Result<[u8; 16], String> {
    parse_hex_array(value, "generation")
}
fn parse_mac(value: &str) -> Result<[u8; 6], String> {
    let compact = value.replace(':', "");
    parse_hex_array(&compact, "MAC")
}
fn parse_hex_array<const N: usize>(value: &str, name: &str) -> Result<[u8; N], String> {
    if value.len() != N * 2 || !value.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err(format!("invalid {name}"));
    }
    let mut output = [0; N];
    for (byte, pair) in output.iter_mut().zip(value.as_bytes().chunks_exact(2)) {
        *byte = u8::from_str_radix(std::str::from_utf8(pair).unwrap(), 16)
            .map_err(|_| format!("invalid {name}"))?;
    }
    Ok(output)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_opaque_generation_and_public_mac_exactly() {
        assert_eq!(
            parse_generation("00112233445566778899aabbccddeeff").unwrap(),
            [
                0x00, 0x11, 0x22, 0x33, 0x44, 0x55, 0x66, 0x77, 0x88, 0x99, 0xaa, 0xbb, 0xcc, 0xdd,
                0xee, 0xff,
            ]
        );
        assert_eq!(parse_mac("02:00:00:00:00:01").unwrap(), [2, 0, 0, 0, 0, 1]);
        assert!(parse_generation("00").is_err());
        assert!(parse_mac("02:00:00:00:00:gg").is_err());
    }
}
