use super::*;
use proptest::prelude::*;

#[derive(Clone, Debug)]
enum Command {
    Setup {
        tid: u8,
        window: u8,
    },
    Delete {
        tid: u8,
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
    prop_oneof![
        3 => (0_u8..=3, 1_u8..=128).prop_map(|(tid, window)| Command::Setup { tid, window }),
        3 => (0_u8..=3).prop_map(|tid| Command::Delete { tid }),
        5 => (0_u16..=511, any::<u8>(), 0_u8..=3, any::<bool>()).prop_map(
            |(tag, pending, execution, short)| Command::Status { tag, pending, execution, short }
        ),
        2 => any::<u16>().prop_map(|now_ms| Command::Poll { now_ms }),
    ]
}

fn check_accounting(peers: &PeerRxTids<DeterministicBackend>) {
    let owned = peers.tids.len()
        + peers.pending_delete.len()
        + peers.cached_delete.len()
        + peers.pending_flush.len()
        + peers.uncertain_setup.len()
        + peers.failed_delete.len();
    assert_eq!((peers.pool.free_segments() + owned) % 8, 0);
    for tid in 0..=3 {
        assert!(
            peers
                .tids
                .iter()
                .filter(|entry| entry.tid.tid == tid)
                .count()
                <= 1
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
    let mut reo = controller();
    let mut rings = ModelRings::default();
    let mut wmi = ModelWmi::default();

    for command in commands {
        match *command {
            Command::Setup { tid, window } => {
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
            }
            Command::Delete { tid } => {
                peers
                    .ath11k_peer_rx_tid_delete(&mut reo, &mut rings, 1, [1; 6], tid)
                    .unwrap();
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
    #![proptest_config(ProptestConfig::with_cases(128))]

    #[test]
    fn reo_command_sequences_keep_owner_accounting_consistent(
        commands in proptest::collection::vec(command(), 1..=64)
    ) {
        run_commands(&commands);
    }
}
