use ath11k_core::{
    CoreError, ModelSubsystems, Operation as CoreOperation, Subsystems, WCN6750, WlanEvent,
};
use ath11k_dp::tx::{
    DpHost, HostRxDropCounters, HostRxFrame, HostRxInfo, HostServiceResult, RxDecapType,
    RxDecryptStatus,
};
use ath11k_softmac_adapter::Ath11kClientDevice;
use drv_hardware::{Device, Error as HardwareError};
use drv_hardware_backends::{
    DeterministicBackend, DeviceResponseInput, Operation as HardwareOperation, OperationLog,
    run_edu_sequence,
};
use fidl_fuchsia_wlan_ieee80211::{ChannelNumber, WlanBand};
use proptest::prelude::*;
use std::{
    collections::{BTreeSet, VecDeque},
    sync::{Arc, Mutex},
};
use wlan_softmac_host::{
    ClientRuntimeDriver, WlanRxInfo, WlanSoftmac, WlanSoftmacBaseStartPassiveScanRequest,
    WlanSoftmacLifecycle, WlanSoftmacUpcalls, WlanTxResult,
};

const CLIENT: [u8; 6] = [2, 0, 0, 0, 0, 1];
const SCAN_EVENT_COMPLETED: u32 = 1 << 1;
const DRIVE_BUDGET: usize = 8;
// Prefix of the first command in deterministic-dry-run.jsonl. The QMI model
// treats this as the known response marker and rejects other markers.
const RECORDED_RESPONSE_PREFIX: [u8; 8] = [0x01, 0x50, 0, 0, 0x24, 0, 0x56, 0];

fn response_input() -> impl Strategy<Value = DeviceResponseInput> {
    let progressing = prop::collection::vec(any::<u8>(), 0..=24).prop_map(|suffix| {
        let mut response_bytes = RECORDED_RESPONSE_PREFIX.to_vec();
        response_bytes.extend(suffix);
        DeviceResponseInput {
            register_reads: Vec::new(),
            response_bytes,
            // Two decisions complete the DMA response round trip, then DP and
            // WMI each receive bounded batches during the service loop.
            completions_per_poll: vec![1, 1, 1, 2, 1, 1, 1, 1],
            completion_order: Vec::new(),
            stays_silent: false,
        }
    });
    prop_oneof![
        6 => progressing.clone(),
        1 => (any::<u32>(), progressing.clone()).prop_map(|(value, mut input)| {
            input.register_reads = vec![value];
            input
        }),
        1 => (any::<u8>(), progressing.clone()).prop_map(|(marker, mut input)| {
            input.response_bytes[0] = marker;
            input
        }),
        1 => (prop::collection::vec(0_usize..=4, 0..=12), progressing.clone())
            .prop_map(|(batches, mut input)| {
                input.completions_per_poll = batches;
                input
            }),
        1 => (prop::collection::vec(0_usize..=15, 0..=16), progressing.clone())
            .prop_map(|(order, mut input)| {
                input.completion_order = order;
                input
            }),
        1 => progressing.prop_map(|mut input| {
            input.stays_silent = true;
            input
        }),
    ]
}

struct ResponseModelSubsystems {
    lifecycle: ModelSubsystems,
    device: Device<DeterministicBackend>,
    operations: OperationLog,
    response: Vec<u8>,
    deadline: u64,
    event_index: usize,
    next_cookie: u32,
    dp_ring: VecDeque<(u32, Vec<u8>)>,
    live_cookies: BTreeSet<u32>,
    completed_cookies: BTreeSet<u32>,
    stages: usize,
}

impl ResponseModelSubsystems {
    fn new(input: DeviceResponseInput) -> Self {
        let (device, operations, _) =
            DeterministicBackend::recording_noncoherent_device_with_model(input);
        Self {
            lifecycle: ModelSubsystems::default(),
            device,
            operations,
            response: Vec::new(),
            deadline: 0,
            event_index: 0,
            next_cookie: 1,
            dp_ring: VecDeque::new(),
            live_cookies: BTreeSet::new(),
            completed_cookies: BTreeSet::new(),
            stages: 0,
        }
    }

