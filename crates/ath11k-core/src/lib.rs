#![no_std]
#![forbid(unsafe_code)]
//! WCN6750 composition, lifecycle, and the hardware-effects half of ath11k.
//!
//! Linux's mac80211 policy is deliberately not represented here.  [`RadioControl`]
//! and [`ClientRadioControl`] are the effects used by the WlanSoftmac seam.

extern crate alloc;

mod ahb;
mod events;
mod hw;
mod operation;
mod qmi;
mod real;

pub use ahb::{
    InterruptRoute, MsiUser, RegisterWindow, WCN6750_CE_INTERRUPT_ROUTES,
    WCN6750_DP_INTERRUPT_ROUTES, WCN6750_INTERRUPT_ROUTES, Wcn6750CeWaiter, Wcn6750DpInterrupts,
    Wcn6750InterruptService, Wcn6750InterruptServiceError, Wcn6750Interrupts, Wcn6750Irq,
    dispatch_wcn6750_interrupt, service_ce_interrupt, service_dp_external_group,
    wcn6750_register_offset,
};
pub use events::{EventSink, WlanEvent};
pub use hw::{FirmwareLayout, HardwareParams, RingMask, WCN6750, Wcn6750};
pub use operation::{
    AssociationBandwidth, Channel, Cipher, KeyConfig, KeyKind, KeyProtection, ManagementFrame,
    ModelSubsystems, Operation, OperationTarget, PeerAssociation, RegulatoryChannel,
    RegulatoryDomain, ScanConfig, ScanId, Subsystems, VdevStartFailure, WmmAccessCategory,
    WmmConfig, redwood_india_domain,
};
pub use qmi::{
    HardwareMemoryProvider, Wcn6750FirmwareAssetError, Wcn6750FirmwareAssets, Wcn6750QmiSession,
};
pub use real::{NoWmiTrace, Wcn6750Subsystems, WmiTraceSink, wcn6750_scan_start};

