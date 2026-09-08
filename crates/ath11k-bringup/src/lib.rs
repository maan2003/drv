#![forbid(unsafe_code)]
#![cfg_attr(not(target_os = "linux"), allow(dead_code))]

#[cfg(not(target_os = "linux"))]
compile_error!("ath11k-bringup is Linux-only");

use ath11k_qmi_qrtr::QrtrTransport;
use drv_hardware::Device as HardwareDevice;
use drv_hardware_backends::LinuxVfio;
use sha2::{Digest, Sha256};
use std::fmt;
use std::fs;
use std::fs::File;
use std::io::{self, BufWriter, Write};
use std::path::{Path, PathBuf};
use std::str::FromStr;
use std::time::Instant;

pub const DEFAULT_BOARD: &str = "/run/current-system/firmware/ath11k/WCN6750/hw1.0/board.bin";
pub const DEFAULT_REGDB: &str = "/run/current-system/firmware/ath11k/WCN6750/hw1.0/regdb.bin";
pub const BOARD_BYTES: usize = 59_924;
pub const REGDB_BYTES: usize = 24_278;
const BOARD_SHA256: [u8; 32] = [
    0xb4, 0x5a, 0x60, 0xf0, 0x7e, 0x4c, 0x83, 0x8b, 0x6f, 0x52, 0x2a, 0xb5, 0x87, 0x22, 0x9c, 0x4a,
    0x9f, 0xee, 0xfd, 0x53, 0xe7, 0xe0, 0x40, 0xbd, 0xe1, 0x82, 0x0a, 0x9e, 0x94, 0x06, 0xbf, 0xd1,
];
const REGDB_SHA256: [u8; 32] = [
    0x2f, 0xe6, 0xb7, 0x9e, 0x6d, 0x36, 0xe1, 0x90, 0xf3, 0x9e, 0x89, 0x16, 0xbe, 0xe5, 0xae, 0x4c,
    0xf9, 0x7a, 0xb5, 0xe7, 0x15, 0x19, 0xaf, 0x5a, 0xf8, 0x92, 0x0b, 0xb7, 0x57, 0x74, 0x0d, 0xd9,
];

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub enum Stage {
    Resources,
    Firmware,
    Qmi,
    Core,
    PassiveScan,
}

impl Stage {
    pub const ALL: [Self; 5] = [
        Self::Resources,
        Self::Firmware,
        Self::Qmi,
        Self::Core,
        Self::PassiveScan,
    ];

    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Resources => "resources",
            Self::Firmware => "firmware",
            Self::Qmi => "qmi",
            Self::Core => "core",
            Self::PassiveScan => "passive-scan",
        }
    }
}

impl FromStr for Stage {
    type Err = String;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        Self::ALL
            .into_iter()
            .find(|stage| stage.as_str() == value)
            .ok_or_else(|| format!("unknown stage {value:?}; expected resources, firmware, qmi, core, or passive-scan"))
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Cli {
    pub dry_run: bool,
    pub stop_after: Stage,
    pub coherent: bool,
    pub vfio_device: Option<PathBuf>,
    pub board: PathBuf,
    pub regdb: PathBuf,
    pub wmi_log: Option<PathBuf>,
}

impl Default for Cli {
    fn default() -> Self {
        Self {
            dry_run: false,
            stop_after: Stage::PassiveScan,
            coherent: false,
            vfio_device: None,
            board: DEFAULT_BOARD.into(),
            regdb: DEFAULT_REGDB.into(),
            wmi_log: Some("ath11k-wmi-run.jsonl".into()),
        }
    }
}

impl Cli {
    pub fn parse<I, S>(arguments: I) -> Result<Self, String>
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        let mut cli = Self::default();
        let mut arguments = arguments.into_iter().map(Into::into);
        while let Some(argument) = arguments.next() {
            let value = |name: &str, arguments: &mut dyn Iterator<Item = String>| {
                arguments
                    .next()
                    .ok_or_else(|| format!("{name} requires a value"))
            };
            match argument.as_str() {
                "--dry-run" => cli.dry_run = true,
                "--coherent" => cli.coherent = true,
                "--stop-after" => {
                    cli.stop_after = value("--stop-after", &mut arguments)?.parse()?
                }
                "--vfio-device" => {
                    cli.vfio_device = Some(value("--vfio-device", &mut arguments)?.into())
                }
                "--board" => cli.board = value("--board", &mut arguments)?.into(),
                "--regdb" => cli.regdb = value("--regdb", &mut arguments)?.into(),
                "--wmi-log" => cli.wmi_log = Some(value("--wmi-log", &mut arguments)?.into()),
                "-h" | "--help" => return Err(usage().into()),
                _ => return Err(format!("unknown argument {argument:?}\n{}", usage())),
            }
        }
        if cli.dry_run && cli.coherent {
            return Err("--coherent has no effect with --dry-run".into());
        }
        if !cli.dry_run && cli.vfio_device.is_none() {
            return Err(format!(
                "real mode requires --vfio-device <path>\n{}",
                usage()
            ));
        }
        Ok(cli)
    }
}

