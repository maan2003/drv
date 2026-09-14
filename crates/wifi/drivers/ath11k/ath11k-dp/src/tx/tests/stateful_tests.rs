use super::*;
extern crate std;
use alloc::collections::BTreeSet;
use alloc::collections::VecDeque;
use ath11k_platform_backend::MmioRegion;
use proptest::prelude::*;

#[derive(Clone, Debug)]
enum Command {
    Submit {
        qos: bool,
        protected: bool,
    },
    TxCompletion {
        owner: u8,
        source: u8,
        status: u8,
        short: bool,
    },
    RxCompletion {
        owner: u8,
        shape: u8,
        push_reason: u8,
        length: u16,
    },
    Service {
        work: u8,
        receive: u8,
    },
    DmaPrepareFailure,
}

fn command() -> impl Strategy<Value = Command> {
    prop_oneof![
        3 => (any::<bool>(), any::<bool>()).prop_map(|(qos, protected)| Command::Submit { qos, protected }),
        6 => (0_u8..=3, 0_u8..=7, any::<u8>(), any::<bool>()).prop_map(
            |(owner, source, status, short)| Command::TxCompletion { owner, source, status, short }
        ),
        7 => (0_u8..=3, 0_u8..=6, 0_u8..=3, any::<u16>()).prop_map(
            |(owner, shape, push_reason, length)| Command::RxCompletion { owner, shape, push_reason, length }
        ),
        7 => (0_u8..=8, 0_u8..=4).prop_map(|(work, receive)| Command::Service { work, receive }),
        1 => Just(Command::DmaPrepareFailure),
    ]
}

fn stateful_cases() -> u32 {
    std::env::var("ATH11K_STATEFUL_CASES")
        .or_else(|_| std::env::var("PROPTEST_CASES"))
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(128)
}

fn stateful_max_steps() -> usize {
    std::env::var("ATH11K_STATEFUL_STEPS")
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(96)
        .clamp(1, 4_096)
}

fn sequence_lengths() -> core::ops::RangeInclusive<usize> {
    let max = stateful_max_steps();
    if max > 96 { max / 2..=max } else { 1..=max }
}

#[derive(Clone, Copy)]
enum ExpectedTxCompletion {
    Malformed,
    Retain,
    Remove { msdu_id: u32, callback: bool },
}

#[derive(Default)]
struct RecordingHost {
    rx_count: usize,
    tx_ids: BTreeSet<u32>,
}

impl DpHost for RecordingHost {
    fn receive(&mut self, _: HostRxFrame) {
        self.rx_count += 1;
    }

    fn tx_complete(&mut self, result: TxResult) {
        assert!(
            self.tx_ids.insert(result.msdu_id),
            "TX callback repeated for ID {}",
            result.msdu_id
        );
    }
}

fn rx_image(shape: u8, requested_length: u16) -> Vec<u8> {
    let mut bytes = vec![0; RX_BUFFER_SIZE];
    let continuation = shape == 1;
    let first = shape != 2;
    let last = !continuation;
    let info4 = (u16::from(first) << 12) | (u16::from(last) << 13);
    bytes[46..48].copy_from_slice(&info4.to_le_bytes());
    if shape != 3 {
        bytes[84..88].copy_from_slice(&(1_u32 << 31).to_le_bytes());
    }
    let length = if shape == 4 {
        u32::from(requested_length).max(RX_BUFFER_SIZE as u32)
    } else {
        u32::from(requested_length % 64)
    };
    bytes[96..100].copy_from_slice(&length.to_le_bytes());
    bytes[182..184].copy_from_slice(&7_u16.to_le_bytes());
    for (index, byte) in bytes[WCN6750_RX_DESCRIPTOR_BYTES..].iter_mut().enumerate() {
        *byte = index as u8;
    }
    bytes
}

fn copy_to_rx(
    dp: &ClientDataPath<DeterministicBackend, ModelRings>,
    bar: &MmioRegion<DeterministicBackend>,
    index: usize,
    image: &[u8],
) {
    let source = TxBuffer::map(&dp.device, image).unwrap();
    bar.write_device_address(0x80, Some(0x84), source.device_address().unwrap())
        .unwrap();
    bar.write_u32(0x90, RX_BUFFER_SIZE as u32).unwrap();
    bar.write_u32(0x98, 1).unwrap();
    bar.write_device_address(
        0x88,
        Some(0x8c),
        dp.rx_buffers[index].buffer.device_address().unwrap(),
    )
    .unwrap();
    bar.write_u32(0x98, 1 | 2).unwrap();
}