use alloc::vec::Vec;
use ath11k_qmi::FirmwareReady;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct VdevId(pub u8);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PdevId(pub u8);

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum CoreError {
    WrongState,
    Qmi(ath11k_qmi::QmiError),
    DpAllocation {
        cause: ath11k_dp::DpError,
        cleanup: Option<ath11k_dp::DpError>,
    },
    HtcControlSend(ath11k_ce::CeError),
    HtcControlReceive(ath11k_ce::CeError),
    HtcControlTimeout {
        ce0_source_progress: Option<(u32, u32)>,
        ce2_destination_progress: Option<(u32, u32)>,
        ce2_status_progress: Option<(u32, u32)>,
    },
    WmiSend(ath11k_wmi::WmiError),
    WmiWait(ath11k_wmi::WmiError),
    DpPeerSetup(ath11k_dp::DpError),
    HttPeerMap {
        cause: ath11k_dp::DpError,
        message: Option<(usize, u8)>,
    },
    HttPeerMapTimeout {
        last_event: Option<ath11k_dp::htt::HttEvent>,
        ce1_destination_progress: Option<(u32, u32)>,
        ce1_status_progress: Option<(u32, u32)>,
    },
    HttVersionTimeout {
        ce4_source_progress: Option<(u32, u32)>,
        ce1_destination_progress: Option<(u32, u32)>,
        ce1_status_progress: Option<(u32, u32)>,
    },
    Protocol,
    ProtocolAt(Operation),
    DeviceFault,
    DeviceFaultAt(Operation),
    NoResources,
    NotFound,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DeviceState {
    Allocated,
    Probed,
    Ready,
    Recovering,
    Stopped,
    Wedged,
}

pub trait Lifecycle {
    fn probe(&mut self) -> Result<(), CoreError>;
    fn attach_firmware(&mut self) -> Result<FirmwareReady, CoreError>;
    fn start_radio(&mut self) -> Result<(), CoreError>;
    fn stop(&mut self) -> Result<(), CoreError>;
}

/// Ordered firmware event source handed upward after lifecycle initialization.
pub trait EventSource {
    fn next_wlan_event(&mut self) -> Result<Option<WlanEvent>, CoreError>;
}

impl<B: Subsystems> EventSource for Device<B> {
    fn next_wlan_event(&mut self) -> Result<Option<WlanEvent>, CoreError> {
        if self.state != DeviceState::Ready {
            return Err(CoreError::WrongState);
        }
        self.backend.next_wlan_event()
    }
}

/// The frozen, minimal hardware-effects interface.
pub trait RadioControl {
    fn create_client_vdev(&mut self, mac: [u8; 6]) -> Result<VdevId, CoreError>;
    fn start_vdev(&mut self, vdev: VdevId, channel: RegulatoryChannel) -> Result<(), CoreError>;
    fn create_peer(&mut self, vdev: VdevId, address: [u8; 6]) -> Result<(), CoreError>;
    fn delete_peer(&mut self, vdev: VdevId, address: [u8; 6]) -> Result<(), CoreError>;
}

/// Remaining client-mode WlanSoftmac effects.  Methods are operations rather
/// than policy decisions and retain the WMI call order in the pinned mac.c.
pub trait ClientRadioControl {
    fn up_vdev(&mut self, vdev: VdevId, bssid: [u8; 6], aid: u16) -> Result<(), CoreError>;
    fn down_vdev(&mut self, vdev: VdevId) -> Result<(), CoreError>;
    fn stop_vdev(&mut self, vdev: VdevId) -> Result<(), CoreError>;
    fn delete_vdev(&mut self, vdev: VdevId) -> Result<(), CoreError>;
    fn associate_peer(&mut self, association: PeerAssociation) -> Result<(), CoreError>;
    fn install_key(&mut self, key: KeyConfig) -> Result<(), CoreError>;
    fn set_peer_authorized(
        &mut self,
        vdev: VdevId,
        address: [u8; 6],
        authorized: bool,
    ) -> Result<(), CoreError>;
    fn start_scan(&mut self, scan: ScanConfig) -> Result<(), CoreError>;
    fn stop_scan(&mut self, vdev: VdevId, scan: ScanId) -> Result<(), CoreError>;
    fn transmit_management(&mut self, frame: ManagementFrame) -> Result<(), CoreError>;
    fn set_tx_power(&mut self, dbm: i8) -> Result<(), CoreError>;
    fn set_regulatory_domain(&mut self, domain: RegulatoryDomain) -> Result<(), CoreError>;
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct Vdev {
    id: VdevId,
    mac: [u8; 6],
    started: bool,
    up: bool,
    channel: Option<Channel>,
}

/// Runtime owner of the post-substrate ath11k device.
pub struct Device<B: Subsystems> {
    backend: B,
    state: DeviceState,
    firmware: Option<FirmwareReady>,
    vdevs: Vec<Vdev>,
    uncertain_vdev_starts: Vec<VdevId>,
    peers: Vec<(VdevId, [u8; 6])>,
    installed_keys: Vec<KeyConfig>,
    uncertain_key_peers: Vec<(VdevId, [u8; 6])>,
    crash_count: u32,
}

impl Wcn6750 {
    pub fn device<B: Subsystems>(self, backend: B) -> Device<B> {
        Device {
            backend,
            state: DeviceState::Allocated,
            firmware: None,
            vdevs: Vec::new(),
            uncertain_vdev_starts: Vec::new(),
            peers: Vec::new(),
            installed_keys: Vec::new(),
            uncertain_key_peers: Vec::new(),
            crash_count: 0,
        }
    }
}

impl<B: Subsystems> Device<B> {
    pub fn state(&self) -> DeviceState {
        self.state
    }
    pub fn backend(&self) -> &B {
        &self.backend
    }
    pub fn backend_mut(&mut self) -> &mut B {
        &mut self.backend
    }
    pub fn into_backend(self) -> B {
        self.backend
    }
    pub fn firmware(&self) -> Option<FirmwareReady> {
        self.firmware
    }
    pub fn firmware_crash_count(&self) -> u32 {
        self.crash_count
    }

    /// Service one bounded host-facing data-path slot.
    pub fn service_dp_host<H: ath11k_dp::tx::DpHost>(
        &mut self,
        work_budget: usize,
        receive_budget: usize,
        host: &mut H,
    ) -> Result<ath11k_dp::tx::HostServiceResult, CoreError> {
        if self.state != DeviceState::Ready {
            return Err(CoreError::WrongState);
        }
        self.backend
            .service_dp_host(work_budget, receive_budget, host)
    }

    /// Consume a bounded control-event slot, including ignored firmware work.
    pub fn poll_wlan_event(
        &mut self,
        work_budget: usize,
    ) -> Result<(Option<WlanEvent>, bool), CoreError> {
        if self.state != DeviceState::Ready {
            return Err(CoreError::WrongState);
        }
        self.backend.next_wlan_event_bounded(work_budget)
    }

    /// Unwind a partially or fully initialized device after lifecycle start
    /// fails. This is terminal; callers construct a fresh owner to retry.
    pub fn abort_startup(&mut self) {
        match self.state {
            DeviceState::Allocated | DeviceState::Probed => {
                let _ = self.op(Operation::DpFree);
                let _ = self.op(Operation::WmiDetach);
                let _ = self.op(Operation::QmiFirmwareStop);
                let _ = self.op(Operation::HifPowerDown);
                let _ = self.op(Operation::RegFree);
                let _ = self.op(Operation::QmiDeinitService);
                self.state = DeviceState::Stopped;
            }
            DeviceState::Ready | DeviceState::Recovering => {
                let _ = self.stop();
            }
            DeviceState::Stopped | DeviceState::Wedged => {}
        }
    }

    fn op(&mut self, operation: Operation) -> Result<(), CoreError> {
        let failed_operation = operation.clone();
        self.backend
            .execute(operation)
            .map_err(|error| match error {
                CoreError::DeviceFault => CoreError::DeviceFaultAt(failed_operation),
                CoreError::Protocol => CoreError::ProtocolAt(failed_operation),
                error => error,
            })
    }

    fn has_vdev(&self, id: VdevId) -> bool {
        self.vdevs.iter().any(|v| v.id == id)
    }

    fn core_start(&mut self) -> Result<(), CoreError> {
        const OPS: &[Operation] = &[
            Operation::WmiAttach,
            Operation::HtcInit,
            Operation::HifStart,
            Operation::HtcWaitTarget,
            Operation::DpHttConnect,
            Operation::WmiConnect,
            Operation::HtcStart,
            Operation::WmiWaitServiceReady,
            Operation::MacAllocate,
            Operation::DpPdevPreAllocate,
            Operation::DpReoSetup,
            Operation::WmiCommandInit,
            Operation::WmiWaitUnifiedReady,
            Operation::DpHttVersionRequest,
        ];
        for (index, operation) in OPS.iter().cloned().enumerate() {
            if let Err(error) = self.op(operation) {
                // These labels exactly mirror core.c's fall-through unwind.
                if index >= 10 {
                    let _ = self.op(Operation::DpReoCleanup);
                }
                if index >= 9 {
                    let _ = self.op(Operation::MacDestroy);
                }
                if index >= 3 {
                    let _ = self.op(Operation::HifStop);
                }
                let _ = self.op(Operation::WmiDetach);
                return Err(error);
            }
        }
        Ok(())
    }

    fn core_stop(&mut self, crash_flush: bool) {
        if !crash_flush {
            let _ = self.op(Operation::QmiFirmwareStop);
        }
        let _ = self.op(Operation::HifStop);
        let _ = self.op(Operation::WmiDetach);
        let _ = self.op(Operation::DpReoCleanup);
    }

    pub fn firmware_crashed(&mut self) -> Result<(), CoreError> {
        if self.state != DeviceState::Ready {
            return Err(CoreError::WrongState);
        }
        self.crash_count = self.crash_count.saturating_add(1);
        self.state = DeviceState::Recovering;
        let _ = self.op(Operation::RecoveryQuiesce);
        self.core_stop(true);
        self.op(Operation::RecoveryRestart)?;
        Ok(())
    }

    pub fn complete_recovery(&mut self) -> Result<FirmwareReady, CoreError> {
        if self.state != DeviceState::Recovering {
            return Err(CoreError::WrongState);
        }
        self.state = DeviceState::Probed;
        self.vdevs.clear();
        self.uncertain_vdev_starts.clear();
        self.peers.clear();
        self.installed_keys.clear();
        self.uncertain_key_peers.clear();
        self.attach_firmware()
    }
}

impl<B: Subsystems> Lifecycle for Device<B> {
    fn probe(&mut self) -> Result<(), CoreError> {
        if self.state != DeviceState::Allocated {
            return Err(CoreError::WrongState);
        }
        self.op(Operation::QmiInitService)?;
        if let Err(error) = self.op(Operation::HifPowerUp) {
            let _ = self.op(Operation::QmiDeinitService);
            return Err(error);
        }
        self.state = DeviceState::Probed;
        Ok(())
    }

    fn attach_firmware(&mut self) -> Result<FirmwareReady, CoreError> {
        if self.state != DeviceState::Probed {
            return Err(CoreError::WrongState);
        }
        let ready = self.backend.wait_for_firmware_ready()?;
        self.op(Operation::QmiFirmwareStart)?;
        if let Err(error) = self.op(Operation::CeInitPipes) {
            let _ = self.op(Operation::QmiFirmwareStop);
            return Err(error);
        }
        if let Err(error) = self.op(Operation::DpAllocate) {
            let _ = self.op(Operation::QmiFirmwareStop);
            return Err(error);
        }
        if let Err(error) = self.core_start() {
            let _ = self.op(Operation::DpFree);
            let _ = self.op(Operation::QmiFirmwareStop);
            return Err(error);
        }
        if let Err(error) = self.op(Operation::DpPdevAllocate) {
            self.core_stop(false);
            let _ = self.op(Operation::MacDestroy);
            let _ = self.op(Operation::DpFree);
            let _ = self.op(Operation::QmiFirmwareStop);
            return Err(error);
        }
        if let Err(error) = self.op(Operation::MacRegister) {
            let _ = self.op(Operation::DpPdevFree);
            self.core_stop(false);
            let _ = self.op(Operation::MacDestroy);
            let _ = self.op(Operation::DpFree);
            let _ = self.op(Operation::QmiFirmwareStop);
            return Err(error);
        }
        let _ = self.op(Operation::HifIrqEnable);
        self.firmware = Some(ready);
        self.state = DeviceState::Ready;
        Ok(ready)
    }

    fn start_radio(&mut self) -> Result<(), CoreError> {
        if self.state != DeviceState::Ready {
            return Err(CoreError::WrongState);
        }
        self.op(Operation::RadioStart)
    }

    fn stop(&mut self) -> Result<(), CoreError> {
        if !matches!(self.state, DeviceState::Ready | DeviceState::Recovering) {
            return Err(CoreError::WrongState);
        }
        let _ = self.op(Operation::MacUnregister);
        // Do not dismantle firmware-visible RX rings without a suspend ACK.
        self.op(Operation::PdevSuspend)?;
        let _ = self.op(Operation::HifIrqDisable);
        let _ = self.op(Operation::DpPdevFree);
        self.core_stop(self.state == DeviceState::Recovering);
        let _ = self.op(Operation::HifPowerDown);
        let _ = self.op(Operation::MacDestroy);
        let _ = self.op(Operation::DpFree);
        let _ = self.op(Operation::RegFree);
        let _ = self.op(Operation::QmiDeinitService);
        self.vdevs.clear();
        self.peers.clear();
        self.installed_keys.clear();
        self.uncertain_key_peers.clear();
        self.state = DeviceState::Stopped;
        Ok(())
    }
}

impl<B: Subsystems> RadioControl for Device<B> {
    fn create_client_vdev(&mut self, mac: [u8; 6]) -> Result<VdevId, CoreError> {
        if self.state != DeviceState::Ready {
            return Err(CoreError::WrongState);
        }
        if self.vdevs.len() >= WCN6750.params().num_vdevs as usize {
            return Err(CoreError::NoResources);
        }
        let id = VdevId(
            (0..WCN6750.params().num_vdevs)
                .find(|id| !self.has_vdev(VdevId(*id)))
                .ok_or(CoreError::NoResources)?,
        );
        self.op(Operation::WmiVdevCreate { vdev: id, mac })?;
        let configuration = [
            Operation::WmiVdevSetNss {
                vdev: id,
                nss: self.backend.client_nss()?,
            },
            Operation::WmiStaPsRxWake { vdev: id },
            Operation::WmiStaPsTxWake { vdev: id },
            Operation::WmiStaPsPollCount { vdev: id },
            Operation::WmiStaPsDisable { vdev: id },
        ];
        for operation in configuration {
            if let Err(error) = self.op(operation) {
                let _ = self.op(Operation::WmiVdevDelete { vdev: id });
                let _ = self.op(Operation::WaitVdevDeleted { vdev: id });
                return Err(error);
            }
        }
        // mac.c warns but deliberately retains the vdev if RTS setup fails.
        self.op(Operation::WmiVdevSetRtsThreshold {
            vdev: id,
            threshold: u32::MAX,
        })
        .ok();
        let _ = self.op(Operation::DpVdevTxAttach { vdev: id });
        self.vdevs.push(Vdev {
            id,
            mac,
            started: false,
            up: false,
            channel: None,
        });
        Ok(id)
    }

    fn start_vdev(&mut self, vdev: VdevId, regulatory: RegulatoryChannel) -> Result<(), CoreError> {
        if self.state != DeviceState::Ready || !self.has_vdev(vdev) {
            return Err(CoreError::WrongState);
        }
        let restart = self
            .vdevs
            .iter()
            .find(|item| item.id == vdev)
            .ok_or(CoreError::WrongState)?
            .started;
        if self.uncertain_vdev_starts.contains(&vdev) {
            return Err(CoreError::WrongState);
        }
        let channel = Channel::client_20mhz(regulatory);
        match self.backend.execute_vdev_start(vdev, restart, channel) {
            Ok(()) => {}
            Err(VdevStartFailure::NotSent(error) | VdevStartFailure::Rejected(error)) => {
                return Err(error);
            }
            Err(VdevStartFailure::Ambiguous(error)) => {
                self.uncertain_vdev_starts.push(vdev);
                return Err(error);
            }
        }
        if let Some(item) = self.vdevs.iter_mut().find(|item| item.id == vdev) {
            item.started = true;
            item.channel = Some(channel);
        }
        Ok(())
    }

    fn create_peer(&mut self, vdev: VdevId, address: [u8; 6]) -> Result<(), CoreError> {
        if !self.has_vdev(vdev) {
            return Err(CoreError::WrongState);
        }
        if self.peers.len() >= WCN6750.params().num_peers as usize {
            return Err(CoreError::NoResources);
        }
        if self.peers.contains(&(vdev, address)) {
            return Err(CoreError::Protocol);
        }
        self.op(Operation::WmiPeerCreate { vdev, address })?;
        self.op(Operation::WaitPeerCreated { vdev, address })?;
        if let Err(error) = self.op(Operation::DpPeerSetup { vdev, address }) {
            let _ = self.op(Operation::DpPeerCleanup { vdev, address });
            let _ = self.op(Operation::WmiPeerDelete { vdev, address });
            let _ = self.op(Operation::WaitPeerDeleted { vdev, address });
            return Err(error);
        }
        self.peers.push((vdev, address));
        Ok(())
    }

    fn delete_peer(&mut self, vdev: VdevId, address: [u8; 6]) -> Result<(), CoreError> {
        let index = self
            .peers
            .iter()
            .position(|peer| *peer == (vdev, address))
            .ok_or(CoreError::NotFound)?;
        let cleanup = self.op(Operation::DpPeerCleanup { vdev, address });
        self.op(Operation::WmiPeerDelete { vdev, address })?;
        self.op(Operation::WaitPeerDeleted { vdev, address })?;
        cleanup?;
        self.peers.remove(index);
        self.installed_keys
            .retain(|key| key.vdev != vdev || key.peer != address);
        self.uncertain_key_peers
            .retain(|peer| *peer != (vdev, address));
        Ok(())
    }
}

impl<B: Subsystems> ClientRadioControl for Device<B> {
    fn up_vdev(&mut self, vdev: VdevId, bssid: [u8; 6], aid: u16) -> Result<(), CoreError> {
        self.op(Operation::WmiVdevUp { vdev, bssid, aid })?;
        // ath11k_bss_assoc continues with best-effort OBSS and DTIM setup.
        self.op(Operation::WmiObssSpatialReuse { vdev }).ok();
        self.op(Operation::WmiDtimPolicyStick { vdev }).ok();
        if let Some(item) = self.vdevs.iter_mut().find(|item| item.id == vdev) {
            item.up = true;
        }
        Ok(())
    }
    fn down_vdev(&mut self, vdev: VdevId) -> Result<(), CoreError> {
        self.op(Operation::WmiVdevDown { vdev })?;
        if let Some(item) = self.vdevs.iter_mut().find(|item| item.id == vdev) {
            item.up = false;
        }
        Ok(())
    }
    fn stop_vdev(&mut self, vdev: VdevId) -> Result<(), CoreError> {
        self.op(Operation::WmiVdevStop { vdev })?;
        self.op(Operation::WaitVdevSetup { vdev })?;
        if let Some(item) = self.vdevs.iter_mut().find(|item| item.id == vdev) {
            item.started = false;
        }
        Ok(())
    }
    fn delete_vdev(&mut self, vdev: VdevId) -> Result<(), CoreError> {
        let index = self
            .vdevs
            .iter()
            .position(|item| item.id == vdev)
            .ok_or(CoreError::NotFound)?;
        self.op(Operation::WmiVdevDelete { vdev })?;
        self.op(Operation::WaitVdevDeleted { vdev })?;
        self.vdevs.remove(index);
        self.peers.retain(|peer| peer.0 != vdev);
        self.installed_keys.retain(|key| key.vdev != vdev);
        self.uncertain_key_peers.retain(|peer| peer.0 != vdev);
        Ok(())
    }
    fn associate_peer(&mut self, association: PeerAssociation) -> Result<(), CoreError> {
        let vdev = association.vdev;
        let address = association.peer;
        if !self.peers.contains(&(vdev, address)) {
            return Err(CoreError::NotFound);
        }
        let channel = self
            .vdevs
            .iter()
            .find(|item| item.id == vdev && item.started)
            .and_then(|item| item.channel)
            .ok_or(CoreError::WrongState)?;
        if channel.primary_mhz != association.primary_mhz {
            return Err(CoreError::WrongState);
        }
        let smps = association.ht_capabilities.map(|cap| {
            match (u16::from_le_bytes([cap[0], cap[1]]) >> 2) & 3 {
                0 => 1,
                1 => 2,
                3 => 0,
                _ => 0,
            }
        });
        let wmm = association.wmm;
        self.op(Operation::WmiPeerAssociate(association))?;
        self.op(Operation::WaitPeerAssociated { vdev, address })?;
        if let Some(wmm) = wmm {
            self.op(Operation::WmiWmmUpdate { vdev, wmm })?;
        }
        if let Some(mode) = smps {
            self.op(Operation::WmiPeerSetSmps {
                vdev,
                address,
                mode,
            })?;
        }
        Ok(())
    }
    fn install_key(&mut self, key: KeyConfig) -> Result<(), CoreError> {
        if !self.has_vdev(key.vdev) {
            return Err(CoreError::NotFound);
        }
        if !self.peers.contains(&(key.vdev, key.peer)) {
            return Err(CoreError::NotFound);
        }
        if self.uncertain_key_peers.contains(&(key.vdev, key.peer)) {
            return Err(CoreError::WrongState);
        }
        if self.installed_keys.contains(&key) {
            // A completed synchronous retry must not reset the hardware replay
            // counter by reissuing the REO PN update.
            return Ok(());
        }
        if matches!(
            key.cipher,
            Cipher::BipCmac128 | Cipher::BipGmac128 | Cipher::BipGmac256
        ) {
            return Err(CoreError::Protocol);
        }
        if key.protection != KeyProtection::RxTx {
            return Err(CoreError::Protocol);
        }
        let expected_len = match key.cipher {
            Cipher::Ccmp128 | Cipher::Gcmp128 => 16,
            Cipher::Ccmp256 | Cipher::Gcmp256 | Cipher::Tkip => 32,
            _ => return Err(CoreError::Protocol),
        };
        if key.index > 3
            || key.bytes.len() != expected_len
            || key.receive_sequence_counter > 0x0000_ffff_ffff_ffff
        {
            return Err(CoreError::Protocol);
        }
        self.op(Operation::WmiInstallKey(key.clone()))?;
        if let Err(error) = self.op(Operation::WaitKeyInstalled {
            vdev: key.vdev,
            key_index: key.index,
        }) {
            self.uncertain_key_peers.push((key.vdev, key.peer));
            return Err(error);
        }
        if let Err(error) = self.op(Operation::DpInstallPeerKey(key.clone())) {
            self.uncertain_key_peers.push((key.vdev, key.peer));
            return Err(error);
        }
        self.installed_keys
            .retain(|installed| !(installed.vdev == key.vdev && installed.kind == key.kind));
        self.installed_keys.push(key);
        Ok(())
    }
    fn set_peer_authorized(
        &mut self,
        vdev: VdevId,
        address: [u8; 6],
        authorized: bool,
    ) -> Result<(), CoreError> {
        if !self.peers.contains(&(vdev, address)) {
            return Err(CoreError::NotFound);
        }
        self.op(Operation::WmiPeerAuthorize {
            vdev,
            address,
            authorized,
        })
    }
    fn start_scan(&mut self, scan: ScanConfig) -> Result<(), CoreError> {
        if !self.has_vdev(scan.vdev) {
            return Err(CoreError::NotFound);
        }
        self.op(Operation::WmiScanStart(scan))
    }
    fn stop_scan(&mut self, vdev: VdevId, scan: ScanId) -> Result<(), CoreError> {
        self.op(Operation::WmiScanStop { vdev, scan })
    }
    fn transmit_management(&mut self, frame: ManagementFrame) -> Result<(), CoreError> {
        match self.vdevs.iter().find(|vdev| vdev.id == frame.vdev) {
            Some(vdev) if vdev.started => {}
            Some(_) => return Err(CoreError::WrongState),
            None => return Err(CoreError::NotFound),
        }
        self.op(Operation::WmiMgmtTx(frame))
    }
    fn set_tx_power(&mut self, dbm: i8) -> Result<(), CoreError> {
        self.op(Operation::WmiPdevSetTxPower {
            pdev: PdevId(0),
            half_dbm: i16::from(dbm) * 2,
        })
    }
    fn set_regulatory_domain(&mut self, domain: RegulatoryDomain) -> Result<(), CoreError> {
        if domain.channels.is_empty() {
            return Err(CoreError::Protocol);
        }
        self.op(Operation::WmiSetCurrentCountry {
            alpha2: domain.alpha2,
        })?;
        self.op(Operation::WaitRegulatoryUpdate { pdev: PdevId(0) })?;
        self.op(Operation::WmiScanChannelList {
            pdev: PdevId(0),
            channels: domain.channels,
        })
    }
}

#[cfg(test)]
mod tests;