pub const fn usage() -> &'static str {
    "usage: ath11k-bringup [--dry-run] [--stop-after <resources|firmware|qmi|core|passive-scan>] [--vfio-device <path>] [--board <path>] [--regdb <path>] [--wmi-log <path>] [--coherent]"
}

#[derive(Debug)]
pub enum Error {
    Io {
        action: &'static str,
        source: io::Error,
    },
    Hardware(String),
    Qmi(ath11k_qmi::QmiError),
    Core(ath11k_core::CoreError),
    InvalidAsset(&'static str),
    Unsupported(&'static str),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io { action, source } => write!(f, "{action}: {source}"),
            Self::Hardware(error) => write!(f, "hardware resource acquisition failed: {error}"),
            Self::Qmi(error) => write!(f, "QMI service initialization failed: {error:?}"),
            Self::Core(error) => write!(f, "ath11k core lifecycle failed: {error:?}"),
            Self::InvalidAsset(message) => write!(f, "firmware asset validation failed: {message}"),
            Self::Unsupported(message) => write!(f, "unsupported: {message}"),
        }
    }
}

impl std::error::Error for Error {}

pub trait Host {
    fn resources(&mut self, config: &Cli) -> Result<(), Error>;
    fn firmware(&mut self, board: &Path, regdb: &Path) -> Result<(), Error>;
    fn qmi(&mut self) -> Result<(), Error>;
    fn core(&mut self) -> Result<(), Error>;
    fn passive_scan(&mut self) -> Result<(), Error>;
}

pub fn run(config: &Cli, host: &mut dyn Host) -> Result<Vec<Stage>, Error> {
    let mut completed = Vec::new();
    for stage in Stage::ALL {
        match stage {
            Stage::Resources => host.resources(config)?,
            Stage::Firmware => host.firmware(&config.board, &config.regdb)?,
            Stage::Qmi => host.qmi()?,
            Stage::Core => host.core()?,
            Stage::PassiveScan => host.passive_scan()?,
        }
        completed.push(stage);
        if stage == config.stop_after {
            break;
        }
    }
    Ok(completed)
}

pub struct DryRunHost {
    device: Option<ath11k_core::Device<ath11k_core::ModelSubsystems>>,
    vdev: Option<ath11k_core::VdevId>,
}

impl Default for DryRunHost {
    fn default() -> Self {
        Self {
            device: Some(ath11k_core::WCN6750.device(Default::default())),
            vdev: None,
        }
    }
}

impl Host for DryRunHost {
    fn resources(&mut self, _: &Cli) -> Result<(), Error> {
        Ok(())
    }
    fn firmware(&mut self, _: &Path, _: &Path) -> Result<(), Error> {
        Ok(())
    }
    fn qmi(&mut self) -> Result<(), Error> {
        use ath11k_core::Lifecycle as _;
        self.device.as_mut().unwrap().probe().map_err(Error::Core)
    }
    fn core(&mut self) -> Result<(), Error> {
        use ath11k_core::{Lifecycle as _, RadioControl as _};
        let device = self.device.as_mut().unwrap();
        device.attach_firmware().map_err(Error::Core)?;
        device.start_radio().map_err(Error::Core)?;
        self.vdev = Some(
            device
                .create_client_vdev([0x02, 0, 0, 0, 0, 1])
                .map_err(Error::Core)?,
        );
        Ok(())
    }
    fn passive_scan(&mut self) -> Result<(), Error> {
        use ath11k_core::ClientRadioControl as _;
        self.device
            .as_mut()
            .unwrap()
            .start_scan(ath11k_core::ScanConfig {
                vdev: self
                    .vdev
                    .ok_or(Error::Unsupported("scan requested before vdev creation"))?,
                id: ath11k_core::ScanId(1),
                active: false,
                channels_mhz: vec![2412, 2437, 2462],
                ssids: Vec::new(),
            })
            .map_err(Error::Core)
    }
}