fn check_state(
    dp: &ClientDataPath<DeterministicBackend, ModelRings>,
    host: &RecordingHost,
    expected_live_tx: &BTreeSet<u32>,
) {
    let tx_ids = dp
        .pending
        .iter()
        .map(|pending| pending.msdu_id)
        .collect::<BTreeSet<_>>();
    assert_eq!(
        tx_ids.len(),
        dp.pending.len(),
        "live TX IDs must be distinct"
    );
    assert_eq!(
        &tx_ids, expected_live_tx,
        "TX owner accounting differs from the independent command model"
    );
    assert!(
        host.tx_ids.is_disjoint(&tx_ids),
        "completed TX ID remained live"
    );

    let cookies = dp
        .rx_buffers
        .iter()
        .map(|entry| entry.cookie)
        .collect::<BTreeSet<_>>();
    assert_eq!(
        cookies.len(),
        dp.rx_buffers.len(),
        "live RX cookies must be distinct"
    );
    let addresses = dp
        .rx_buffers
        .iter()
        .map(|entry| entry.buffer.device_address().unwrap().bits())
        .collect::<BTreeSet<_>>();
    assert_eq!(
        addresses.len(),
        dp.rx_buffers.len(),
        "live RX addresses must be distinct"
    );
}

fn run_commands(commands: &[Command]) {
    let (device, operations) = DeterministicBackend::recording_noncoherent_device();
    let bar = device.open_region(0).unwrap();
    let mut dp = ClientDataPath::without_allocated_rings(device, ModelRings::default(), config());
    dp.configure(DataRings {
        tcl: RingId(1),
        reo: RingId(2),
        wbm: RingId(3),
    })
    .unwrap();
    dp.ath11k_dp_rxbufs_replenish(
        RxdmaConfig {
            ring: RingId(4),
            pdev_id: 0,
            return_buffer_manager: 3,
            buffer_size: RX_BUFFER_SIZE,
        },
        8,
    )
    .unwrap();
    // Put the cursor at its boundary while IDs 1 through 8 remain live. Two
    // replacements therefore cross the 18-bit wrap in a short generated run.
    dp.next_rx_cookie = RX_BUFFER_ID_MASK;
    let mut host = RecordingHost::default();
    let mut device_written = BTreeSet::new();
    let mut expected_live_tx = BTreeSet::new();
    let mut next_msdu_id = 0_u32;
    let mut expected_tx_completions = VecDeque::new();

    for command in commands {
        match *command {
            Command::Submit { qos, protected } => {
                let mut frame = vec![0; if qos { 26 } else { 24 }];
                let mut fc = if qos { 0x0088_u16 } else { 0x0008 };
                if protected {
                    fc |= 0x4000;
                }
                frame[..2].copy_from_slice(&fc.to_le_bytes());
                if qos {
                    frame[24] = 3;
                }
                dp.submit_host_frame(
                    &frame,
                    crate::PeerId(4),
                    HostTxFlags {
                        protected,
                        qos,
                        favor_reliability: false,
                    },
                )
                .unwrap();
                let mut expected = next_msdu_id;
                while expected_live_tx.contains(&expected) {
                    expected = if expected == MAX_MSDU_ID {
                        0
                    } else {
                        expected + 1
                    };
                }
                assert!(expected_live_tx.insert(expected));
                next_msdu_id = if expected == MAX_MSDU_ID {
                    0
                } else {
                    expected + 1
                };
            }
            Command::TxCompletion {
                owner,
                source,
                status,
                short,
            } => {
                let msdu_id = if dp.pending.is_empty() {
                    u32::from(owner)
                } else {
                    dp.pending[usize::from(owner) % dp.pending.len()].msdu_id
                };
                let descriptor = if short {
                    expected_tx_completions.push_back(ExpectedTxCompletion::Malformed);
                    Descriptor::new(vec![0; usize::from(owner % 32)], usize::from(owner % 32))
                        .unwrap()
                } else {
                    let mut release = WbmReleaseRing::new();
                    let mut address = RxdmaBufferRing::new();
                    address
                        .set_software_cookie((1 << 19) | (msdu_id << 2))
                        .unwrap();
                    release.set_buffer_address(&address);
                    release.set_release_source(source).unwrap();
                    let mut raw = *release.as_bytes();
                    let info0 = u32::from_le_bytes(raw[8..12].try_into().unwrap())
                        | (u32::from(status & 0x1f) << 9);
                    raw[8..12].copy_from_slice(&info0.to_le_bytes());
                    let expected = match source {
                        0 => ExpectedTxCompletion::Remove {
                            msdu_id,
                            callback: true,
                        },
                        3 if status & 0x0f == 5 => ExpectedTxCompletion::Retain,
                        3 => ExpectedTxCompletion::Remove {
                            msdu_id,
                            callback: !matches!(status & 0x0f, 3 | 4),
                        },
                        _ => ExpectedTxCompletion::Retain,
                    };
                    expected_tx_completions.push_back(expected);
                    Descriptor::new(raw.to_vec(), raw.len()).unwrap()
                };
                dp.rings_mut()
                    .ring_completions
                    .entry(3)
                    .or_default()
                    .push_back(descriptor);
            }
            Command::RxCompletion {
                owner,
                shape,
                push_reason,
                length,
            } => {
                let live = !dp.rx_buffers.is_empty() && shape != 6;
                let index = if live {
                    usize::from(owner) % dp.rx_buffers.len()
                } else {
                    0
                };
                let cookie = if live {
                    dp.rx_buffers[index].cookie
                } else {
                    0x3_ffff
                };
                if live && device_written.insert(cookie) {
                    copy_to_rx(&dp, &bar, index, &rx_image(shape, length));
                }
                let descriptor = if shape == 5 {
                    Descriptor::new(vec![0; usize::from(owner % 32)], usize::from(owner % 32))
                        .unwrap()
                } else {
                    let mut address = RxdmaBufferRing::new();
                    address.set_software_cookie(cookie).unwrap();
                    let mut reo = ReoDestinationRing::new();
                    reo.set_buffer_address(&address);
                    reo.set_push_reason(push_reason).unwrap();
                    reo.into_descriptor()
                };
                dp.rings_mut()
                    .ring_completions
                    .entry(2)
                    .or_default()
                    .push_back(descriptor);
            }
            Command::Service { work, receive } => {
                operations.borrow_mut().clear();
                let before_rx = host.rx_count;
                let before_tx = host.tx_ids.len();
                let queued_before = dp.rings().ring_completions.get(&3).map_or(0, VecDeque::len);
                let result = dp
                    .service_host(usize::from(work), usize::from(receive), &mut host)
                    .unwrap();
                let queued_after = dp.rings().ring_completions.get(&3).map_or(0, VecDeque::len);
                let mut expected_callbacks = 0;
                for _ in 0..queued_before - queued_after {
                    match expected_tx_completions.pop_front().unwrap() {
                        ExpectedTxCompletion::Malformed | ExpectedTxCompletion::Retain => {}
                        ExpectedTxCompletion::Remove { msdu_id, callback } => {
                            if expected_live_tx.remove(&msdu_id) && callback {
                                expected_callbacks += 1;
                            }
                        }
                    }
                }
                assert_eq!(host.rx_count - before_rx, result.rx_delivered);
                assert_eq!(host.tx_ids.len() - before_tx, result.tx_delivered);
                assert_eq!(
                    host.tx_ids.len() - before_tx,
                    expected_callbacks,
                    "TX callbacks differ from supported completion-source accounting"
                );
                let operations = operations.borrow();
                for pair in operations.chunks(2) {
                    match pair {
                        [
                            Operation::SyncForCpu {
                                dma: cpu_dma,
                                range: cpu,
                            },
                            Operation::SyncForDevice {
                                dma: device_dma,
                                range: device,
                            },
                        ] => {
                            assert_eq!((cpu_dma, cpu), (device_dma, device));
                        }
                        [] => {}
                        _ => panic!("unexpected DMA ownership log: {pair:?}"),
                    }
                }
            }
            Command::DmaPrepareFailure => {
                let (device, failures) = DeterministicBackend::noncoherent_device_with_failures();
                let pool = make_rx_pool(device).unwrap();
                let mut completed = RxBuffer::replenish(&pool).unwrap();
                let discarded = completed.device_address().unwrap().bits();
                completed.sync_and_read(completed.len()).unwrap();
                failures.fail_next_sync_for_device();
                assert_eq!(completed.prepare_for_device(), Err(DpError::DeviceFault));
                drop(completed);
                let replacement = RxBuffer::replenish(&pool).unwrap();
                assert_ne!(
                    replacement.device_address().unwrap().bits(),
                    discarded,
                    "a segment whose prepare failed returned to the DMA pool"
                );
            }
        }
        check_state(&dp, &host, &expected_live_tx);
    }
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(stateful_cases()))]

    #[test]
    fn command_sequences_keep_buffer_lifecycle_consistent(
        commands in proptest::collection::vec(command(), sequence_lengths())
    ) {
        run_commands(&commands);
    }
}
