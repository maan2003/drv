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
use std::fs::{File, OpenOptions};
use std::io::{self, BufWriter, Write};
use std::os::unix::fs::{FileTypeExt, MetadataExt};
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
const EXPECTED_VFIO_DEVICE: &str = "17a10040.wifi";
const EXPECTED_VFIO_DRIVER: &str = "vfio-platform";
const EXPECTED_WATCHDOG_DRIVER: &str = "qcom_wdt";
const WATCHDOG_DEVICE: &str = "/dev/watchdog";
const WATCHDOG_CLASS: &str = "/sys/class/watchdog/watchdog0";
const WATCHDOG_MISC_CLASS: &str = "/sys/class/misc/watchdog";
const IOMMU_DEVICE: &str = "/dev/iommu";
const IOMMU_CLASS: &str = "/sys/class/misc/iommu";

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub enum Stage {
    Resources,
    Firmware,
    Qmi,
    Core,
    PassiveScan,
    ScanResults,
    DpPoll,
}

impl Stage {
    pub const ALL: [Self; 7] = [
        Self::Resources,
        Self::Firmware,
        Self::Qmi,
        Self::Core,
        Self::PassiveScan,
        Self::ScanResults,
        Self::DpPoll,
    ];

    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Resources => "resources",
            Self::Firmware => "firmware",
            Self::Qmi => "qmi",
            Self::Core => "core",
            Self::PassiveScan => "passive-scan",
            Self::ScanResults => "scan-results",
            Self::DpPoll => "dp-poll",
        }
    }
}

impl FromStr for Stage {
    type Err = String;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        Self::ALL
            .into_iter()
            .find(|stage| stage.as_str() == value)
            .ok_or_else(|| format!("unknown stage {value:?}; expected resources, firmware, qmi, core, passive-scan, scan-results, or dp-poll"))
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Cli {
    pub preflight: bool,
    pub dry_run: bool,
    pub stop_after: Stage,
    pub broker: bool,
    pub vfio_device: Option<PathBuf>,
    pub board: PathBuf,
    pub regdb: PathBuf,
    pub wmi_log: Option<PathBuf>,
    pub ssid: Option<Vec<u8>>,
}

impl Default for Cli {
    fn default() -> Self {
        Self {
            preflight: false,
            dry_run: false,
            stop_after: Stage::DpPoll,
            broker: false,
            vfio_device: None,
            board: DEFAULT_BOARD.into(),
            regdb: DEFAULT_REGDB.into(),
            wmi_log: Some("ath11k-wmi-run.jsonl".into()),
            ssid: None,
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
                "preflight" => cli.preflight = true,
                "--dry-run" => cli.dry_run = true,
                "--coherent" => {
                    return Err("--coherent is now the default; omit it or use --broker".into());
                }
                "--broker" => cli.broker = true,
                "--stop-after" => {
                    cli.stop_after = value("--stop-after", &mut arguments)?.parse()?
                }
                "--vfio-device" => {
                    cli.vfio_device = Some(value("--vfio-device", &mut arguments)?.into())
                }
                "--board" => cli.board = value("--board", &mut arguments)?.into(),
                "--regdb" => cli.regdb = value("--regdb", &mut arguments)?.into(),
                "--wmi-log" => cli.wmi_log = Some(value("--wmi-log", &mut arguments)?.into()),
                "--ssid" => cli.ssid = Some(value("--ssid", &mut arguments)?.into_bytes()),
                "-h" | "--help" => return Err(usage().into()),
                _ => return Err(format!("unknown argument {argument:?}\n{}", usage())),
            }
        }
        if cli.preflight && (cli.dry_run || cli.broker) {
            return Err("preflight cannot be combined with --dry-run or --broker".into());
        }
        if cli.dry_run && cli.broker {
            return Err("--broker has no effect with --dry-run".into());
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
    "usage: ath11k-bringup [preflight] [--dry-run] [--stop-after <resources|firmware|qmi|core|passive-scan|scan-results|dp-poll>] [--ssid <name>] [--vfio-device <path>] [--board <path>] [--regdb <path>] [--wmi-log <path>] [--broker]"
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
    SsidNotFound(Vec<u8>),
    Preflight(String),
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
            Self::SsidNotFound(ssid) => {
                write!(f, "no BSS matched SSID {:?}", String::from_utf8_lossy(ssid))
            }
            Self::Preflight(message) => write!(f, "preflight failed: {message}"),
        }
    }
}

impl std::error::Error for Error {}

fn trimmed_file(path: impl AsRef<Path>) -> Result<String, Error> {
    let bytes = fs::read(path).map_err(|source| Error::Io {
        action: "read preflight identity",
        source,
    })?;
    Ok(String::from_utf8_lossy(&bytes)
        .trim_end_matches(['\0', '\n', '\r'])
        .to_owned())
}

fn symlink_basename(path: impl AsRef<Path>) -> Result<String, Error> {
    let target = fs::canonicalize(path).map_err(|source| Error::Io {
        action: "resolve preflight sysfs link",
        source,
    })?;
    target
        .file_name()
        .and_then(|name| name.to_str())
        .map(str::to_owned)
        .ok_or_else(|| Error::Preflight("sysfs link has no UTF-8 basename".into()))
}

fn device_number(device: u64) -> (u64, u64) {
    let major = ((device >> 8) & 0xfff) | ((device >> 32) & 0xffff_f000);
    let minor = (device & 0xff) | ((device >> 12) & 0xffff_ff00);
    (major, minor)
}

