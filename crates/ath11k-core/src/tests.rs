use super::*;
use alloc::{vec, vec::Vec};

#[derive(Default)]
struct Model {
    log: Vec<Operation>,
    fail: Option<Operation>,
    vdev_start_failure: Option<VdevStartFailure>,
}

impl Subsystems for Model {
    fn execute_vdev_start(
        &mut self,
        vdev: VdevId,
        restart: bool,
        channel: Channel,
    ) -> Result<(), VdevStartFailure> {
        self.log.push(Operation::WmiVdevStart {
            vdev,
            restart,
            channel,
        });
        if let Some(error) = self.vdev_start_failure.take() {
            return Err(error);
        }
        self.execute(Operation::WaitVdevSetup { vdev })
            .map_err(VdevStartFailure::Ambiguous)
    }

    fn execute(&mut self, operation: Operation) -> Result<(), CoreError> {
        self.log.push(operation.clone());
        if self.fail.as_ref() == Some(&operation) {
            self.fail = None;
            Err(CoreError::DeviceFault)
        } else {
            Ok(())
        }
    }
    fn wait_for_firmware_ready(&mut self) -> Result<FirmwareReady, CoreError> {
        self.execute(Operation::QmiWaitFirmwareReady)?;
        Ok(FirmwareReady {
            firmware_version: 1,
            target_mem_mode: 0,
        })
    }

    fn next_wlan_event(&mut self) -> Result<Option<WlanEvent>, CoreError> {
        Ok(None)
    }
    fn client_nss(&self) -> Result<u8, CoreError> {
        Ok(2)
    }
}

fn ready_device() -> Device<Model> {
    let mut device = WCN6750.device(Model::default());
    device.probe().unwrap();
    device.attach_firmware().unwrap();
    device
}

#[test]
fn wcn6750_parameters_and_static_windows_match_source() {
    let params = WCN6750.params();
    assert_eq!(
        (params.ce_count, params.num_vdevs, params.num_peers),
        (9, 3, 512)
    );
    assert_eq!(RegisterWindow::for_offset(0x00a0_1234), RegisterWindow::Dp);
    assert_eq!(RegisterWindow::Dp.mapped_offset(0x00a0_1234), 0x0008_1234);
    assert_eq!(
        RegisterWindow::for_offset(0x01b8_1234),
        RegisterWindow::CopyEngine
    );
    assert_eq!(
        RegisterWindow::CopyEngine.mapped_offset(0x01b8_1234),
        0x0010_1234
    );
    // Direct HIF, REO/TCL/WBM DP, and the complete CE0..CE11 register
    // ranges used by the current HAL/CE/DP implementations must fit the
    // exact 2 MiB aperture returned by QMI DEVICE_INFO.
    for raw in [
        0x0000_0000,
        0x0007_fffc,
        0x00a3_4000,
        0x00a3_b028,
        0x00a4_4000,
        0x00a4_49fc,
        0x01b8_0000,
        0x01b9_7058,
    ] {
        assert!(wcn6750_register_offset(raw).unwrap() < 0x20_0000);
    }
    assert_eq!(wcn6750_register_offset(0x0007_fffc), Some(0x0007_fffc));
    assert_eq!(wcn6750_register_offset(0x00a3_8000), Some(0x000b_8000));
    assert_eq!(wcn6750_register_offset(0x01b8_0000), Some(0x0010_0000));
    assert_eq!(
        usize::try_from(0x1_0000_0000_u64)
            .ok()
            .and_then(wcn6750_register_offset),
        None
    );
    assert_eq!(WCN6750_INTERRUPT_ROUTES.len(), 16);
    assert!(
        WCN6750_INTERRUPT_ROUTES[..7]
            .iter()
            .all(|route| route.user == MsiUser::CopyEngine)
    );
    assert!(
        WCN6750_INTERRUPT_ROUTES[7..]
            .iter()
            .all(|route| route.user == MsiUser::DataPath)
    );
    assert_eq!(
        WCN6750_INTERRUPT_ROUTES
            .iter()
            .map(|route| (route.vector, route.irq))
            .collect::<Vec<_>>(),
        vec![
            (0, Wcn6750Irq::CopyEngine(0)),
            (1, Wcn6750Irq::CopyEngine(1)),
            (2, Wcn6750Irq::CopyEngine(2)),
            (3, Wcn6750Irq::CopyEngine(3)),
            (4, Wcn6750Irq::CopyEngine(5)),
            (5, Wcn6750Irq::CopyEngine(7)),
            (6, Wcn6750Irq::CopyEngine(8)),
            (10, Wcn6750Irq::DataPathExternalGroup(0)),
            (11, Wcn6750Irq::DataPathExternalGroup(1)),
            (12, Wcn6750Irq::DataPathExternalGroup(2)),
            (14, Wcn6750Irq::DataPathExternalGroup(4)),
            (16, Wcn6750Irq::DataPathExternalGroup(6)),
            (17, Wcn6750Irq::DataPathExternalGroup(7)),
            (18, Wcn6750Irq::DataPathExternalGroup(8)),
            (19, Wcn6750Irq::DataPathExternalGroup(9)),
            (20, Wcn6750Irq::DataPathExternalGroup(10)),
        ]
    );
    let mask = WCN6750.ring_mask();
    let active_groups = (0..11)
        .filter(|&group| {
            mask.tx[group] != 0
                || mask.rx_mon_status[group] != 0
                || mask.rx[group] != 0
                || mask.rx_err[group] != 0
                || mask.rx_wbm_release[group] != 0
                || mask.reo_status[group] != 0
                || mask.rxdma_to_host[group] != 0
        })
        .collect::<Vec<_>>();
    assert_eq!(active_groups, vec![0, 1, 2, 4, 6, 7, 8, 9, 10]);
    assert_eq!(
        WCN6750_DP_INTERRUPT_ROUTES
            .iter()
            .map(|route| match route.irq {
                Wcn6750Irq::DataPathExternalGroup(group) => usize::from(group),
                Wcn6750Irq::CopyEngine(_) => unreachable!(),
            })
            .collect::<Vec<_>>(),
        active_groups
    );
}

