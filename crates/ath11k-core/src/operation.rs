use crate::{CoreError, PdevId, VdevId, WlanEvent};
use alloc::vec::Vec;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ScanId(pub u32);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Channel {
    pub primary_mhz: u16,
    pub center1_mhz: u16,
    pub center2_mhz: u16,
    pub info: u32,
    pub reg_info_1: u32,
    pub reg_info_2: u32,
}
impl Channel {
    pub const fn from_primary_frequency(primary_mhz: u16) -> Self {
        Self {
            primary_mhz,
            center1_mhz: primary_mhz,
            center2_mhz: 0,
            info: 0,
            reg_info_1: 0,
            reg_info_2: 0,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ScanConfig {
    pub vdev: VdevId,
    pub id: ScanId,
    pub active: bool,
    pub channels_mhz: Vec<u16>,
    pub ssids: Vec<Vec<u8>>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AssociationBandwidth {
    Bw20,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct WmmAccessCategory {
    pub ecw_min: u8,
    pub ecw_max: u8,
    pub aifsn: u8,
    pub txop_limit: u16,
    pub admission_control_mandatory: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct WmmConfig {
    /// Firmware order: best-effort, background, video, voice.
    pub access_categories: [WmmAccessCategory; 4],
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PeerAssociation {
    pub vdev: VdevId,
    pub peer: [u8; 6],
    pub aid: u16,
    pub listen_interval: u16,
    pub primary_mhz: u16,
    pub bandwidth: AssociationBandwidth,
    pub capability_info: u16,
    /// IEEE half-Mbps values with the basic-rate bit removed.
    pub legacy_rates: Vec<u8>,
    pub qos: bool,
    pub ht_capabilities: Option<[u8; 26]>,
    pub vht_capabilities: Option<[u8; 12]>,
    pub wmm: Option<WmmConfig>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Cipher {
    Ccmp128,
    Ccmp256,
    Tkip,
    Gcmp128,
    Gcmp256,
    BipCmac128,
    BipGmac128,
    BipGmac256,
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum KeyKind {
    Pairwise,
    Group,
    IntegrityGroup,
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum KeyProtection {
    Rx,
    Tx,
    RxTx,
}
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct KeyConfig {
    pub vdev: VdevId,
    pub peer: [u8; 6],
    pub index: u8,
    pub cipher: Cipher,
    pub kind: KeyKind,
    /// Requested traffic direction. WCN6750 WMI has no direction field, so
    /// core accepts only `RxTx` rather than silently weakening this contract.
    pub protection: KeyProtection,
    pub receive_sequence_counter: u64,
    pub bytes: Vec<u8>,
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RegulatoryChannel {
    pub frequency_mhz: u16,
    pub max_power_dbm: i8,
    pub max_reg_power_dbm: i8,
    pub max_antenna_gain_dbi: i8,
    pub passive: bool,
    pub radar: bool,
    pub allow_ht: bool,
    pub allow_vht: bool,
    pub allow_he: bool,
}

impl Channel {
    /// Normalize the C `ath11k_mac_vdev_start_restart` channel fields for a
    /// 20 MHz client channel.
    pub fn client_20mhz(channel: RegulatoryChannel) -> Self {
        let mut info = if channel.frequency_mhz < 3_000 {
            21
        } else {
            16
        };
        if channel.passive {
            info |= 1 << 7;
        }
        let max_power = u32::from(channel.max_power_dbm as u8);
        let max_reg_power = u32::from(channel.max_reg_power_dbm as u8);
        let max_antenna_gain = u32::from(channel.max_antenna_gain_dbi as u8);
        Self {
            primary_mhz: channel.frequency_mhz,
            center1_mhz: channel.frequency_mhz,
            center2_mhz: 0,
            info,
            reg_info_1: (max_power << 8) | (max_reg_power << 16),
            reg_info_2: max_antenna_gain | (max_power << 8),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RegulatoryDomain {
    pub alpha2: [u8; 2],
    pub channels: Vec<RegulatoryChannel>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ManagementFrame {
    pub vdev: VdevId,
    pub buffer_id: u32,
    pub bytes: Vec<u8>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Operation {
    QmiInitService,
    QmiDeinitService,
    QmiFirmwareStart,
    QmiWaitFirmwareReady,
    QmiFirmwareStop,
    HifPowerUp,
    HifPowerDown,
    HifStart,
    HifStop,
    HifIrqEnable,
    HifIrqDisable,
    CeInitPipes,
    HtcInit,
    HtcWaitTarget,
    HtcStart,
    WmiAttach,
    WmiConnect,
    WmiWaitServiceReady,
    WmiCommandInit,
    WmiWaitUnifiedReady,
    WmiDetach,
    DpAllocate,
    DpFree,
    DpHttConnect,
    DpPdevPreAllocate,
    DpPdevAllocate,
    DpPdevFree,
    DpReoSetup,
    DpReoCleanup,
    DpHttVersionRequest,
    MacAllocate,
    MacDestroy,
    MacRegister,
    MacUnregister,
    RadioStart,
    RegFree,
    PdevSuspend,
    RecoveryQuiesce,
    RecoveryRestart,
    WmiVdevCreate {
        vdev: VdevId,
        mac: [u8; 6],
    },
    WmiVdevSetNss {
        vdev: VdevId,
        nss: u8,
    },
    WmiStaPsRxWake {
        vdev: VdevId,
    },
    WmiStaPsTxWake {
        vdev: VdevId,
    },
    WmiStaPsPollCount {
        vdev: VdevId,
    },
    WmiStaPsDisable {
        vdev: VdevId,
    },
    WmiVdevSetRtsThreshold {
        vdev: VdevId,
        threshold: u32,
    },
    DpVdevTxAttach {
        vdev: VdevId,
    },
    WmiVdevStart {
        vdev: VdevId,
        restart: bool,
        channel: Channel,
    },
    WaitVdevSetup {
        vdev: VdevId,
    },
    WmiVdevUp {
        vdev: VdevId,
        bssid: [u8; 6],
        aid: u16,
    },
    WmiObssSpatialReuse {
        vdev: VdevId,
    },
    WmiDtimPolicyStick {
        vdev: VdevId,
    },
    WmiVdevDown {
        vdev: VdevId,
    },
    WmiVdevStop {
        vdev: VdevId,
    },
    WmiVdevDelete {
        vdev: VdevId,
    },
    WaitVdevDeleted {
        vdev: VdevId,
    },
    WmiPeerCreate {
        vdev: VdevId,
        address: [u8; 6],
    },
    WaitPeerCreated {
        vdev: VdevId,
        address: [u8; 6],
    },
    WmiPeerAssociate(PeerAssociation),
    WmiWmmUpdate {
        vdev: VdevId,
        wmm: WmmConfig,
    },
    WmiPeerSetSmps {
        vdev: VdevId,
        address: [u8; 6],
        mode: u32,
    },
    WaitPeerAssociated {
        vdev: VdevId,
        address: [u8; 6],
    },
    WmiPeerAuthorize {
        vdev: VdevId,
        address: [u8; 6],
        authorized: bool,
    },
    WmiPeerDelete {
        vdev: VdevId,
        address: [u8; 6],
    },
    WaitPeerDeleted {
        vdev: VdevId,
        address: [u8; 6],
    },
    WmiInstallKey(KeyConfig),
    WaitKeyInstalled {
        vdev: VdevId,
        key_index: u8,
    },
    WmiScanStart(ScanConfig),
    WmiScanStop {
        vdev: VdevId,
        scan: ScanId,
    },
    WmiMgmtTx(ManagementFrame),
    WmiSetCurrentCountry {
        alpha2: [u8; 2],
    },
    WmiScanChannelList {
        pdev: PdevId,
        channels: Vec<RegulatoryChannel>,
    },
    WaitRegulatoryUpdate {
        pdev: PdevId,
    },
    WmiPdevSetTxPower {
        pdev: PdevId,
        half_dbm: i16,
    },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum OperationTarget {
    Qmi,
    Hif,
    CeHtc,
    Wmi,
    DpHtt,
    MacMlme,
    Regulatory,
    Recovery,
}
impl Operation {
    pub const fn target(&self) -> OperationTarget {
        use Operation::*;
        match self {
            QmiInitService | QmiDeinitService | QmiFirmwareStart | QmiWaitFirmwareReady
            | QmiFirmwareStop => OperationTarget::Qmi,
            HifPowerUp | HifPowerDown | HifStart | HifStop | HifIrqEnable | HifIrqDisable => {
                OperationTarget::Hif
            }
            CeInitPipes | HtcInit | HtcWaitTarget | HtcStart => OperationTarget::CeHtc,
            DpAllocate
            | DpFree
            | DpHttConnect
            | DpPdevPreAllocate
            | DpPdevAllocate
            | DpPdevFree
            | DpReoSetup
            | DpReoCleanup
            | DpHttVersionRequest
            | DpVdevTxAttach { .. } => OperationTarget::DpHtt,
            MacAllocate | MacDestroy | MacRegister | MacUnregister | RadioStart => {
                OperationTarget::MacMlme
            }
            RegFree => OperationTarget::Regulatory,
            RecoveryQuiesce | RecoveryRestart => OperationTarget::Recovery,
            _ => OperationTarget::Wmi,
        }
    }
}

/// Composition seam while subsystem crates acquire their source-shaped
/// lifecycle APIs. Implementations dispatch operations to QMI/CE/HTC/WMI/DP.
pub trait Subsystems {
    fn execute(&mut self, operation: Operation) -> Result<(), CoreError>;

    /// Execute one vdev start and classify whether failure leaves a firmware
    /// response outstanding. Generic transports conservatively report
    /// ambiguity; transports with stronger send/completion contracts override.
    fn execute_vdev_start(
        &mut self,
        vdev: VdevId,
        restart: bool,
        channel: Channel,
    ) -> Result<(), VdevStartFailure> {
        self.execute(Operation::WmiVdevStart {
            vdev,
            restart,
            channel,
        })
        .map_err(VdevStartFailure::Ambiguous)?;
        self.execute(Operation::WaitVdevSetup { vdev })
            .map_err(VdevStartFailure::Ambiguous)
    }

    /// Drive the event-oriented QMI handshake until firmware reports ready.
    /// The returned value is consumed by core rather than supplied by its caller.
    fn wait_for_firmware_ready(&mut self) -> Result<ath11k_qmi::FirmwareReady, CoreError>;

    /// Receive the next hardware event consumed by the WlanSoftmac half.
    fn next_wlan_event(&mut self) -> Result<Option<WlanEvent>, CoreError>;

    /// Consume at most one bounded runtime slot and report whether hardware
    /// work was observed even when it did not produce a host-facing event.
    fn next_wlan_event_bounded(
        &mut self,
        work_budget: usize,
    ) -> Result<(Option<WlanEvent>, bool), CoreError> {
        if work_budget == 0 {
            return Ok((None, false));
        }
        let event = self.next_wlan_event()?;
        let progressed = event.is_some();
        Ok((event, progressed))
    }

    /// Station spatial streams advertised by firmware for vdev setup.
    fn client_nss(&self) -> Result<u8, CoreError>;

    /// Service bounded client data-path work without exposing rings or DMA.
    /// Deterministic subsystem models have no data-path completions to report.
    fn service_dp_host<H: ath11k_dp::tx::DpHost>(
        &mut self,
        _work_budget: usize,
        _receive_budget: usize,
        _host: &mut H,
    ) -> Result<ath11k_dp::tx::HostServiceResult, CoreError> {
        Ok(ath11k_dp::tx::HostServiceResult {
            tx_delivered: 0,
            tx_malformed: 0,
            rx_delivered: 0,
            rx_dropped: Default::default(),
        })
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum VdevStartFailure {
    /// The command was definitively not visible to firmware.
    NotSent(CoreError),
    /// Firmware returned a matched, completed rejection.
    Rejected(CoreError),
    /// A response may still arrive and cannot be correlated with a retry.
    Ambiguous(CoreError),
}

/// Deterministic subsystem model used before transports are attached and by
/// transcript conformance tests.
#[derive(Default)]
pub struct ModelSubsystems {
    operations: Vec<Operation>,
    fail_next: Option<Operation>,
    events: Vec<WlanEvent>,
}
impl ModelSubsystems {
    pub fn operations(&self) -> &[Operation] {
        &self.operations
    }
    pub fn clear(&mut self) {
        self.operations.clear();
    }
    pub fn fail_once(&mut self, operation: Operation) {
        self.fail_next = Some(operation);
    }
    pub fn push_event(&mut self, event: WlanEvent) {
        self.events.push(event);
    }
}
impl Subsystems for ModelSubsystems {
    fn execute(&mut self, operation: Operation) -> Result<(), CoreError> {
        self.operations.push(operation.clone());
        if self.fail_next.as_ref() == Some(&operation) {
            self.fail_next = None;
            Err(CoreError::DeviceFault)
        } else {
            Ok(())
        }
    }

    fn wait_for_firmware_ready(&mut self) -> Result<ath11k_qmi::FirmwareReady, CoreError> {
        self.execute(Operation::QmiWaitFirmwareReady)?;
        Ok(ath11k_qmi::FirmwareReady {
            firmware_version: 1,
            target_mem_mode: 0,
        })
    }

    fn next_wlan_event(&mut self) -> Result<Option<WlanEvent>, CoreError> {
        if self.events.is_empty() {
            Ok(None)
        } else {
            Ok(Some(self.events.remove(0)))
        }
    }

    fn client_nss(&self) -> Result<u8, CoreError> {
        Ok(2)
    }
}