fn parse_device_number(value: &str) -> Result<(u64, u64), Error> {
    let (major, minor) = value
        .split_once(':')
        .ok_or_else(|| Error::Preflight(format!("invalid sysfs dev value {value:?}")))?;
    Ok((
        major
            .parse()
            .map_err(|_| Error::Preflight(format!("invalid sysfs dev major {major:?}")))?,
        minor
            .parse()
            .map_err(|_| Error::Preflight(format!("invalid sysfs dev minor {minor:?}")))?,
    ))
}

fn require_mapped_char_device(path: &Path, sysfs_dev: &Path) -> Result<(), Error> {
    let metadata = fs::metadata(path).map_err(|source| Error::Io {
        action: "stat preflight character device",
        source,
    })?;
    if !metadata.file_type().is_char_device() {
        return Err(Error::Preflight(format!(
            "{} is not a character device",
            path.display()
        )));
    }
    let expected = parse_device_number(&trimmed_file(sysfs_dev)?)?;
    if device_number(metadata.rdev()) != expected {
        return Err(Error::Preflight(format!(
            "{} rdev does not match {}",
            path.display(),
            sysfs_dev.display()
        )));
    }
    Ok(())
}

fn valid_vfio_cdev_name(name: &str) -> bool {
    name.strip_prefix("vfio").is_some_and(|suffix| {
        !suffix.is_empty() && suffix.bytes().all(|byte| byte.is_ascii_digit())
    })
}

fn interrupt_facts(bytes: &[u8]) -> Result<(usize, bool), Error> {
    const GIC_CELLS: usize = 3;
    const CELL_BYTES: usize = 4;
    const SPI: u32 = 0;
    const EDGE_RISING: u32 = 1;
    let tuple_bytes = GIC_CELLS * CELL_BYTES;
    if !bytes.len().is_multiple_of(tuple_bytes) {
        return Err(Error::Preflight(format!(
            "Wi-Fi FDT interrupts has {} bytes, not 3-cell GIC tuples",
            bytes.len()
        )));
    }
    let interrupts = bytes
        .chunks_exact(tuple_bytes)
        .map(|tuple| {
            let kind = u32::from_be_bytes(tuple[0..4].try_into().unwrap());
            let flags = u32::from_be_bytes(tuple[8..12].try_into().unwrap());
            (kind, flags)
        })
        .collect::<Vec<_>>();
    let all_edge_rising = interrupts
        .iter()
        .all(|&(kind, flags)| kind == SPI && flags & 0xf == EDGE_RISING);
    Ok((interrupts.len(), all_edge_rising))
}

#[derive(Clone, Copy)]
struct PreflightIdentity<'a> {
    device_name: &'a str,
    driver: &'a str,
    watchdog: &'a str,
    watchdog_driver: &'a str,
    watchdog_node: &'a str,
    watchdog_status: &'a str,
    irq_count: usize,
    irqs_edge_rising: bool,
}

fn validate_preflight_identity(facts: PreflightIdentity<'_>) -> Result<(), Error> {
    if facts.device_name != EXPECTED_VFIO_DEVICE {
        return Err(Error::Preflight(format!(
            "expected VFIO device {EXPECTED_VFIO_DEVICE}, found {}",
            facts.device_name
        )));
    }
    if facts.driver != EXPECTED_VFIO_DRIVER {
        return Err(Error::Preflight(format!(
            "expected sole driver {EXPECTED_VFIO_DRIVER}, found {}",
            facts.driver
        )));
    }
    if facts.watchdog != EXPECTED_WATCHDOG_DRIVER {
        return Err(Error::Preflight(format!(
            "expected watchdog driver {EXPECTED_WATCHDOG_DRIVER}, found {}",
            facts.watchdog
        )));
    }
    if facts.watchdog_driver != EXPECTED_WATCHDOG_DRIVER {
        return Err(Error::Preflight(format!(
            "expected watchdog platform driver {EXPECTED_WATCHDOG_DRIVER}, found {}",
            facts.watchdog_driver
        )));
    }
    if facts.watchdog_node != "watchdog@17c10000" {
        return Err(Error::Preflight(format!(
            "expected watchdog FDT node watchdog@17c10000, found {}",
            facts.watchdog_node
        )));
    }
    if facts.watchdog_status != "okay" && facts.watchdog_status != "ok" {
        return Err(Error::Preflight(format!(
            "watchdog FDT status is {:?}, not okay",
            facts.watchdog_status
        )));
    }
    if facts.irq_count != 32 {
        return Err(Error::Preflight(format!(
            "expected 32 Wi-Fi SPI interrupts, found {}",
            facts.irq_count
        )));
    }
    if !facts.irqs_edge_rising {
        return Err(Error::Preflight(
            "Wi-Fi FDT interrupts are not all SPI EDGE_RISING".into(),
        ));
    }
    Ok(())
}