    fn response_batch(&mut self) -> Result<usize, CoreError> {
        self.deadline += 1;
        let interrupt = self.device.open_interrupt(0).map_err(map_hardware)?;
        Ok(interrupt
            .wait_until(self.deadline)
            .map_err(map_hardware)?
            .map_or(0, |event| {
                usize::try_from(event.count).unwrap_or(usize::MAX)
            }))
    }

    fn check_accounting(&self) {
        assert!(self.live_cookies.is_disjoint(&self.completed_cookies));
        assert!(
            self.stages <= 96,
            "bounded lifecycle exceeded its stage budget"
        );

        let operations = self.operations.borrow();
        for (index, operation) in operations.iter().enumerate() {
            if let HardwareOperation::SyncForCpu { dma, range } = operation {
                assert!(
                    operations[..index].iter().any(|earlier| matches!(
                        earlier,
                        HardwareOperation::SyncForDevice { dma: owner, .. } if owner == dma
                    )),
                    "CPU acquired DMA {dma:#x} without an earlier device handoff for {range:?}"
                );
            }
        }
    }
}

fn map_hardware(_: HardwareError) -> CoreError {
    CoreError::Protocol
}

impl Subsystems for ResponseModelSubsystems {
    fn execute(&mut self, operation: CoreOperation) -> Result<(), CoreError> {
        self.stages += 1;
        if self.stages > 96 {
            return Err(CoreError::Protocol);
        }
        let region = self.device.open_region(0).map_err(map_hardware)?;
        let decision = region.read_u32(0).map_err(map_hardware)?;
        if decision != 0 {
            return Err(CoreError::Protocol);
        }
        self.lifecycle.execute(operation)
    }

    fn wait_for_firmware_ready(&mut self) -> Result<ath11k_qmi::FirmwareReady, CoreError> {
        self.execute(CoreOperation::QmiWaitFirmwareReady)?;
        let response = run_edu_sequence(&self.device).map_err(map_hardware)?;
        if response[0] != RECORDED_RESPONSE_PREFIX[0] {
            return Err(CoreError::Protocol);
        }
        self.response = response.to_vec();
        Ok(ath11k_qmi::FirmwareReady {
            firmware_version: u32::from_le_bytes(response),
            target_mem_mode: u32::from(response[1]),
        })
    }

    fn next_wlan_event(&mut self) -> Result<Option<WlanEvent>, CoreError> {
        Ok(self.next_wlan_event_bounded(1)?.0)
    }

    fn next_wlan_event_bounded(
        &mut self,
        work_budget: usize,
    ) -> Result<(Option<WlanEvent>, bool), CoreError> {
        if work_budget == 0 {
            return Ok((None, false));
        }
        let batch = self.response_batch()?.min(work_budget);
        if batch == 0 {
            return Ok((None, false));
        }
        let event = match self.event_index {
            0 => WlanEvent::ManagementReceived {
                pdev_id: 0,
                channel_mhz: 2437,
                snr: 44,
                rssi: -42,
                flags: 0,
                frame: self.response.clone(),
            },
            _ => WlanEvent::Scan {
                event_type: SCAN_EVENT_COMPLETED,
                reason: 0,
                request_id: 1,
                scan_id: 1,
                vdev_id: 0,
                channel_mhz: 0,
            },
        };
        self.event_index += 1;
        Ok((Some(event), true))
    }

    fn client_nss(&self) -> Result<u8, CoreError> {
        Ok(2)
    }

