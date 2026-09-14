use super::*;
use drv_hardware::{Bidirectional, Device, StreamingDma, ToDevice};
use drv_hardware_backends::{DeterministicBackend, Operation};
use proptest::prelude::*;
use std::collections::{BTreeMap, BTreeSet};

#[derive(Clone, Copy, Debug)]
enum PublishFailure {
    None,
    Descriptor,
    Producer,
}

#[derive(Clone, Debug)]
enum Command {
    Enqueue {
        length: u16,
        second: bool,
        high_address: bool,
        failure: u8,
    },
    DeviceComplete {
        index: u8,
    },
    Reclaim,
    PublishToken {
        token: u16,
    },
    TxFree {
        token: u16,
        short: bool,
    },
    PublishPid {
        pid: u8,
    },
    TxStatus {
        pid: u8,
        status: u8,
        short: bool,
    },
    ReserveMcu,
    McuResponse {
        owner: u8,
        matches: bool,
        short: bool,
    },
}

fn command() -> impl Strategy<Value = Command> {
    prop_oneof![
        5 => (any::<u16>(), any::<bool>(), any::<bool>(), 0_u8..=2).prop_map(
            |(length, second, high_address, failure)| Command::Enqueue {
                length,
                second,
                high_address,
                failure,
            }
        ),
        3 => any::<u8>().prop_map(|index| Command::DeviceComplete { index }),
        3 => Just(Command::Reclaim),
        3 => any::<u16>().prop_map(|token| Command::PublishToken { token }),
        4 => (any::<u16>(), any::<bool>()).prop_map(|(token, short)| Command::TxFree { token, short }),
        3 => any::<u8>().prop_map(|pid| Command::PublishPid { pid }),
        4 => (any::<u8>(), 0_u8..=7, any::<bool>()).prop_map(
            |(pid, status, short)| Command::TxStatus { pid, status, short }
        ),
        3 => Just(Command::ReserveMcu),
        4 => (any::<u8>(), any::<bool>(), any::<bool>()).prop_map(
            |(owner, matches, short)| Command::McuResponse { owner, matches, short }
        ),
    ]
}

#[derive(Default)]
struct RingMemory {
    freed: Vec<RingAllocation>,
}

impl Low32RingMemory for RingMemory {
    type Error = ();

    fn allocate_low32(&mut self, size: usize, _: usize) -> Result<RingAllocation, Self::Error> {
        Ok(RingAllocation {
            id: 1,
            iova: 0x1000_0000,
            len: size,
        })
    }

    fn free(&mut self, allocation: RingAllocation) {
        self.freed.push(allocation);
    }
}

struct DmaPublisher {
    dma: StreamingDma<DeterministicBackend, Bidirectional>,
    failure: PublishFailure,
    producers: Vec<u16>,
    acquire_count: usize,
}

impl DmaPublisher {
    fn new(device: &Device<DeterministicBackend>) -> Self {
        Self {
            dma: device.alloc_streaming(64, 16).unwrap(),
            failure: PublishFailure::None,
            producers: Vec::new(),
            acquire_count: 0,
        }
    }
}

impl RingPublisher for DmaPublisher {
    type Error = PublishFailure;

    fn write_descriptor(
        &mut self,
        index: u16,
        descriptor: DmaDescriptor,
    ) -> Result<(), Self::Error> {
        if matches!(self.failure, PublishFailure::Descriptor) {
            self.failure = PublishFailure::None;
            return Err(PublishFailure::Descriptor);
        }
        let offset = usize::from(index) * DMA_DESCRIPTOR_LEN;
        self.dma
            .acquire_for_cpu(offset, DMA_DESCRIPTOR_LEN)
            .unwrap();
        self.dma.write(offset, &descriptor.to_le_bytes()).unwrap();
        self.dma
            .sync_for_device(offset, DMA_DESCRIPTOR_LEN)
            .unwrap();
        Ok(())
    }

    fn release_fence(&mut self) {}

    fn publish_producer(&mut self, index: u16) -> Result<(), Self::Error> {
        if matches!(self.failure, PublishFailure::Producer) {
            self.failure = PublishFailure::None;
            return Err(PublishFailure::Producer);
        }
        self.producers.push(index);
        Ok(())
    }