/// Run the inert host-resource gate. This does not open the VFIO or watchdog
/// cdev and never binds, maps, resets, or accesses the Wi-Fi device.
pub fn preflight(config: &Cli) -> Result<Vec<String>, Error> {
    let path = config
        .vfio_device
        .as_ref()
        .ok_or(Error::Unsupported("preflight requires --vfio-device"))?;
    let cdev_name = path
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| Error::Preflight("VFIO cdev path has no UTF-8 basename".into()))?;
    if !valid_vfio_cdev_name(cdev_name) {
        return Err(Error::Preflight(format!(
            "VFIO cdev basename {cdev_name:?} is not vfio followed by digits"
        )));
    }
    let class = PathBuf::from("/sys/class/vfio-dev").join(cdev_name);
    let class_device = class.join("device");
    require_mapped_char_device(path, &class.join("dev"))?;
    let physical_device = fs::canonicalize(&class_device).map_err(|source| Error::Io {
        action: "resolve VFIO platform device",
        source,
    })?;
    let expected_device = fs::canonicalize(format!(
        "/sys/bus/platform/devices/{EXPECTED_VFIO_DEVICE}"
    ))
    .map_err(|source| Error::Io {
        action: "resolve expected Wi-Fi platform device",
        source,
    })?;
    if physical_device != expected_device {
        return Err(Error::Preflight(format!(
            "VFIO class device resolves to {}, not {}",
            physical_device.display(),
            expected_device.display()
        )));
    }
    let device_name = symlink_basename(&class_device)?;
    let driver = symlink_basename(class_device.join("driver"))?;
    let interrupts =
        fs::read(class_device.join("of_node/interrupts")).map_err(|source| Error::Io {
            action: "read Wi-Fi FDT interrupts",
            source,
        })?;
    let (irq_count, irqs_edge_rising) = interrupt_facts(&interrupts)?;
    let unsafe_noiommu = Path::new("/sys/module/vfio/parameters/enable_unsafe_noiommu_mode");
    if unsafe_noiommu.exists() && trimmed_file(unsafe_noiommu)? != "N" {
        return Err(Error::Preflight(
            "VFIO unsafe no-IOMMU mode is enabled".into(),
        ));
    }

    require_mapped_char_device(
        Path::new(WATCHDOG_DEVICE),
        &Path::new(WATCHDOG_MISC_CLASS).join("dev"),
    )?;
    let watchdog_device =
        fs::canonicalize(Path::new(WATCHDOG_CLASS).join("device")).map_err(|source| Error::Io {
            action: "resolve watchdog0 platform device",
            source,
        })?;
    let misc_watchdog_device = fs::canonicalize(Path::new(WATCHDOG_MISC_CLASS).join("device"))
        .map_err(|source| Error::Io {
            action: "resolve legacy watchdog platform device",
            source,
        })?;
    if watchdog_device != misc_watchdog_device {
        return Err(Error::Preflight(format!(
            "{WATCHDOG_DEVICE} and watchdog0 do not map to the same device"
        )));
    }
    let watchdog = trimmed_file(Path::new(WATCHDOG_CLASS).join("identity"))?;
    let watchdog_driver = symlink_basename(watchdog_device.join("driver"))?;
    let watchdog_node = symlink_basename(watchdog_device.join("of_node"))?;
    let watchdog_status = trimmed_file(watchdog_device.join("of_node/status"))?;
    validate_preflight_identity(PreflightIdentity {
        device_name: &device_name,
        driver: &driver,
        watchdog: &watchdog,
        watchdog_driver: &watchdog_driver,
        watchdog_node: &watchdog_node,
        watchdog_status: &watchdog_status,
        irq_count,
        irqs_edge_rising,
    })?;

    require_mapped_char_device(Path::new(IOMMU_DEVICE), &Path::new(IOMMU_CLASS).join("dev"))?;
    OpenOptions::new()
        .read(true)
        .write(true)
        .open(IOMMU_DEVICE)
        .map_err(|source| Error::Io {
            action: "open /dev/iommu for preflight",
            source,
        })?;
    Ok(vec![
        format!("vfio_device={device_name}"),
        format!("vfio_driver={driver}"),
        format!("wifi_spi_irqs={irq_count}"),
        format!("wifi_irqs_edge_rising={irqs_edge_rising}"),
        "iommu_open=true".into(),
        format!("watchdog_device={WATCHDOG_DEVICE}"),
        format!("watchdog_driver={watchdog}"),
        format!("watchdog_fdt_status={watchdog_status}"),
    ])
}

pub trait Host {
    fn resources(&mut self, config: &Cli) -> Result<(), Error>;
    fn firmware(&mut self, board: &Path, regdb: &Path) -> Result<(), Error>;
    fn qmi(&mut self) -> Result<(), Error>;
    fn core(&mut self) -> Result<(), Error>;
    fn passive_scan(&mut self) -> Result<(), Error>;
    fn scan_results(&mut self, ssid: Option<&[u8]>) -> Result<(), Error>;
    fn dp_poll(&mut self) -> Result<(), Error>;
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
            Stage::ScanResults => host.scan_results(config.ssid.as_deref())?,
            Stage::DpPoll => host.dp_poll()?,
        }
        completed.push(stage);
        if stage == config.stop_after {
            break;
        }
    }
    Ok(completed)
}

const SCAN_EVENT_COMPLETED: u32 = 1 << 1;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BssResult {
    pub ssid: Vec<u8>,
    pub bssid: [u8; 6],
    pub channel: u16,
    pub channel_mhz: u32,
    pub rssi_dbm: i32,
    pub rsn: bool,
    pub rsnxe: bool,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct ScanSummary {
    pub bsses: Vec<BssResult>,
    pub selected: Option<BssResult>,
}

impl fmt::Display for BssResult {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "ssid={:?} bssid={:02x}:{:02x}:{:02x}:{:02x}:{:02x}:{:02x} channel={} frequency_mhz={} rssi_dbm={} rsn={} rsnxe={}",
            String::from_utf8_lossy(&self.ssid),
            self.bssid[0],
            self.bssid[1],
            self.bssid[2],
            self.bssid[3],
            self.bssid[4],
            self.bssid[5],
            self.channel,
            self.channel_mhz,
            self.rssi_dbm,
            self.rsn,
            self.rsnxe
        )
    }
}