#[test]
fn client_20mhz_channel_words_match_wmi_c_bitfields() {
    let channel = Channel::client_20mhz(RegulatoryChannel {
        frequency_mhz: 2437,
        max_power_dbm: 20,
        max_reg_power_dbm: 18,
        max_antenna_gain_dbi: 6,
        passive: true,
        radar: false,
        allow_ht: true,
        allow_vht: true,
        allow_he: true,
    });
    assert_eq!(
        (
            channel.primary_mhz,
            channel.center1_mhz,
            channel.center2_mhz
        ),
        (2437, 2437, 0)
    );
    // The pinned station path sets only phymode and NO_IR/passive. DFS and
    // allow-HT/VHT/HE are left zero even though regulatory metadata retains
    // those facts for other command paths.
    assert_eq!(channel.info, 21 | (1 << 7));
    assert_eq!(channel.reg_info_1, (20 << 8) | (18 << 16));
    assert_eq!(channel.reg_info_2, 6 | (20 << 8));
}

#[test]
fn firmware_ready_runs_core_c_order() {
    let device = ready_device();
    assert_eq!(device.state(), DeviceState::Ready);
    assert_eq!(
        device.backend().log,
        vec![
            Operation::QmiInitService,
            Operation::HifPowerUp,
            Operation::QmiWaitFirmwareReady,
            Operation::QmiFirmwareStart,
            Operation::CeInitPipes,
            Operation::DpAllocate,
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
            Operation::DpPdevAllocate,
            Operation::MacRegister,
            Operation::HifIrqEnable,
        ]
    );
}