struct JsonTrace(WmiJsonl<BufWriter<File>>);

impl ath11k_core::WmiTraceSink for JsonTrace {
    fn record(
        &mut self,
        command: bool,
        id: u32,
        bytes: &[u8],
    ) -> Result<(), ath11k_core::CoreError> {
        self.0
            .record(
                if command {
                    WmiKind::Command
                } else {
                    WmiKind::Event
                },
                id,
                bytes,
            )
            .map_err(|_| ath11k_core::CoreError::DeviceFault)
    }
}

fn control_deadline() -> u64 {
    userspace_vfio::monotonic_time_ns()
        .unwrap_or(0)
        .saturating_add(10_000_000_000)
}

type LiveSubsystems = ath11k_core::Wcn6750Subsystems<
    LinuxVfio,
    QrtrTransport,
    ath11k_core::Wcn6750FirmwareAssets,
    ath11k_core::HardwareMemoryProvider<LinuxVfio>,
    ath11k_core::Wcn6750CeWaiter<LinuxVfio>,
    fn() -> u64,
    JsonTrace,
>;
type LiveDevice = ath11k_core::Device<LiveSubsystems>;

#[derive(Default)]
pub struct RealHost {
    hardware: Option<HardwareDevice<LinuxVfio>>,
    waiter: Option<ath11k_core::Wcn6750CeWaiter<LinuxVfio>>,
    dp_interrupts: Option<ath11k_core::Wcn6750DpInterrupts<LinuxVfio>>,
    qrtr: Option<QrtrTransport>,
    firmware: Option<ath11k_core::Wcn6750FirmwareAssets>,
    wmi_log: Option<PathBuf>,
    device: Option<LiveDevice>,
    vdev: Option<ath11k_core::VdevId>,
}

impl Host for RealHost {
    fn resources(&mut self, config: &Cli) -> Result<(), Error> {
        let vfio = if config.coherent {
            LinuxVfio::open_coherent(config.vfio_device.as_ref().ok_or(Error::Unsupported(
                "real mode requires an explicit VFIO cdev path",
            ))?)
        } else {
            LinuxVfio::open_broker(config.vfio_device.as_ref().ok_or(Error::Unsupported(
                "real mode requires an explicit VFIO cdev path",
            ))?)
        }
        .map_err(|error| Error::Hardware(error.to_string()))?;
        // Open QRTR only after exclusive VFIO acquisition succeeded (fail closed).
        let qrtr = QrtrTransport::open().map_err(|source| Error::Io {
            action: "open AF_QIPCRTR socket",
            source,
        })?;
        let hardware = HardwareDevice::from_backend(vfio);
        let (waiter, dp_interrupts) = ath11k_core::Wcn6750Interrupts::configure(hardware.clone())
            .map_err(|error| Error::Hardware(format!("configure WCN6750 interrupts: {error:?}")))?
            .split();
        self.hardware = Some(hardware);
        self.waiter = Some(waiter);
        self.dp_interrupts = Some(dp_interrupts);
        self.qrtr = Some(qrtr);
        self.wmi_log = config.wmi_log.clone();
        Ok(())
    }

    fn firmware(&mut self, board: &Path, regdb: &Path) -> Result<(), Error> {
        let board = fs::read(board).map_err(|source| Error::Io {
            action: "read selected board.bin",
            source,
        })?;
        let regdb = fs::read(regdb).map_err(|source| Error::Io {
            action: "read regdb.bin",
            source,
        })?;
        validate_asset("board.bin", &board, BOARD_BYTES, BOARD_SHA256)?;
        validate_asset("regdb.bin", &regdb, REGDB_BYTES, REGDB_SHA256)?;
        self.firmware = Some(ath11k_core::Wcn6750FirmwareAssets {
            board,
            calibration: None,
            regulatory: Some(regdb),
            m3: None,
        });
        Ok(())
    }