/// Minimal diagnostic parser used only until the chip-neutral SoftMAC host
/// binds its canonical Fuchsia BSS-description conversion to ath11k.
fn parse_bss(frame: &[u8], channel_mhz: u32, rssi_dbm: i32) -> Option<BssResult> {
    let fixed = frame.get(..36)?;
    let frame_control = u16::from_le_bytes([fixed[0], fixed[1]]);
    if !matches!(frame_control & 0x00fc, 0x0080 | 0x0050) {
        return None;
    }
    let bssid = fixed[16..22].try_into().ok()?;
    let mut ssid = None;
    let mut channel = None;
    let mut rsn = false;
    let mut rsnxe = false;
    let mut ies = &frame[36..];
    while !ies.is_empty() {
        let header = ies.get(..2)?;
        let len = usize::from(header[1]);
        let body = ies.get(2..2 + len)?;
        match header[0] {
            0 if ssid.is_none() && len <= 32 => ssid = Some(body.to_vec()),
            3 if !body.is_empty() => channel = Some(u16::from(body[0])),
            48 => rsn = true,
            61 if channel.is_none() && !body.is_empty() => channel = Some(u16::from(body[0])),
            244 => rsnxe = true,
            _ => {}
        }
        ies = &ies[2 + len..];
    }
    let channel = channel.or_else(|| frequency_channel(channel_mhz))?;
    Some(BssResult {
        ssid: ssid?,
        bssid,
        channel,
        channel_mhz,
        rssi_dbm,
        rsn,
        rsnxe,
    })
}

fn frequency_channel(frequency_mhz: u32) -> Option<u16> {
    match frequency_mhz {
        2484 => Some(14),
        2412..=2472 if (frequency_mhz - 2407).is_multiple_of(5) => {
            u16::try_from((frequency_mhz - 2407) / 5).ok()
        }
        5005..=5895 if (frequency_mhz - 5000).is_multiple_of(5) => {
            u16::try_from((frequency_mhz - 5000) / 5).ok()
        }
        5955..=7115 if (frequency_mhz - 5950).is_multiple_of(5) => {
            u16::try_from((frequency_mhz - 5950) / 5).ok()
        }
        _ => None,
    }
}

fn collect_scan_results<B: ath11k_core::Subsystems>(
    device: &mut ath11k_core::Device<B>,
    selected_ssid: Option<&[u8]>,
) -> Result<ScanSummary, Error> {
    use ath11k_core::EventSource as _;
    let mut bsses: Vec<BssResult> = Vec::new();
    loop {
        match device.next_wlan_event().map_err(Error::Core)? {
            Some(ath11k_core::WlanEvent::ManagementReceived {
                channel_mhz,
                rssi,
                frame,
                ..
            }) => {
                if let Some(candidate) = parse_bss(&frame, channel_mhz, rssi) {
                    if let Some(existing) =
                        bsses.iter_mut().find(|bss| bss.bssid == candidate.bssid)
                    {
                        if candidate.rssi_dbm > existing.rssi_dbm {
                            *existing = candidate;
                        }
                    } else {
                        bsses.push(candidate);
                    }
                }
            }
            Some(ath11k_core::WlanEvent::Scan {
                event_type,
                reason,
                scan_id: 0xa000,
                ..
            }) if event_type & SCAN_EVENT_COMPLETED != 0 => {
                if reason != 0 {
                    return Err(Error::Core(ath11k_core::CoreError::Protocol));
                }
                break;
            }
            Some(_) => {}
            None => return Err(Error::Core(ath11k_core::CoreError::Protocol)),
        }
    }
    bsses.sort_by(|a, b| b.rssi_dbm.cmp(&a.rssi_dbm).then(a.bssid.cmp(&b.bssid)));
    let selected = match selected_ssid {
        Some(ssid) => Some(
            bsses
                .iter()
                .find(|bss| bss.ssid == ssid)
                .cloned()
                .ok_or_else(|| Error::SsidNotFound(ssid.to_vec()))?,
        ),
        None => None,
    };
    Ok(ScanSummary { bsses, selected })
}

fn diagnostic_beacon(bssid: [u8; 6], ssid: &[u8], channel: u8) -> Vec<u8> {
    let mut frame = vec![0x80, 0, 0, 0, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff];
    frame.extend_from_slice(&bssid);
    frame.extend_from_slice(&bssid);
    frame.extend_from_slice(&[0; 14]);
    frame.extend_from_slice(&[0, ssid.len() as u8]);
    frame.extend_from_slice(ssid);
    frame.extend_from_slice(&[3, 1, channel, 48, 2, 1, 0, 244, 1, 0x20]);
    frame
}