#[test]
fn client_sequence_preserves_mac_c_wmi_order() {
    let mut device = ready_device();
    device.backend_mut().log.clear();
    let mac = [2, 0, 0, 0, 0, 1];
    let bssid = [2, 0, 0, 0, 0, 2];
    let channel = RegulatoryChannel {
        frequency_mhz: 5180,
        max_power_dbm: 23,
        max_reg_power_dbm: 23,
        max_antenna_gain_dbi: 6,
        passive: false,
        radar: false,
        allow_ht: true,
        allow_vht: true,
        allow_he: true,
    };
    let vdev = device.create_client_vdev(mac).unwrap();
    device.start_vdev(vdev, channel).unwrap();
    device
        .start_scan(ScanConfig {
            vdev,
            id: ScanId(7),
            active: true,
            channels_mhz: vec![5180],
            ssids: vec![b"ap".to_vec()],
        })
        .unwrap();
    device.stop_scan(vdev, ScanId(7)).unwrap();
    device.create_peer(vdev, bssid).unwrap();
    let association = PeerAssociation {
        vdev,
        peer: bssid,
        aid: 42,
        listen_interval: 0,
        primary_mhz: 5180,
        bandwidth: AssociationBandwidth::Bw20,
        capability_info: 0x421,
        legacy_rates: vec![12, 18, 24, 36, 48, 72, 96, 108],
        qos: false,
        ht_capabilities: None,
        vht_capabilities: None,
        wmm: None,
    };
    device.associate_peer(association.clone()).unwrap();
    device.up_vdev(vdev, bssid, 42).unwrap();
    device
        .install_key(KeyConfig {
            vdev,
            peer: bssid,
            index: 0,
            cipher: Cipher::Ccmp128,
            kind: KeyKind::Pairwise,
            protection: KeyProtection::RxTx,
            receive_sequence_counter: 0,
            bytes: vec![0x55; 16],
        })
        .unwrap();

    device.set_peer_authorized(vdev, bssid, true).unwrap();

    assert_eq!(
        device.backend().log,
        vec![
            Operation::WmiVdevCreate { vdev, mac },
            Operation::WmiVdevSetNss { vdev, nss: 2 },
            Operation::WmiStaPsRxWake { vdev },
            Operation::WmiStaPsTxWake { vdev },
            Operation::WmiStaPsPollCount { vdev },
            Operation::WmiStaPsDisable { vdev },
            Operation::WmiVdevSetRtsThreshold {
                vdev,
                threshold: u32::MAX
            },
            Operation::DpVdevTxAttach { vdev },
            Operation::WmiVdevStart {
                vdev,
                restart: false,
                channel: Channel::client_20mhz(channel)
            },
            Operation::WaitVdevSetup { vdev },
            Operation::WmiScanStart(ScanConfig {
                vdev,
                id: ScanId(7),
                active: true,
                channels_mhz: vec![5180],
                ssids: vec![b"ap".to_vec()],
            }),
            Operation::WmiScanStop {
                vdev,
                scan: ScanId(7)
            },
            Operation::WmiPeerCreate {
                vdev,
                address: bssid
            },
            Operation::WaitPeerCreated {
                vdev,
                address: bssid
            },
            Operation::WmiPeerAssociate(association),
            Operation::WaitPeerAssociated {
                vdev,
                address: bssid
            },
            Operation::WmiVdevUp {
                vdev,
                bssid,
                aid: 42
            },
            Operation::WmiObssSpatialReuse { vdev },
            Operation::WmiDtimPolicyStick { vdev },
            Operation::WmiInstallKey(KeyConfig {
                vdev,
                peer: bssid,
                index: 0,
                cipher: Cipher::Ccmp128,
                kind: KeyKind::Pairwise,
                protection: KeyProtection::RxTx,
                receive_sequence_counter: 0,
                bytes: vec![0x55; 16],
            }),
            Operation::WaitKeyInstalled { vdev, key_index: 0 },
            Operation::WmiPeerAuthorize {
                vdev,
                address: bssid,
                authorized: true,
            },
        ]
    );
}

#[test]
fn repeated_channel_set_uses_vdev_restart_only_after_completed_start() {
    let mut device = ready_device();
    let vdev = device.create_client_vdev([2, 0, 0, 0, 0, 1]).unwrap();
    let channel = RegulatoryChannel {
        frequency_mhz: 2437,
        max_power_dbm: 20,
        max_reg_power_dbm: 20,
        max_antenna_gain_dbi: 0,
        passive: false,
        radar: false,
        allow_ht: true,
        allow_vht: true,
        allow_he: true,
    };
    device.backend_mut().log.clear();
    device.start_vdev(vdev, channel).unwrap();
    device.start_vdev(vdev, channel).unwrap();
    assert_eq!(
        device.backend().log,
        vec![
            Operation::WmiVdevStart {
                vdev,
                restart: false,
                channel: Channel::client_20mhz(channel),
            },
            Operation::WaitVdevSetup { vdev },
            Operation::WmiVdevStart {
                vdev,
                restart: true,
                channel: Channel::client_20mhz(channel),
            },
            Operation::WaitVdevSetup { vdev },
        ]
    );
}