    fn qmi(&mut self) -> Result<(), Error> {
        use ath11k_core::Lifecycle as _;

        let transport = self.qrtr.take().ok_or(Error::Unsupported(
            "QMI requested before resource acquisition",
        ))?;
        let hardware = self
            .hardware
            .take()
            .ok_or(Error::Unsupported("QMI requested before VFIO acquisition"))?;
        let waiter = self.waiter.take().ok_or(Error::Unsupported(
            "QMI requested before interrupt acquisition",
        ))?;
        let dp_interrupts = self.dp_interrupts.take().ok_or(Error::Unsupported(
            "QMI requested before DP interrupt acquisition",
        ))?;
        let assets = self
            .firmware
            .take()
            .ok_or(Error::Unsupported("QMI requested before firmware loading"))?;
        let path = self.wmi_log.as_ref().ok_or(Error::Unsupported(
            "real mode requires a WMI run-record destination",
        ))?;
        let file = File::create(path).map_err(|source| Error::Io {
            action: "create WMI JSONL run record",
            source,
        })?;
        let memory = ath11k_core::HardwareMemoryProvider::new(hardware.clone());
        let qmi = ath11k_core::Wcn6750QmiSession::new(transport, assets, memory);
        let subsystems = ath11k_core::Wcn6750Subsystems::new(
            qmi,
            hardware,
            waiter,
            dp_interrupts,
            control_deadline as fn() -> u64,
            JsonTrace(WmiJsonl::new(BufWriter::new(file))),
        );
        let mut device = ath11k_core::WCN6750.device(subsystems);
        device.probe().map_err(Error::Core)?;
        self.device = Some(device);
        Ok(())
    }

    fn core(&mut self) -> Result<(), Error> {
        use ath11k_core::{Lifecycle as _, RadioControl as _};

        let device = self.device.as_mut().ok_or(Error::Unsupported(
            "core requested before QMI initialization",
        ))?;
        device.attach_firmware().map_err(Error::Core)?;
        device.start_radio().map_err(Error::Core)?;
        self.vdev = Some(
            device
                .create_client_vdev([0x02, 0, 0, 0, 0, 1])
                .map_err(Error::Core)?,
        );
        Ok(())
    }

    fn passive_scan(&mut self) -> Result<(), Error> {
        use ath11k_core::ClientRadioControl as _;

        self.device
            .as_mut()
            .ok_or(Error::Unsupported("scan requested before core startup"))?
            .start_scan(ath11k_core::ScanConfig {
                vdev: self
                    .vdev
                    .ok_or(Error::Unsupported("scan requested before vdev creation"))?,
                id: ath11k_core::ScanId(1),
                active: false,
                channels_mhz: vec![2412, 2437, 2462],
                ssids: Vec::new(),
            })
            .map_err(Error::Core)
    }
}

impl Drop for RealHost {
    fn drop(&mut self) {
        use ath11k_core::Lifecycle as _;

        if let Some(device) = self.device.as_mut()
            && matches!(
                device.state(),
                ath11k_core::DeviceState::Ready | ath11k_core::DeviceState::Recovering
            )
        {
            let _ = device.stop();
        }
    }
}

fn validate_asset(
    name: &'static str,
    bytes: &[u8],
    expected_len: usize,
    expected_sha256: [u8; 32],
) -> Result<(), Error> {
    if bytes.len() != expected_len {
        return Err(Error::InvalidAsset(match name {
            "board.bin" => "board.bin length is not the pinned Redwood m20in asset",
            _ => "regdb.bin length is not the pinned Redwood asset",
        }));
    }
    if Sha256::digest(bytes).as_slice() != expected_sha256 {
        return Err(Error::InvalidAsset(match name {
            "board.bin" => "board.bin SHA-256 is not the pinned Redwood m20in asset",
            _ => "regdb.bin SHA-256 is not the pinned Redwood asset",
        }));
    }
    Ok(())
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum WmiKind {
    Command,
    Event,
}

impl WmiKind {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Command => "wmi_cmd",
            Self::Event => "wmi_event",
        }
    }
}

pub struct WmiJsonl<W> {
    writer: W,
    next_seq: u64,
    epoch: Instant,
}

impl<W: Write> WmiJsonl<W> {
    pub fn new(writer: W) -> Self {
        Self {
            writer,
            next_seq: 0,
            epoch: Instant::now(),
        }
    }

    pub fn record(&mut self, kind: WmiKind, id: u32, bytes: &[u8]) -> io::Result<()> {
        let timestamp = u64::try_from(self.epoch.elapsed().as_nanos()).unwrap_or(u64::MAX);
        self.record_at(timestamp, kind, id, bytes)
    }