fn diagnostic_wmi_events() -> Vec<(u32, Vec<u8>)> {
    use ath11k_wmi::tags::{
        WMI_MGMT_RX_EVENTID, WMI_SCAN_EVENTID, WMI_TAG_ARRAY_BYTE, WMI_TAG_MGMT_RX_HDR,
        WMI_TAG_SCAN_EVENT,
    };
    let tlv = |tag: u16, value: &[u8]| {
        let mut bytes = Vec::with_capacity(4 + value.len());
        bytes.extend_from_slice(&((u32::from(tag) << 16) | value.len() as u32).to_le_bytes());
        bytes.extend_from_slice(value);
        bytes
    };
    let envelope = |id: u32, tlvs: Vec<u8>| {
        let mut bytes = Vec::with_capacity(4 + tlvs.len());
        bytes.extend_from_slice(&(id & 0x00ff_ffff).to_le_bytes());
        bytes.extend(tlvs);
        bytes
    };

    let frame = diagnostic_beacon([0x02, 0, 0, 0, 0, 6], b"dry-run", 6);
    let mut header = [0u8; 68];
    header[0..4].copy_from_slice(&6u32.to_le_bytes());
    header[4..8].copy_from_slice(&44u32.to_le_bytes());
    header[16..20].copy_from_slice(&(frame.len() as u32).to_le_bytes());
    header[44..48].copy_from_slice(&(-42i32).to_le_bytes());
    header[64..68].copy_from_slice(&2437u32.to_le_bytes());
    let mut frame_tlv = frame;
    frame_tlv.resize(frame_tlv.len().next_multiple_of(4), 0);
    let mut mgmt_tlvs = tlv(WMI_TAG_MGMT_RX_HDR.0, &header);
    mgmt_tlvs.extend(tlv(WMI_TAG_ARRAY_BYTE.0, &frame_tlv));

    let words = [SCAN_EVENT_COMPLETED, 0, 0, 1, 0xa000, 0, 0];
    let scan_fixed: Vec<u8> = words.into_iter().flat_map(u32::to_le_bytes).collect();
    vec![
        (
            WMI_MGMT_RX_EVENTID.0,
            envelope(WMI_MGMT_RX_EVENTID.0, mgmt_tlvs),
        ),
        (
            WMI_SCAN_EVENTID.0,
            envelope(WMI_SCAN_EVENTID.0, tlv(WMI_TAG_SCAN_EVENT.0, &scan_fixed)),
        ),
    ]
}

fn dry_scan_config(vdev: ath11k_core::VdevId) -> ath11k_core::ScanConfig {
    ath11k_core::ScanConfig {
        vdev,
        id: ath11k_core::ScanId(0xa000),
        active: false,
        channels_mhz: vec![2412, 2437, 2462],
        ssids: Vec::new(),
    }
}

pub struct DryRunHost {
    device: Option<ath11k_core::Device<ath11k_core::ModelSubsystems>>,
    vdev: Option<ath11k_core::VdevId>,
    summary: Option<ScanSummary>,
    wmi_log: Option<WmiJsonl<BufWriter<File>>>,
    dp_poll_log: Vec<String>,
}

impl Default for DryRunHost {
    fn default() -> Self {
        Self {
            device: Some(ath11k_core::WCN6750.device(Default::default())),
            vdev: None,
            summary: None,
            wmi_log: None,
            dp_poll_log: Vec::new(),
        }
    }
}

impl Host for DryRunHost {
    fn resources(&mut self, config: &Cli) -> Result<(), Error> {
        if let Some(path) = &config.wmi_log {
            self.wmi_log = Some(WmiJsonl::new(BufWriter::new(File::create(path).map_err(
                |source| Error::Io {
                    action: "create deterministic dry-run WMI JSONL",
                    source,
                },
            )?)));
        }
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
        self.record_client_vdev_commands()?;
        Ok(())
    }
    fn passive_scan(&mut self) -> Result<(), Error> {
        use ath11k_core::ClientRadioControl as _;
        self.device
            .as_mut()
            .unwrap()
            .start_scan(dry_scan_config(
                self.vdev
                    .ok_or(Error::Unsupported("scan requested before vdev creation"))?,
            ))
            .map_err(Error::Core)?;
        self.record_scan_start()
    }

    fn scan_results(&mut self, ssid: Option<&[u8]>) -> Result<(), Error> {
        self.record_scan_fixture()?;
        let device = self.device.as_mut().unwrap();
        device
            .backend_mut()
            .push_event(ath11k_core::WlanEvent::ManagementReceived {
                pdev_id: 0,
                channel_mhz: 2437,
                snr: 44,
                rssi: -42,
                flags: 0,
                frame: diagnostic_beacon([0x02, 0, 0, 0, 0, 6], b"dry-run", 6),
            });
        device
            .backend_mut()
            .push_event(ath11k_core::WlanEvent::Scan {
                event_type: SCAN_EVENT_COMPLETED,
                reason: 0,
                request_id: 1,
                scan_id: 0xa000,
                vdev_id: u32::from(self.vdev.unwrap().0),
                channel_mhz: 0,
            });
        self.summary = Some(collect_scan_results(device, ssid)?);
        Ok(())
    }

    fn dp_poll(&mut self) -> Result<(), Error> {
        self.dp_poll_log = vec![dp_poll_summary_line(ath11k_dp::tx::HostServiceResult {
            tx_delivered: 0,
            tx_malformed: 0,
            rx_delivered: 0,
            rx_dropped: Default::default(),
        })];
        Ok(())
    }
}

impl DryRunHost {
    pub fn scan_summary(&self) -> Option<&ScanSummary> {
        self.summary.as_ref()
    }

    pub fn dp_poll_log(&self) -> &[String] {
        &self.dp_poll_log
    }