#[test]
fn failed_vdev_start_completion_makes_the_vdev_uncertain() {
    let mut device = ready_device();
    let vdev = device.create_client_vdev([2, 0, 0, 0, 0, 1]).unwrap();
    let channel = RegulatoryChannel {
        frequency_mhz: 2437,
        max_power_dbm: 20,
        max_reg_power_dbm: 20,
        max_antenna_gain_dbi: 0,
        passive: false,
        radar: false,
        allow_ht: true,
        allow_vht: true,
        allow_he: true,
    };
    device.backend_mut().fail = Some(Operation::WaitVdevSetup { vdev });
    assert_eq!(
        device.start_vdev(vdev, channel),
        Err(CoreError::DeviceFault)
    );
    device.backend_mut().log.clear();
    device.backend_mut().fail = None;
    assert_eq!(device.start_vdev(vdev, channel), Err(CoreError::WrongState));
    assert!(device.backend().log.is_empty());
}

#[test]
fn uncertain_start_id_remains_quarantined_after_delete_and_recreate() {
    let mut device = ready_device();
    let mac = [2, 0, 0, 0, 0, 1];
    let vdev = device.create_client_vdev(mac).unwrap();
    let channel = RegulatoryChannel {
        frequency_mhz: 2437,
        max_power_dbm: 20,
        max_reg_power_dbm: 20,
        max_antenna_gain_dbi: 0,
        passive: false,
        radar: false,
        allow_ht: true,
        allow_vht: true,
        allow_he: true,
    };
    device.backend_mut().vdev_start_failure =
        Some(VdevStartFailure::Ambiguous(CoreError::Protocol));
    assert_eq!(device.start_vdev(vdev, channel), Err(CoreError::Protocol));
    device.delete_vdev(vdev).unwrap();
    let replacement = device.create_client_vdev(mac).unwrap();
    assert_eq!(replacement, vdev);
    device.backend_mut().log.clear();
    assert_eq!(
        device.start_vdev(replacement, channel),
        Err(CoreError::WrongState)
    );
    assert!(device.backend().log.is_empty());
}

#[test]
fn definitive_start_failures_remain_retryable() {
    for (failure, expected) in [
        (
            VdevStartFailure::NotSent(CoreError::DeviceFault),
            CoreError::DeviceFault,
        ),
        (
            VdevStartFailure::Rejected(CoreError::Protocol),
            CoreError::Protocol,
        ),
    ] {
        let mut device = ready_device();
        let vdev = device.create_client_vdev([2, 0, 0, 0, 0, 1]).unwrap();
        let channel = RegulatoryChannel {
            frequency_mhz: 2437,
            max_power_dbm: 20,
            max_reg_power_dbm: 20,
            max_antenna_gain_dbi: 0,
            passive: false,
            radar: false,
            allow_ht: true,
            allow_vht: true,
            allow_he: true,
        };
        device.backend_mut().vdev_start_failure = Some(failure);
        assert_eq!(device.start_vdev(vdev, channel), Err(expected));
        device.start_vdev(vdev, channel).unwrap();
    }
}

#[test]
fn core_start_error_unwinds_without_panicking() {
    let model = Model {
        fail: Some(Operation::WmiCommandInit),
        ..Default::default()
    };
    let mut device = WCN6750.device(model);
    device.probe().unwrap();
    assert_eq!(device.attach_firmware(), Err(CoreError::DeviceFault));
    assert_eq!(device.state(), DeviceState::Probed);
    assert!(device.backend().log.ends_with(&[
        Operation::DpReoCleanup,
        Operation::MacDestroy,
        Operation::HifStop,
        Operation::WmiDetach,
        Operation::DpFree,
        Operation::QmiFirmwareStop,
    ]));
}

