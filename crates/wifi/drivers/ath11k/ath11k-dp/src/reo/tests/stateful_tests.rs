use super::*;
extern crate std;
use proptest::prelude::*;

#[derive(Clone, Debug)]
enum Command {
    Setup {
        tid: u8,
        window: u8,
        wmi_outcome: u8,
    },
    Delete {
        tid: u8,
        fail_invalidation: bool,
    },
    Status {
        tag: u16,
        pending: u8,
        execution: u8,
        short: bool,
    },
    Poll {
        now_ms: u16,
    },
}

fn command() -> impl Strategy<Value = Command> {
    let tid = prop_oneof![5 => Just(0_u8), 1 => 1_u8..=3];
    prop_oneof![
        5 => (tid.clone(), 1_u8..=128, 0_u8..=2).prop_map(
            |(tid, window, wmi_outcome)| Command::Setup { tid, window, wmi_outcome }
        ),
        4 => (tid, any::<bool>()).prop_map(
            |(tid, fail_invalidation)| Command::Delete { tid, fail_invalidation }
        ),
        5 => (0_u16..=511, any::<u8>(), 0_u8..=3, any::<bool>()).prop_map(
            |(tag, pending, execution, short)| Command::Status { tag, pending, execution, short }
        ),
        2 => any::<u16>().prop_map(|now_ms| Command::Poll { now_ms }),
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

fn check_accounting(peers: &PeerRxTids<DeterministicBackend>) {
    let owned = peers.tids.len()
        + peers.pending_delete.len()
        + peers.cached_delete.len()
        + peers.pending_flush.len()
        + peers.uncertain_setup.len()
        + peers.failed_delete.len();
    assert_eq!((peers.pool.free_segments() + owned) % 8, 0);
    let mut device_visible_keys = alloc::collections::BTreeSet::new();
    for entry in peers
        .tids
        .iter()
        .chain(&peers.uncertain_setup)
        .chain(&peers.failed_delete)
    {
        assert!(
            device_visible_keys.insert((entry.vdev_id, entry.peer_addr, entry.tid.tid)),
            "same peer/TID key has more than one possibly device-visible owner"
        );
    }
}

fn descriptor(tag: u16, command_number: u16, execution: u8, short: bool) -> Descriptor {
    if short {
        return Descriptor::new(vec![0; usize::from(tag % 104)], usize::from(tag % 104)).unwrap();
    }
    let mut bytes = vec![0; 104];
    let header = (u32::from(tag) & 0x1ff) << 1 | 100 << 10;
    bytes[..4].copy_from_slice(&header.to_le_bytes());
    let info = u32::from(command_number) | (u32::from(execution & 3) << 26);
    bytes[4..8].copy_from_slice(&info.to_le_bytes());
    Descriptor::new(bytes, 104).unwrap()
}

fn run_commands(commands: &[Command]) {
    let (device, operations) = DeterministicBackend::recording_noncoherent_device();
    let mut peers = PeerRxTids::new(device).unwrap();
    peers
        .register_peer_after_firmware_create(1, [1; 6])
        .unwrap();
    let mut reo = controller();
    let mut rings = ModelRings::default();
    let mut wmi = ModelWmi::default();

    for command in commands {
        match *command {
            Command::Setup {
                tid,
                window,
                wmi_outcome,
            } => {
                if wmi_outcome == 2 {
                    let _ = peers.ath11k_peer_rx_tid_setup(
                        &mut reo,
                        &mut rings,
                        &mut UncertainWmi,
                        1,
                        [1; 6],
                        tid,
                        u32::from(window),
                        0,
                        PacketNumberType::None,
                    );
                } else {
                    wmi.fail = wmi_outcome == 1;
                    let _ = peers.ath11k_peer_rx_tid_setup(
                        &mut reo,
                        &mut rings,
                        &mut wmi,
                        1,
                        [1; 6],
                        tid,
                        u32::from(window),
                        0,
                        PacketNumberType::None,
                    );
                    wmi.fail = false;
                }
            }
            Command::Delete {
                tid,
                fail_invalidation,
            } => {
                rings.fail_publish = fail_invalidation;
                let result = peers.ath11k_peer_rx_tid_delete(&mut reo, &mut rings, 1, [1; 6], tid);
                rings.fail_publish = false;
                if fail_invalidation && result.is_ok() {
                    assert!(
                        !peers.tids.iter().any(|entry| entry.tid.tid == tid),
                        "a successful no-op delete must not leave an unexpected active owner"
                    );
                }
            }
            Command::Status {
                tag,
                pending,
                execution,
                short,
            } => {
                let numbers = peers
                    .pending_delete
                    .iter()
                    .chain(&peers.pending_flush)
                    .map(|entry| entry.command_number)
                    .collect::<Vec<_>>();
                let number = numbers
                    .get(usize::from(pending) % numbers.len().max(1))
                    .copied()
                    .unwrap_or(u16::from(pending));
                rings
                    .status
                    .push_back(descriptor(tag, number, execution, short));
            }
            Command::Poll { now_ms } => {
                let before = operations.borrow().len();
                let _ = peers.ath11k_dp_rx_tid_del_func(&mut reo, &mut rings, u64::from(now_ms));
                for operation in &operations.borrow()[before..] {
                    if let Operation::SyncForDevice { range, .. } = operation {
                        assert_eq!(range.end - range.start, 512);
                    }
                }
            }
        }
        check_accounting(&peers);
    }
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(stateful_cases()))]

    #[test]
    fn reo_command_sequences_keep_owner_accounting_consistent(
        commands in proptest::collection::vec(command(), sequence_lengths())
    ) {
        run_commands(&commands);
    }
}