    fn record_command(
        &mut self,
        command: &impl ath11k_wmi::cmd::EncodeCommand,
    ) -> Result<(), Error> {
        let command = command
            .encode_command()
            .map_err(|_| Error::Core(ath11k_core::CoreError::Protocol))?;
        let mut bytes = Vec::with_capacity(4 + command.tlvs().len());
        bytes.extend_from_slice(&(command.id.0 & 0x00ff_ffff).to_le_bytes());
        bytes.extend_from_slice(command.tlvs());
        if let Some(log) = &mut self.wmi_log {
            log.record_deterministic(WmiKind::Command, command.id.0, &bytes)
                .map_err(|source| Error::Io {
                    action: "write deterministic dry-run WMI command",
                    source,
                })?;
        }
        Ok(())
    }

    fn record_client_vdev_commands(&mut self) -> Result<(), Error> {
        use ath11k_wmi::cmd::{
            StaPowerSaveMode, StaPowerSaveParameter, TxRxStreams, VdevCreate, VdevSetParam,
        };
        let vdev_id = u32::from(self.vdev.unwrap().0);
        self.record_command(&VdevCreate {
            vdev_id,
            vdev_type: 2,
            vdev_subtype: 0,
            mac_addr: [0x02, 0, 0, 0, 0, 1],
            pdev_id: 0,
            mbssid_flags: 0,
            mbssid_tx_vdev_id: 0,
            band_2ghz: TxRxStreams { tx: 2, rx: 2 },
            band_5ghz: TxRxStreams { tx: 2, rx: 2 },
        })?;
        self.record_command(&VdevSetParam {
            vdev_id,
            param_id: 0x22,
            param_value: 2,
        })?;
        for (param, value) in [(0, 0), (1, 1), (2, 0)] {
            self.record_command(&StaPowerSaveParameter {
                vdev_id,
                param,
                value,
            })?;
        }
        self.record_command(&StaPowerSaveMode { vdev_id, mode: 0 })?;
        self.record_command(&VdevSetParam {
            vdev_id,
            param_id: 1,
            param_value: u32::MAX,
        })
    }

    fn record_scan_start(&mut self) -> Result<(), Error> {
        self.record_command(&ath11k_core::wcn6750_scan_start(dry_scan_config(
            self.vdev.unwrap(),
        )))
    }

    fn record_scan_fixture(&mut self) -> Result<(), Error> {
        for (id, bytes) in diagnostic_wmi_events() {
            if let Some(log) = &mut self.wmi_log {
                log.record_deterministic(WmiKind::Event, id, &bytes)
                    .map_err(|source| Error::Io {
                        action: "write deterministic dry-run WMI event",
                        source,
                    })?;
            }
        }
        Ok(())
    }
}

struct JsonTrace(WmiJsonl<BufWriter<File>>);

#[derive(Default)]
struct DiagnosticDpHost {
    lines: Vec<String>,
}

impl ath11k_dp::tx::DpHost for DiagnosticDpHost {
    fn receive(&mut self, frame: ath11k_dp::tx::HostRxFrame) {
        let peer = frame
            .info
            .peer
            .map_or_else(|| "none".into(), |peer| peer.0.to_string());
        let bytes_hex = frame
            .bytes
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>();
        self.lines.push(format!(
            "dp_rx len={} peer={peer} tid={} decap={:?} decrypt={:?} phy_metadata={:#010x} bandwidth={} mcs={} packet_type={} nss={} ppdu_id={} bytes_hex={bytes_hex}",
            frame.bytes.len(),
            frame.info.tid,
            frame.info.decap_type,
            frame.info.decrypt_status,
            frame.info.phy_metadata,
            frame.info.bandwidth,
            frame.info.mcs,
            frame.info.packet_type,
            frame.info.nss,
            frame.info.phy_ppdu_id,
        ));
    }

    fn tx_complete(&mut self, result: ath11k_dp::tx::TxResult) {
        let peer = result
            .peer
            .map_or_else(|| "none".into(), |peer| peer.0.to_string());
        self.lines.push(format!(
            "dp_tx_complete msdu_id={} status={} acknowledged={} ack_rssi={} peer={peer}",
            result.msdu_id, result.status, result.acknowledged, result.ack_rssi,
        ));
    }
}

fn dp_poll_summary_line(result: ath11k_dp::tx::HostServiceResult) -> String {
    format!(
        "dp_poll tx_delivered={} tx_malformed={} rx_delivered={} rx_dropped_malformed={} rx_dropped_fcs={} rx_dropped_decrypt={} rx_dropped_tkip_mic={} rx_dropped_decap={}",
        result.tx_delivered,
        result.tx_malformed,
        result.rx_delivered,
        result.rx_dropped.malformed,
        result.rx_dropped.fcs_error,
        result.rx_dropped.decrypt_error,
        result.rx_dropped.tkip_mic_error,
        result.rx_dropped.unsupported_decap,
    )
}

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
    summary: Option<ScanSummary>,
    dp_poll_log: Vec<String>,
}

fn diagnose_iommufd_open(error: &str) -> String {
    if error.contains("bind VFIO device to iommufd") && error.contains("os error 1") {
        format!(
            "VFIO_DEVICE_BIND_IOMMUFD returned EPERM: the arm64 wired-IRQ gate \
             iommu_group_has_isolated_msi() requires runtime parameter \
             iommufd.allow_unsafe_interrupts=1 for the polling-only first run; \
             revisit this override before installing software MSI ({error})"
        )
    } else {
        error.into()
    }
}