#[test]
fn crash_and_teardown_follow_distinct_paths() {
    let mut device = ready_device();
    device.backend_mut().log.clear();
    device.firmware_crashed().unwrap();
    assert_eq!(device.state(), DeviceState::Recovering);
    assert_eq!(
        device.backend().log,
        vec![
            Operation::RecoveryQuiesce,
            Operation::HifStop,
            Operation::WmiDetach,
            Operation::DpReoCleanup,
            Operation::RecoveryRestart,
        ]
    );
    device.complete_recovery().unwrap();
    device.stop().unwrap();
    assert_eq!(device.state(), DeviceState::Stopped);
}

#[test]
fn vdev_resource_limit_is_the_firmware_limit() {
    let mut device = ready_device();
    for last in 0..3 {
        device.create_client_vdev([2, 0, 0, 0, 0, last]).unwrap();
    }
    assert_eq!(
        device.create_client_vdev([2, 0, 0, 0, 0, 9]),
        Err(CoreError::NoResources)
    );
}

#[test]
fn vdev_configuration_failure_deletes_firmware_vdev() {
    let mut device = ready_device();
    device.backend_mut().log.clear();
    device.backend_mut().fail = Some(Operation::WmiVdevSetNss {
        vdev: VdevId(0),
        nss: 2,
    });
    assert_eq!(
        device.create_client_vdev([2, 0, 0, 0, 0, 1]),
        Err(CoreError::DeviceFault)
    );
    assert!(device.backend().log.ends_with(&[
        Operation::WmiVdevDelete { vdev: VdevId(0) },
        Operation::WaitVdevDeleted { vdev: VdevId(0) },
    ]));
    assert_eq!(device.create_client_vdev([2, 0, 0, 0, 0, 2]), Ok(VdevId(0)));
}

#[test]
fn qmi_memory_provider_returns_only_allocation_derived_iovas() {
    use ath11k_qmi::{
        MemoryProvider,
        wire::{MemorySegment, MemoryType},
    };
    use drv_hardware_backends::DeterministicBackend;

    let mut provider = HardwareMemoryProvider::new(DeterministicBackend::device(), 0);
    let responses = provider
        .provision(&[MemorySegment {
            size: 4096,
            kind: MemoryType(1),
            configs: Vec::new(),
        }])
        .unwrap();
    assert_eq!(provider.allocations(), 1);
    assert_eq!(responses[0].address, 0x1000_0000);
    assert_eq!(responses[0].size, 4096);
    assert_eq!(responses[0].restore, 0);
}

#[test]
fn qmi_memory_provider_uses_selected_exact_bar_window() {
    use ath11k_qmi::MemoryProvider;
    use drv_hardware_backends::DeterministicBackend;

    let (device, _) = DeterministicBackend::recording_device_with_region_len(0x20_0000);
    let mut provider = HardwareMemoryProvider::new(device, 0);
    assert!(provider.map_device_bar(0x1000_0000, 0x20_0000).is_ok());
    assert_eq!(provider.device_bar().unwrap().len(), 0x20_0000);
    assert_eq!(provider.take_device_bar().unwrap().len(), 0x20_0000);
    assert!(provider.device_bar().is_none());

    let (device, _) = DeterministicBackend::recording_device_with_region_len(0x10_0000);
    let mut wrong_size = HardwareMemoryProvider::new(device, 0);
    assert_eq!(
        wrong_size.map_device_bar(0x1000_0000, 0x20_0000),
        Err(ath11k_qmi::QmiError::Transport)
    );
    let mut wrong_index = HardwareMemoryProvider::new(DeterministicBackend::device(), 1);
    assert_eq!(
        wrong_index.map_device_bar(0x1000_0000, 0x10_0000),
        Err(ath11k_qmi::QmiError::Transport)
    );
}

#[test]
fn qmi_memory_provider_can_discover_bar_without_mapping_doorbell_region() {
    use ath11k_qmi::MemoryProvider;
    use drv_hardware_backends::DeterministicBackend;

    let mut provider = HardwareMemoryProvider::discover_device_bar(DeterministicBackend::device());
    assert!(provider.map_device_bar(0x1234_0000, 0x20_0000).is_ok());
    assert_eq!(
        provider.device_bar_request(),
        Some((0x1234_0000, 0x20_0000))
    );
    assert!(provider.device_bar().is_none());
}