    fn service_dp_host<H: DpHost>(
        &mut self,
        work_budget: usize,
        receive_budget: usize,
        host: &mut H,
    ) -> Result<HostServiceResult, CoreError> {
        let count = self.response_batch()?.min(work_budget).min(receive_budget);
        for _ in 0..count {
            let cookie = self.next_cookie;
            self.next_cookie = self
                .next_cookie
                .checked_add(1)
                .ok_or(CoreError::NoResources)?;
            if !self.live_cookies.insert(cookie) {
                return Err(CoreError::Protocol);
            }
            self.dp_ring.push_back((cookie, self.response.clone()));
        }
        let mut delivered = 0;
        while delivered < count {
            let (cookie, bytes) = self.dp_ring.pop_front().ok_or(CoreError::Protocol)?;
            if !self.live_cookies.remove(&cookie) || !self.completed_cookies.insert(cookie) {
                return Err(CoreError::Protocol);
            }
            host.receive(HostRxFrame {
                bytes,
                info: HostRxInfo {
                    decap_type: RxDecapType::Raw,
                    peer: None,
                    tid: 0,
                    decrypt_status: RxDecryptStatus::NotDecrypted,
                    phy_metadata: 2437,
                    bandwidth: 0,
                    mcs: 0,
                    packet_type: 0,
                    nss: 1,
                    phy_ppdu_id: cookie as u16,
                },
            });
            delivered += 1;
        }
        Ok(HostServiceResult {
            tx_delivered: 0,
            tx_malformed: 0,
            rx_delivered: delivered,
            rx_dropped: HostRxDropCounters::default(),
        })
    }
}

#[derive(Default)]
struct CallbackAccounting {
    frames: Vec<Vec<u8>>,
    scan_ids: Vec<u64>,
}

struct RecordingUpcalls(Arc<Mutex<CallbackAccounting>>);
impl WlanSoftmacUpcalls for RecordingUpcalls {
    fn recv(&mut self, bytes: Vec<u8>, _: WlanRxInfo) {
        self.0.lock().unwrap().frames.push(bytes);
    }
    fn report_tx_result(&mut self, _: WlanTxResult) {}
    fn notify_scan_complete(&mut self, _: zx::Status, scan_id: u64) {
        self.0.lock().unwrap().scan_ids.push(scan_id);
    }
}

fn exercise(input: DeviceResponseInput) {
    let callbacks = Arc::new(Mutex::new(CallbackAccounting::default()));
    let backend = ResponseModelSubsystems::new(input);
    let mut adapter = Ath11kClientDevice::new(WCN6750.device(backend), CLIENT);

    // A rejected or silent response must return a typed error synchronously.
    if adapter
        .start(Box::new(RecordingUpcalls(callbacks.clone())))
        .is_err()
    {
        adapter.into_device().backend().check_accounting();
        return;
    }

    let scan = adapter.start_passive_scan(WlanSoftmacBaseStartPassiveScanRequest {
        channels: Some(vec![ChannelNumber {
            band: WlanBand::TwoGhz,
            number: 6,
        }]),
        min_channel_time: Some(10),
        max_channel_time: Some(20),
        min_home_time: Some(0),
    });
    if scan.is_err() {
        let _ = adapter.stop();
        adapter.into_device().backend().check_accounting();
        return;
    }

    for _ in 0..DRIVE_BUDGET {
        match adapter.drive() {
            Ok(_) => {}
            Err(_) => break,
        }
    }
    let _ = adapter.stop();
    let device = adapter.into_device();
    device.backend().check_accounting();

    let callbacks = callbacks.lock().unwrap();
    let unique_scan_ids = callbacks.scan_ids.iter().copied().collect::<BTreeSet<_>>();
    assert_eq!(unique_scan_ids.len(), callbacks.scan_ids.len());
    assert!(callbacks.scan_ids.len() <= 1);
    assert!(callbacks.frames.len() <= 2 * DRIVE_BUDGET);
}

fn case_count() -> u32 {
    std::env::var("ATH11K_DEVICE_RESPONSE_CASES")
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(128)
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(case_count()))]

    #[test]
    fn randomized_device_response_lifecycle_is_bounded(input in response_input()) {
        exercise(input);
    }
}

#[test]
fn unexpected_response_is_a_typed_error() {
    let input = DeviceResponseInput {
        response_bytes: vec![0xff, 0x50, 0, 0],
        completions_per_poll: vec![1, 1],
        ..DeviceResponseInput::default()
    };
    let backend = ResponseModelSubsystems::new(input);
    let mut adapter = Ath11kClientDevice::new(WCN6750.device(backend), CLIENT);
    let callbacks = Arc::new(Mutex::new(CallbackAccounting::default()));
    assert_eq!(
        adapter.start(Box::new(RecordingUpcalls(callbacks))),
        Err(zx::Status::IO_INVALID)
    );
}