impl Host for RealHost {
    fn resources(&mut self, config: &Cli) -> Result<(), Error> {
        let vfio = if config.broker {
            LinuxVfio::open_broker(config.vfio_device.as_ref().ok_or(Error::Unsupported(
                "real mode requires an explicit VFIO cdev path",
            ))?)
        } else {
            LinuxVfio::open_coherent(config.vfio_device.as_ref().ok_or(Error::Unsupported(
                "real mode requires an explicit VFIO cdev path",
            ))?)
        }
        .map_err(|error| {
            let error = error.to_string();
            Error::Hardware(if config.broker {
                error
            } else {
                diagnose_iommufd_open(&error)
            })
        })?;
        vfio.validate_wcn6750_resources().map_err(|error| {
            Error::Hardware(format!("validate WCN6750 VFIO resources: {error}"))
        })?;
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
            .start_scan(dry_scan_config(
                self.vdev
                    .ok_or(Error::Unsupported("scan requested before vdev creation"))?,
            ))
            .map_err(Error::Core)
    }

    fn scan_results(&mut self, ssid: Option<&[u8]>) -> Result<(), Error> {
        self.summary = Some(collect_scan_results(
            self.device.as_mut().ok_or(Error::Unsupported(
                "scan results requested before core startup",
            ))?,
            ssid,
        )?);
        Ok(())
    }

    fn dp_poll(&mut self) -> Result<(), Error> {
        const WORK_BUDGET: usize = 64;
        const RECEIVE_BUDGET: usize = 64;

        let device = self
            .device
            .as_mut()
            .ok_or(Error::Unsupported("DP poll requested before core startup"))?;
        let mut host = DiagnosticDpHost::default();
        let result = device
            .service_dp_host(WORK_BUDGET, RECEIVE_BUDGET, &mut host)
            .map_err(Error::Core)?;
        host.lines.push(dp_poll_summary_line(result));
        self.dp_poll_log = host.lines;
        Ok(())
    }
}

impl RealHost {
    pub fn scan_summary(&self) -> Option<&ScanSummary> {
        self.summary.as_ref()
    }

    pub fn dp_poll_log(&self) -> &[String] {
        &self.dp_poll_log
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

    fn record_deterministic(&mut self, kind: WmiKind, id: u32, bytes: &[u8]) -> io::Result<()> {
        self.record_at(self.next_seq, kind, id, bytes)
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
        writeln!(self.writer, "\"}}")?;
        self.writer.flush()
    }

    pub fn into_inner(self) -> W {
        self.writer
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct FlushFailure;
    impl Write for FlushFailure {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            Ok(bytes.len())
        }
        fn flush(&mut self) -> io::Result<()> {
            Err(io::Error::other("flush failed"))
        }
    }

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
        fn scan_results(&mut self, _: Option<&[u8]>) -> Result<(), Error> {
            self.visit(Stage::ScanResults)
        }
        fn dp_poll(&mut self) -> Result<(), Error> {
            self.visit(Stage::DpPoll)
        }
    }

    #[test]
    fn parses_cli_and_cdev_iommufd_is_default() {
        let cli = Cli::parse(["--dry-run", "--stop-after", "qmi", "--board", "/b"]).unwrap();
        assert!(cli.dry_run);
        assert!(!cli.broker);
        assert_eq!(cli.stop_after, Stage::Qmi);
        assert_eq!(cli.board, PathBuf::from("/b"));
        assert_eq!(cli.regdb, PathBuf::from(DEFAULT_REGDB));
        assert!(Cli::parse(["--stop-after", "unknown"]).is_err());
        assert!(Cli::parse([] as [&str; 0]).is_err());
        assert!(Cli::parse(["--dry-run", "--broker"]).is_err());
        assert!(Cli::parse(["--coherent"]).is_err());
        let preflight =
            Cli::parse(["preflight", "--vfio-device", "/dev/vfio/devices/vfio7"]).unwrap();
        assert!(preflight.preflight);
    }

    #[test]
    fn preflight_identity_gate_rejects_every_mismatch() {
        let valid = PreflightIdentity {
            device_name: EXPECTED_VFIO_DEVICE,
            driver: EXPECTED_VFIO_DRIVER,
            watchdog: EXPECTED_WATCHDOG_DRIVER,
            watchdog_driver: EXPECTED_WATCHDOG_DRIVER,
            watchdog_node: "watchdog@17c10000",
            watchdog_status: "okay",
            irq_count: 32,
            irqs_edge_rising: true,
        };
        assert!(validate_preflight_identity(valid).is_ok());
        for facts in [
            PreflightIdentity {
                device_name: "wrong.wifi",
                ..valid
            },
            PreflightIdentity {
                driver: "ath11k_ahb",
                ..valid
            },
            PreflightIdentity {
                watchdog: "softdog",
                ..valid
            },
            PreflightIdentity {
                watchdog_driver: "softdog",
                ..valid
            },
            PreflightIdentity {
                watchdog_node: "watchdog@wrong",
                ..valid
            },
            PreflightIdentity {
                watchdog_status: "reserved",
                ..valid
            },
            PreflightIdentity {
                irq_count: 31,
                ..valid
            },
            PreflightIdentity {
                irqs_edge_rising: false,
                ..valid
            },
        ] {
            assert!(validate_preflight_identity(facts).is_err());
        }
    }

    #[test]
    fn interrupt_parser_requires_32_edge_rising_spis() {
        let mut bytes = Vec::new();
        for irq in 0..32_u32 {
            bytes.extend_from_slice(&0_u32.to_be_bytes());
            bytes.extend_from_slice(&irq.to_be_bytes());
            bytes.extend_from_slice(&1_u32.to_be_bytes());
        }
        assert_eq!(interrupt_facts(&bytes).unwrap(), (32, true));
        bytes[8..12].copy_from_slice(&4_u32.to_be_bytes());
        assert_eq!(interrupt_facts(&bytes).unwrap(), (32, false));
        assert!(interrupt_facts(&bytes[..bytes.len() - 1]).is_err());
    }