#[test]
fn wmi_events_cross_the_wlansoftmac_seam_without_policy() {
    let mgmt = ath11k_wmi::event::MgmtRx {
        channel: 36,
        snr: 42,
        rate: 0,
        phy_mode: 0,
        status: 0,
        flags: 3,
        rssi: -55,
        tsf_delta: 0,
        pdev_id: 0,
        channel_freq: 5180,
        frame: vec![0x80, 0x00],
    };
    assert_eq!(
        WlanEvent::from(mgmt),
        WlanEvent::ManagementReceived {
            pdev_id: 0,
            channel_mhz: 5180,
            snr: 42,
            rssi: -55,
            flags: 3,
            frame: vec![0x80, 0x00],
        }
    );
    assert!(matches!(
        WlanEvent::try_from(ath11k_wmi::event::Roam {
            vdev_id: 0,
            reason: 2,
            rssi: 10
        }),
        Ok(WlanEvent::BeaconLoss { vdev_id: 0, .. })
    ));
    assert_eq!(
        WlanEvent::try_from(ath11k_wmi::event::Roam {
            vdev_id: 0,
            reason: 1,
            rssi: 10
        }),
        Err(CoreError::Protocol)
    );
}

#[test]
fn actual_wmi_and_htt_share_one_htc_router() {
    use alloc::collections::VecDeque;
    use alloc::rc::Rc;
    use ath11k_ce::{CeError, Htc, HtcHeader, HtcPacketIo, HtcRouter, HtcTransport, ServiceId};
    use ath11k_dp::{
        HttControl,
        htt::{request_target_version, version_request},
        transport::ath11k_dp_htt_connect_service,
    };
    use ath11k_hal::RingId;
    use ath11k_wmi::{
        WmiError,
        cmd::{HtcWmiTransport, Init, Wmi},
        tags::{
            WMI_INIT_CMDID, WMI_READY_EVENTID, WMI_SERVICE_READY_EVENTID,
            WMI_SERVICE_READY_EXT_EVENTID, WMI_TAG_SERVICE_READY_EVENT,
            WMI_TAG_SERVICE_READY_EXT_EVENT,
        },
    };
    use core::cell::RefCell;

    #[derive(Default)]
    struct FakeHtcPeer {
        incoming: VecDeque<Vec<u8>>,
        outgoing: Rc<RefCell<Vec<Vec<u8>>>>,
    }
    impl HtcPacketIo for FakeHtcPeer {
        fn send_htc(&mut self, _: u8, _: u16, frame: Vec<u8>) -> Result<(), CeError> {
            self.outgoing.borrow_mut().push(frame);
            Ok(())
        }
        fn receive_htc(&mut self, _: u64) -> Result<Option<Vec<u8>>, CeError> {
            Ok(self.incoming.pop_front())
        }
    }
    fn htc_frame(endpoint: u8, payload: &[u8]) -> Vec<u8> {
        let header = HtcHeader {
            endpoint,
            flags: 0,
            payload_len: payload.len() as u16,
            control_byte_0: 0,
            control_byte_1: 0,
        };
        let mut frame = Vec::from(header.encode());
        frame.extend_from_slice(payload);
        frame
    }
    fn wmi_event(id: u32, tag: Option<(u16, usize)>) -> Vec<u8> {
        let mut payload = Vec::new();
        payload.extend_from_slice(&(id & 0x00ff_ffff).to_le_bytes());
        if let Some((tag, len)) = tag {
            payload.extend_from_slice(&((u32::from(tag) << 16) | len as u32).to_le_bytes());
            payload.resize(payload.len() + len, 0);
        }
        htc_frame(1, &payload)
    }

    let mut htc = Htc::new(1, true, true);
    htc.connect_service(ServiceId::RESERVED_CONTROL, &[])
        .unwrap();
    htc.wait_target(&[1, 0, 4, 0, 0, 8, 9, 0]).unwrap();
    htc.connect_service(
        ServiceId::WMI_CONTROL,
        &[3, 0, 0, 1, 0, 1, 0, 8, 0, 0, 0, 0],
    )
    .unwrap();
    htc.connect_service(
        ServiceId::HTT_DATA_MSG,
        &[3, 0, 0, 3, 0, 2, 0, 8, 0, 0, 0, 0],
    )
    .unwrap();

    let outgoing = Rc::new(RefCell::new(Vec::new()));
    let peer = FakeHtcPeer {
        incoming: VecDeque::from(vec![
            wmi_event(
                WMI_SERVICE_READY_EVENTID.0,
                Some((WMI_TAG_SERVICE_READY_EVENT.0, 128)),
            ),
            wmi_event(
                WMI_SERVICE_READY_EXT_EVENTID.0,
                Some((WMI_TAG_SERVICE_READY_EXT_EVENT.0, 76)),
            ),
            htc_frame(2, &[0, 7, 3, 0]),
            wmi_event(WMI_READY_EVENTID.0, None),
        ]),
        outgoing: outgoing.clone(),
    };
    let router = HtcRouter::new(HtcTransport::new(htc, peer));
    let wmi_endpoint = router
        .bind_service(ServiceId::WMI_CONTROL, RingId(3), RingId(2))
        .unwrap();
    let htt_endpoint = router
        .bind_service(ServiceId::HTT_DATA_MSG, RingId(4), RingId(1))
        .unwrap();
    let mut wmi = Wmi::attach(HtcWmiTransport::new(wmi_endpoint));
    let mut htt = ath11k_dp_htt_connect_service(htt_endpoint);
    wmi.pdev_attach(0);
    wmi.connect();
    assert_eq!(router.service_receive(1_000), Ok(4));
    let service = wmi.wait_for_service_ready(1_000).unwrap();
    assert!(service.service_ready.is_some());
    wmi.cmd_init(&Init {
        resource_config: Default::default(),
        memory_chunks: Vec::new(),
        hardware_mode: None,
        bands: Vec::new(),
    })
    .unwrap();
    wmi.wait_for_unified_ready(1_000).unwrap();
    assert_eq!(request_target_version(&mut htt, 1_000), Ok((3, 7)));
    // WCN6750's shadow-register quirk leaves WMI with one credit, consumed by
    // INIT above. HTT disables HTC credit flow in the connect request and can
    // continue bursting while another WMI command is rejected.
    for _ in 0..3 {
        htt.send(version_request()).unwrap();
    }
    assert_eq!(
        wmi.cmd_init(&Init {
            resource_config: Default::default(),
            memory_chunks: Vec::new(),
            hardware_mode: None,
            bands: Vec::new(),
        }),
        Err(WmiError::Transport)
    );

    let outgoing = outgoing.borrow();
    assert_eq!(outgoing.len(), 5);
    assert_eq!(HtcHeader::decode(&outgoing[0]).unwrap().endpoint, 1);
    assert_eq!(HtcHeader::decode(&outgoing[0]).unwrap().flags, 1);
    assert!(outgoing[1..].iter().all(|frame| {
        let header = HtcHeader::decode(frame).unwrap();
        header.endpoint == 2 && header.flags == 0
    }));
    let wmi_header = &outgoing[0][ath11k_ce::HTC_HEADER_LEN..][..4];
    assert_eq!(
        u32::from_le_bytes(wmi_header.try_into().unwrap()),
        WMI_INIT_CMDID.0
    );
    assert!(
        outgoing[1..]
            .iter()
            .all(|frame| frame[ath11k_ce::HTC_HEADER_LEN..] == [0, 0, 0, 0])
    );
}