    pub fn record_at(
        &mut self,
        ts_ns: u64,
        kind: WmiKind,
        id: u32,
        bytes: &[u8],
    ) -> io::Result<()> {
        let seq = self.next_seq;
        self.next_seq = self
            .next_seq
            .checked_add(1)
            .ok_or_else(|| io::Error::other("WMI sequence exhausted"))?;
        write!(
            self.writer,
            "{{\"seq\":{seq},\"ts_ns\":{ts_ns},\"kind\":\"{}\",\"id\":{id},\"len\":{},\"bytes_hex\":\"",
            kind.as_str(),
            bytes.len()
        )?;
        for byte in bytes {
            write!(self.writer, "{byte:02x}")?;
        }
        writeln!(self.writer, "\"}}")
    }

    pub fn into_inner(self) -> W {
        self.writer
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Default)]
    struct Fake {
        visited: Vec<Stage>,
        fail: Option<Stage>,
    }
    impl Fake {
        fn visit(&mut self, stage: Stage) -> Result<(), Error> {
            self.visited.push(stage);
            if self.fail == Some(stage) {
                Err(Error::Unsupported("fake failure"))
            } else {
                Ok(())
            }
        }
    }
    impl Host for Fake {
        fn resources(&mut self, _: &Cli) -> Result<(), Error> {
            self.visit(Stage::Resources)
        }
        fn firmware(&mut self, _: &Path, _: &Path) -> Result<(), Error> {
            self.visit(Stage::Firmware)
        }
        fn qmi(&mut self) -> Result<(), Error> {
            self.visit(Stage::Qmi)
        }
        fn core(&mut self) -> Result<(), Error> {
            self.visit(Stage::Core)
        }
        fn passive_scan(&mut self) -> Result<(), Error> {
            self.visit(Stage::PassiveScan)
        }
    }

    #[test]
    fn parses_cli_and_broker_is_default() {
        let cli = Cli::parse(["--dry-run", "--stop-after", "qmi", "--board", "/b"]).unwrap();
        assert!(cli.dry_run);
        assert!(!cli.coherent);
        assert_eq!(cli.stop_after, Stage::Qmi);
        assert_eq!(cli.board, PathBuf::from("/b"));
        assert_eq!(cli.regdb, PathBuf::from(DEFAULT_REGDB));
        assert!(Cli::parse(["--stop-after", "unknown"]).is_err());
        assert!(Cli::parse([] as [&str; 0]).is_err());
        assert!(Cli::parse(["--dry-run", "--coherent"]).is_err());
    }

    #[test]
    fn stop_after_is_a_hard_stage_gate() {
        let mut host = Fake::default();
        let cli = Cli {
            stop_after: Stage::Firmware,
            ..Cli::default()
        };
        assert_eq!(
            run(&cli, &mut host).unwrap(),
            vec![Stage::Resources, Stage::Firmware]
        );
        assert_eq!(host.visited, vec![Stage::Resources, Stage::Firmware]);
    }

    #[test]
    fn failure_prevents_later_stages() {
        let mut host = Fake {
            fail: Some(Stage::Qmi),
            ..Fake::default()
        };
        assert!(run(&Cli::default(), &mut host).is_err());
        assert_eq!(
            host.visited,
            vec![Stage::Resources, Stage::Firmware, Stage::Qmi]
        );
    }

    #[test]
    fn jsonl_has_one_global_order_and_exact_schema() {
        let mut log = WmiJsonl::new(Vec::new());
        log.record_at(9, WmiKind::Command, 17, &[0, 0xaf]).unwrap();
        log.record_at(10, WmiKind::Event, 18, &[]).unwrap();
        assert_eq!(
            String::from_utf8(log.into_inner()).unwrap(),
            "{\"seq\":0,\"ts_ns\":9,\"kind\":\"wmi_cmd\",\"id\":17,\"len\":2,\"bytes_hex\":\"00af\"}\n{\"seq\":1,\"ts_ns\":10,\"kind\":\"wmi_event\",\"id\":18,\"len\":0,\"bytes_hex\":\"\"}\n"
        );
    }

    #[test]
    fn dry_run_reaches_passive_scan_without_paths_or_hardware() {
        let cli = Cli {
            dry_run: true,
            vfio_device: Some("/does/not/exist".into()),
            board: "/does/not/exist".into(),
            regdb: "/does/not/exist".into(),
            ..Cli::default()
        };
        let mut host = DryRunHost::default();
        let completed = run(&cli, &mut host).unwrap();
        assert_eq!(completed.last(), Some(&Stage::PassiveScan));
    }
}