    #[test]
    fn preflight_rejects_unsafe_cdev_names_and_bad_device_numbers() {
        assert!(valid_vfio_cdev_name("vfio0"));
        assert!(valid_vfio_cdev_name("vfio123"));
        assert!(!valid_vfio_cdev_name("vfio"));
        assert!(!valid_vfio_cdev_name("noiommu-vfio0"));
        assert!(!valid_vfio_cdev_name("vfio0x"));
        assert_eq!(parse_device_number("10:130").unwrap(), (10, 130));
        assert!(parse_device_number("10").is_err());
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
    fn iommufd_bind_eperm_names_the_wired_irq_gate_and_parameter() {
        let message = diagnose_iommufd_open(
            "initialize coherent VFIO/iommufd device: bind VFIO device to iommufd: Operation not permitted (os error 1)",
        );
        assert!(message.contains("VFIO_DEVICE_BIND_IOMMUFD returned EPERM"));
        assert!(message.contains("iommu_group_has_isolated_msi()"));
        assert!(message.contains("iommufd.allow_unsafe_interrupts=1"));
        assert!(message.contains("before installing software MSI"));
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
    fn jsonl_flush_failure_fails_the_record() {
        assert!(
            WmiJsonl::new(FlushFailure)
                .record_at(0, WmiKind::Event, 1, &[])
                .is_err()
        );
    }

    #[test]
    fn diagnostic_dp_host_logs_deliveries_and_completions() {
        use ath11k_dp::tx::{
            DpHost as _, HostRxFrame, HostRxInfo, RxDecapType, RxDecryptStatus, TxResult,
        };

        let mut host = DiagnosticDpHost::default();
        host.receive(HostRxFrame {
            bytes: vec![0x08, 0xaf],
            info: HostRxInfo {
                decap_type: RxDecapType::NativeWifi,
                peer: Some(ath11k_dp::PeerId(7)),
                tid: 3,
                decrypt_status: RxDecryptStatus::Decrypted,
                phy_metadata: 0x1234,
                bandwidth: 1,
                mcs: 5,
                packet_type: 2,
                nss: 2,
                phy_ppdu_id: 9,
            },
        });
        host.tx_complete(TxResult {
            msdu_id: 11,
            status: 0,
            acknowledged: true,
            ack_rssi: -42,
            peer: Some(ath11k_dp::PeerId(7)),
        });
        assert!(host.lines[0].contains("peer=7 tid=3"));
        assert!(host.lines[0].ends_with("bytes_hex=08af"));
        assert!(host.lines[1].contains("msdu_id=11 status=0 acknowledged=true"));
    }

    #[test]
    fn dry_run_completes_dp_poll_without_paths_or_hardware() {
        let cli = Cli {
            dry_run: true,
            vfio_device: Some("/does/not/exist".into()),
            board: "/does/not/exist".into(),
            regdb: "/does/not/exist".into(),
            wmi_log: None,
            ssid: Some(b"dry-run".to_vec()),
            ..Cli::default()
        };
        let mut host = DryRunHost::default();
        let completed = run(&cli, &mut host).unwrap();
        assert_eq!(completed.last(), Some(&Stage::DpPoll));
        assert_eq!(host.dp_poll_log.len(), 1);
        assert!(host.dp_poll_log[0].contains("rx_delivered=0"));
        assert_eq!(host.scan_summary().unwrap().bsses[0].ssid, b"dry-run");
        assert_eq!(
            host.scan_summary().unwrap().selected.as_ref().unwrap().ssid,
            b"dry-run"
        );
        assert!(
            host.device
                .as_ref()
                .unwrap()
                .backend()
                .operations()
                .contains(&ath11k_core::Operation::DpPdevAllocate)
        );
    }

    #[test]
    fn native_probe_response_extracts_diagnostic_bss_fields() {
        // First probe response in the checked-in Redwood native WMI capture.
        let hex = "50003a010284f8caacc6160808f37916160808f379167085e8332a8c01000000640011000011494f54352d4d322d34353933323030393001088b9682840c1830600301010706434e00010d1432046c122448dd0918fe3403010000000030180100000fac020200000fac04000fac020100000fac020000";
        let frame: Vec<u8> = hex
            .as_bytes()
            .chunks_exact(2)
            .map(|pair| u8::from_str_radix(std::str::from_utf8(pair).unwrap(), 16).unwrap())
            .collect();
        let bss = parse_bss(&frame, 2412, -55).unwrap();
        assert_eq!(bss.ssid, b"IOT5-M2-459320090");
        assert_eq!(bss.bssid, [0x16, 0x08, 0x08, 0xf3, 0x79, 0x16]);
        assert_eq!(bss.channel, 1);
        assert!(bss.rsn);
        assert!(!bss.rsnxe);
        assert_eq!(bss.rssi_dbm, -55);
    }

    #[test]
    fn diagnostic_ie_walker_rejects_truncation_and_detects_rsnxe() {
        let mut frame = diagnostic_beacon([1; 6], b"test", 6);
        let bss = parse_bss(&frame, 2437, -40).unwrap();
        assert!(bss.rsn && bss.rsnxe);
        frame.pop();
        assert_eq!(parse_bss(&frame, 2437, -40), None);
    }
}