#[test]
fn actual_qmi_handshake_runs_through_fake_qrtr_peer() {
    use alloc::collections::VecDeque;
    use ath11k_qmi::{
        FirmwareAssets, Incoming, MemoryProvider, MemoryRegion, QmiError, RawIndication, Request,
        Response, TransactionId, Transport,
        wire::{MemorySegment, MemorySegmentResponse, MessageId, WlanConfigRequest},
    };

    struct FakeQrtr {
        incoming: VecDeque<Incoming>,
        sent: Vec<MessageId>,
        transaction: u16,
        service: Option<(u32, u32)>,
    }
    impl Transport for FakeQrtr {
        fn start_service(&mut self, version: u32, instance: u32) -> Result<(), QmiError> {
            self.service = Some((version, instance));
            Ok(())
        }
        fn stop_service(&mut self) {
            self.service = None;
        }
        fn send(&mut self, request: Request) -> Result<TransactionId, QmiError> {
            self.sent.push(request.message_id());
            self.transaction += 1;
            Ok(TransactionId::new(self.transaction))
        }
        fn now_ns(&self) -> u64 {
            1
        }
        fn receive(&mut self, _: u64) -> Result<Incoming, QmiError> {
            self.incoming.pop_front().ok_or(QmiError::Timeout)
        }
    }
    struct FakeMemory {
        bar: Option<(u64, u32)>,
    }
    impl MemoryProvider for FakeMemory {
        fn provision(
            &mut self,
            _: &[MemorySegment],
        ) -> Result<Vec<MemorySegmentResponse>, QmiError> {
            Ok(Vec::new())
        }
        fn load_m3(&mut self, _: &[u8]) -> Result<MemoryRegion, QmiError> {
            Err(QmiError::Transport)
        }
        fn map_device_bar(&mut self, address: u64, size: u32) -> Result<(), QmiError> {
            self.bar = Some((address, size));
            Ok(())
        }
    }
    fn success(transaction: u16, id: MessageId) -> Incoming {
        Incoming::Response(
            Response::checked(
                TransactionId::new(transaction),
                id,
                vec![2, 4, 0, 0, 0, 0, 0],
            )
            .unwrap(),
        )
    }

    let capability = vec![
        2, 4, 0, 0, 0, 0, 0, 0x11, 4, 0, 7, 0, 0, 0, 0x13, 9, 0, 0x44, 0x33, 0x22, 0x11, 4, b't',
        b'i', b'm', b'e',
    ];
    let device_info = vec![
        2, 4, 0, 0, 0, 0, 0, 0x10, 8, 0, 0, 0, 0, 0x10, 0, 0, 0, 0, 0x11, 4, 0, 0, 0, 0x20, 0,
    ];
    let mut qrtr = FakeQrtr {
        incoming: VecDeque::from(vec![
            Incoming::ServerArrived,
            success(1, MessageId::IndicationRegister),
            success(2, MessageId::HostCapability),
            Incoming::Response(
                Response::checked(TransactionId::new(3), MessageId::Capability, capability)
                    .unwrap(),
            ),
            Incoming::Response(
                Response::checked(TransactionId::new(4), MessageId::DeviceInfo, device_info)
                    .unwrap(),
            ),
            success(5, MessageId::BdfDownload),
            Incoming::Indication(
                RawIndication::checked(MessageId::FirmwareInitDone, Vec::new()).unwrap(),
            ),
            success(6, MessageId::WlanMode),
            Incoming::Indication(
                RawIndication::checked(MessageId::FirmwareReady, Vec::new()).unwrap(),
            ),
            success(7, MessageId::WlanConfig),
            success(8, MessageId::WlanMode),
        ]),
        sent: Vec::new(),
        transaction: 0,
        service: None,
    };
    let mut assets = Wcn6750FirmwareAssets {
        board: vec![1, 2, 3],
        calibration: None,
        regulatory: None,
        m3: None,
    };
    // Exercise the public FirmwareAssets implementation, not an ad-hoc test shim.
    assert_eq!(assets.board_data(7).unwrap(), vec![1, 2, 3]);
    let mut memory = FakeMemory { bar: None };
    let mut qmi = Wcn6750QmiSession::new(qrtr, &mut assets, &mut memory);
    let ready = qmi.wait_for_firmware_ready().unwrap();
    assert_eq!(ready.firmware_version, 0x1122_3344);
    qmi.firmware_start(&WlanConfigRequest::default(), 0, false)
        .unwrap();
    qrtr = qmi.deinit();
    assert_eq!(
        qrtr.sent,
        [
            MessageId::IndicationRegister,
            MessageId::HostCapability,
            MessageId::Capability,
            MessageId::DeviceInfo,
            MessageId::BdfDownload,
            MessageId::WlanMode,
            MessageId::WlanConfig,
            MessageId::WlanMode,
        ]
    );
    assert_eq!(qrtr.service, None);
    assert_eq!(memory.bar, Some((0x1000_0000, 0x20_0000)));
}