    fn acquire_fence(&mut self) {
        self.acquire_count += 1;
    }
}

fn tx_free(token: u16, short: bool) -> Vec<u8> {
    if short {
        return vec![0; usize::from(token % 12)];
    }
    let mut bytes = vec![0; 12];
    bytes[..4].copy_from_slice(&((6_u32 << 27) | (1 << 16) | 12).to_le_bytes());
    bytes[8..12].copy_from_slice(&((u32::from(token & 0x7fff) << 16) | 1).to_le_bytes());
    bytes
}

fn tx_status(pid: u8, status: u8, short: bool) -> Vec<u8> {
    if short {
        return vec![0; usize::from(pid % 40)];
    }
    let mut bytes = vec![0; 40];
    bytes[..4].copy_from_slice(&40_u32.to_le_bytes());
    bytes[8..12].copy_from_slice(&(u32::from(status & 7) << 16).to_le_bytes());
    bytes[20..24].copy_from_slice(&(u32::from(pid) << 24).to_le_bytes());
    bytes
}

fn mcu_response(sequence: u8, short: bool) -> Vec<u8> {
    if short {
        return vec![0; usize::from(sequence % 36)];
    }
    let mut bytes = vec![0; 36];
    bytes[24..26].copy_from_slice(&12_u16.to_le_bytes());
    bytes[26..28].copy_from_slice(&0xe000_u16.to_le_bytes());
    bytes[28] = 1;
    bytes[29] = sequence;
    bytes
}

fn run_commands(commands: &[Command]) {
    let (device, operations) = DeterministicBackend::recording_noncoherent_device();
    let mut memory = RingMemory::default();
    let mut ring = WfdmaRing::allocate(&mut memory, 4).unwrap();
    let mut publisher = DmaPublisher::new(&device);
    let mut payloads = Vec::<StreamingDma<DeterministicBackend, ToDevice>>::new();
    let mut ring_owners = BTreeMap::<u16, usize>::new();
    let mut completed_slots = BTreeSet::<u16>::new();
    let mut effects = ClientFirmwareEffectsState {
        association_generation: Some(1),
        ..Default::default()
    };
    let generation = ClientDataGeneration::Association(1);
    let mut completed_tokens = BTreeSet::<u16>::new();
    let mut pending_pids = BTreeSet::<u8>::new();
    let mut completed_pids = BTreeSet::<u8>::new();
    let mut pending_mcu = Vec::<u8>::new();
    let mut delivered_mcu = BTreeSet::<u8>::new();

    for command in commands {
        match *command {
            Command::Enqueue {
                length,
                second,
                high_address,
                failure,
            } => {
                let length = usize::from(length % 128).max(1);
                let mut payload = device.alloc_streaming::<ToDevice>(length, 4).unwrap();
                operations.borrow_mut().clear();
                payload.write(0, &vec![0x5a; length]).unwrap();
                payload.sync_for_device(0, length).unwrap();
                let iova = if high_address {
                    u64::from(u32::MAX) + 1
                } else {
                    payload.device_address_at(0).unwrap().bits()
                };
                let first = DmaSegment {
                    iova,
                    len: length as u16,
                };
                let second = second.then_some(DmaSegment {
                    iova: iova.saturating_add(length as u64),
                    len: 1,
                });
                publisher.failure = match failure {
                    1 => PublishFailure::Descriptor,
                    2 => PublishFailure::Producer,
                    _ => PublishFailure::None,
                };
                let snapshot = (ring.producer(), ring.consumer(), ring.queued());
                let result = ring.enqueue(&mut publisher, first, second, 7);
                let log = operations.borrow();
                assert!(
                    matches!(log.first(), Some(Operation::SyncForDevice { range, .. }) if range == &(0..length))
                );
                if log.len() > 1 {
                    assert!(matches!(
                        &log[1..],
                        [
                            Operation::SyncForCpu { dma: cpu_dma, range: cpu },
                            Operation::SyncForDevice { dma: device_dma, range: device },
                        ] if cpu_dma == device_dma && cpu == device
                    ));
                }
                drop(log);
                if let Ok(index) = result {
                    assert!(ring_owners.insert(index, payloads.len()).is_none());
                } else {
                    assert_eq!(snapshot, (ring.producer(), ring.consumer(), ring.queued()));
                }
                payloads.push(payload);
            }
            Command::DeviceComplete { index } => {
                let index = u16::from(index % 6);
                let was_live = ring_owners.contains_key(&index);
                assert_eq!(ring.complete(index), index < 4);
                if was_live {
                    completed_slots.insert(index);
                }
            }
            Command::Reclaim => {
                let reclaimed = ring.reclaim_one(&mut publisher);
                if let Some(index) = reclaimed {
                    assert!(completed_slots.remove(&index));
                    assert!(ring_owners.remove(&index).is_some());
                }
            }
            Command::PublishToken { token } => {
                let token = token & 0x7fff;
                let before = effects.outstanding_tx.clone();
                let result = effects.publish_tx(token, generation);
                if before.iter().any(|(used, _)| *used == token) {
                    assert!(result.is_err());
                    assert_eq!(effects.outstanding_tx, before);
                } else {
                    result.unwrap();
                    completed_tokens.remove(&token);
                }
            }
            Command::TxFree { token, short } => {
                let before = effects.outstanding_tx.clone();
                match parse_mt7921_tx_free(&tx_free(token, short)) {
                    Ok(status) => {
                        let result = effects.complete_tx(status.token);
                        if before.iter().any(|(used, _)| *used == status.token) {
                            result.unwrap();
                            assert!(completed_tokens.insert(status.token));
                        } else {
                            assert!(result.is_err());
                        }
                    }
                    Err(_) => assert_eq!(effects.outstanding_tx, before),
                }
            }
            Command::PublishPid { pid } => {
                if pending_pids.insert(pid) {
                    completed_pids.remove(&pid);
                }
            }
            Command::TxStatus { pid, status, short } => {
                let before = pending_pids.clone();
                match parse_mt7921_tx_status(&tx_status(pid, status, short)) {
                    Ok(status) if pending_pids.remove(&status.pid) => {
                        assert!(completed_pids.insert(status.pid));
                    }
                    Ok(_) | Err(_) => assert_eq!(pending_pids, before),
                }
            }
            Command::ReserveMcu => {
                if pending_mcu.len() < 8 {
                    let sequence = effects.reserve_mcu_sequence();
                    assert!(!pending_mcu.contains(&sequence));
                    delivered_mcu.remove(&sequence);
                    pending_mcu.push(sequence);
                }
            }
            Command::McuResponse {
                owner,
                matches,
                short,
            } => {
                let Some(index) =
                    (!pending_mcu.is_empty()).then(|| usize::from(owner) % pending_mcu.len())
                else {
                    continue;
                };
                let expected = pending_mcu[index];
                let actual = if matches { expected } else { expected % 15 + 1 };
                let before = pending_mcu.clone();
                match parse_download_response(&mcu_response(actual, short), expected) {
                    Ok(response) => {
                        assert_eq!(response.sequence, expected);
                        pending_mcu.remove(index);
                        assert!(delivered_mcu.insert(expected));
                    }
                    Err(_) => assert_eq!(pending_mcu, before),
                }
            }
        }

        assert_eq!(usize::from(ring.queued()), ring_owners.len());
        assert!(completed_slots.is_subset(&ring_owners.keys().copied().collect()));
        let live_tokens = effects
            .outstanding_tx
            .iter()
            .map(|(token, _)| *token)
            .collect::<BTreeSet<_>>();
        assert_eq!(live_tokens.len(), effects.outstanding_tx.len());
        assert!(live_tokens.is_disjoint(&completed_tokens));
        assert!(pending_pids.is_disjoint(&completed_pids));
    }
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(128))]

    #[test]
    fn command_sequences_keep_wfdma_and_completion_accounting_consistent(
        commands in proptest::collection::vec(command(), 1..=64)
    ) {
        run_commands(&commands);
    }
}
