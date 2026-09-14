//! Transactional MCU RX ownership. A route is not observable until both the
//! replacement descriptor and producer index have been successfully published.

use drv_hardware::{Error, Result};
use mt7921_core::{
    FirmwareRxDisposition, MT7921_MCU_RX_RING_COUNT, McuRxIrqRing, McuRxRoute, McuRxRouteError,
};
use std::collections::VecDeque;
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};

const EVENT_CAPACITY: usize = 64;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct Slot {
    ring: McuRxIrqRing,
    index: u16,
}

impl Slot {
    fn new(ring: McuRxIrqRing, index: u16) -> Result<Self> {
        if usize::from(index) >= MT7921_MCU_RX_RING_COUNT {
            return Err(Error::OutOfBounds);
        }
        Ok(Self { ring, index })
    }

    fn ring_index(self) -> usize {
        match self.ring {
            McuRxIrqRing::Wm => 0,
            McuRxIrqRing::Wm2 => 1,
        }
    }
}

#[derive(Clone, Copy)]
enum SlotState {
    Available(u64),
    Armed(u64),
}

struct Occurrence {
    slot: Slot,
    generation: u64,
}

struct Prepared {
    occurrence: Occurrence,
    posted: Slot,
    route: Option<McuRxRoute>,
}

struct Reposted(Prepared);

/// The private constructor is reached only after successful hardware
/// publication. Moving this value cannot turn a revoked occurrence live again.
pub struct ReceivedEvent {
    occurrence: Occurrence,
    live: Arc<AtomicBool>,
    route: Option<McuRxRoute>,
}

impl ReceivedEvent {
    pub(super) fn into_route(mut self) -> Result<McuRxRoute> {
        if !self.live.load(Ordering::Acquire) {
            return Err(Error::StaleHandle);
        }
        self.route.take().ok_or(Error::Invalid)
    }
}

fn erase_route(route: &mut Option<McuRxRoute>) {
    match route {
        Some(McuRxRoute::Normal(bytes)) => bytes.fill(0),
        Some(McuRxRoute::Firmware(response)) => response.bytes.fill(0),
        _ => {}
    }
    *route = None;
}

impl Drop for Prepared {
    fn drop(&mut self) {
        erase_route(&mut self.route);
    }
}

impl Drop for ReceivedEvent {
    fn drop(&mut self) {
        erase_route(&mut self.route);
    }
}

enum Pending {
    Prepared(Prepared),
    Reposted(Reposted),
    Committed(ReceivedEvent),
}

/// One owner for both MCU receive rings, including events interleaved with a
/// synchronous command response. Slot reuse and queued event lifetimes are
/// distinct: rearming a slot does not revoke an already committed occurrence.
pub(super) struct RxRouting {
    slots: [[SlotState; MT7921_MCU_RX_RING_COUNT]; 2],
    pending: Option<Pending>,
    events: VecDeque<ReceivedEvent>,
    live: Arc<AtomicBool>,
}

impl Default for RxRouting {
    fn default() -> Self {
        let mut slots = [[SlotState::Armed(1); MT7921_MCU_RX_RING_COUNT]; 2];
        for ring in &mut slots {
            ring[MT7921_MCU_RX_RING_COUNT - 1] = SlotState::Available(0);
        }
        Self {
            slots,
            pending: None,
            events: VecDeque::new(),
            live: Arc::new(AtomicBool::new(true)),
        }
    }
}

impl RxRouting {
    fn fault<T>(&mut self, error: Error) -> Result<T> {
        self.abort();
        Err(error)
    }

    pub(super) fn prepare(
        &mut self,
        ring: McuRxIrqRing,
        consumed: u16,
        posted: u16,
        route: std::result::Result<&McuRxRoute, &McuRxRouteError>,
        disposition: Option<FirmwareRxDisposition>,
    ) -> Result<()> {
        if !self.live.load(Ordering::Acquire) {
            return Err(Error::StaleHandle);
        }
        if self.pending.is_some() {
            return self.fault(Error::DeviceFault);
        }
        let (Ok(slot), Ok(posted)) = (Slot::new(ring, consumed), Slot::new(ring, posted)) else {
            return self.fault(Error::OutOfBounds);
        };
        let SlotState::Armed(generation) = self.slots[slot.ring_index()][usize::from(slot.index)]
        else {
            return self.fault(Error::StaleHandle);
        };
        let route = match route {
            Ok(McuRxRoute::Firmware(_)) if disposition == Some(FirmwareRxDisposition::Matched) => {
                None
            }
            Ok(route) => Some(route.clone()),
            Err(_) => return self.fault(Error::DeviceFault),
        };
        self.slots[slot.ring_index()][usize::from(slot.index)] = SlotState::Available(generation);
        self.pending = Some(Pending::Prepared(Prepared {
            occurrence: Occurrence { slot, generation },
            posted,
            route,
        }));
        Ok(())
    }

    pub(super) fn repost(
        &mut self,
        ring: McuRxIrqRing,
        posted: u16,
        write_descriptor: impl FnOnce() -> Result<()>,
    ) -> Result<()> {
        let Some(Pending::Prepared(prepared)) = self.pending.take() else {
            return self.fault(Error::DeviceFault);
        };
        if prepared.posted
            != (Slot {
                ring,
                index: posted,
            })
        {
            return self.fault(Error::Invalid);
        }
        let state = &mut self.slots[prepared.posted.ring_index()][usize::from(posted)];
        let SlotState::Available(generation) = *state else {
            return self.fault(Error::DeviceFault);
        };
        let Some(generation) = generation.checked_add(1) else {
            return self.fault(Error::Limit);
        };
        if let Err(error) = write_descriptor() {
            return self.fault(error);
        }
        *state = SlotState::Armed(generation);
        self.pending = Some(Pending::Reposted(Reposted(prepared)));
        Ok(())
    }

    pub(super) fn publish(
        &mut self,
        ring: McuRxIrqRing,
        producer: u16,
        write_index: impl FnOnce() -> Result<()>,
    ) -> Result<()> {
        let Some(Pending::Reposted(Reposted(mut prepared))) = self.pending.take() else {
            return self.fault(Error::DeviceFault);
        };
        if prepared.occurrence.slot
            != (Slot {
                ring,
                index: producer,
            })
        {
            return self.fault(Error::Invalid);
        }
        if let Err(error) = write_index() {
            return self.fault(error);
        }
        self.pending = Some(Pending::Committed(ReceivedEvent {
            occurrence: Occurrence {
                slot: prepared.occurrence.slot,
                generation: prepared.occurrence.generation,
            },
            live: Arc::clone(&self.live),
            route: prepared.route.take(),
        }));
        Ok(())
    }

    pub(super) fn complete(&mut self, ring: McuRxIrqRing, consumed: u16) -> Result<()> {
        let Some(Pending::Committed(committed)) = self.pending.take() else {
            return self.fault(Error::DeviceFault);
        };
        if committed.occurrence.slot
            != (Slot {
                ring,
                index: consumed,
            })
        {
            return self.fault(Error::Invalid);
        }
        if committed.route.is_some() {
            if self.events.len() == EVENT_CAPACITY {
                return self.fault(Error::Limit);
            }
            self.events.push_back(committed);
        }
        Ok(())
    }

    pub(super) fn take_event(&mut self) -> Option<ReceivedEvent> {
        self.events.pop_front()
    }

    pub(super) fn abort(&mut self) {
        // Revoke escaped occurrences before dropping any payload or pending
        // publication state. This owner is terminal after an RX failure.
        self.live.store(false, Ordering::Release);
        self.pending = None;
        self.events.clear();
    }
}

impl Drop for RxRouting {
    fn drop(&mut self) {
        self.abort();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn stage(routing: &mut RxRouting, slot: u16, route: &McuRxRoute) {
        let posted = (slot + MT7921_MCU_RX_RING_COUNT as u16 - 1) % MT7921_MCU_RX_RING_COUNT as u16;
        routing
            .prepare(McuRxIrqRing::Wm, slot, posted, Ok(route), None)
            .unwrap();
        assert!(routing.take_event().is_none());
        routing.repost(McuRxIrqRing::Wm, posted, || Ok(())).unwrap();
        assert!(routing.take_event().is_none());
        routing.publish(McuRxIrqRing::Wm, slot, || Ok(())).unwrap();
        assert!(routing.take_event().is_none());
        routing.complete(McuRxIrqRing::Wm, slot).unwrap();
    }

    #[test]
    fn only_committed_occurrences_escape_and_slot_wrap_does_not_duplicate() {
        let mut routing = RxRouting::default();
        for ordinal in 0..40 {
            let slot = ordinal % MT7921_MCU_RX_RING_COUNT as u16;
            let route = McuRxRoute::Normal(vec![ordinal as u8; 16]);
            stage(&mut routing, slot, &route);
            let event = routing.take_event().unwrap();
            assert_eq!(event.occurrence.slot.index, slot);
            assert_eq!(
                event.occurrence.generation,
                u64::from(ordinal / MT7921_MCU_RX_RING_COUNT as u16) + 1
            );
            assert_eq!(event.into_route().unwrap(), route);
            assert!(routing.take_event().is_none());
        }
    }

    #[test]
    fn failure_at_each_publication_boundary_revokes_already_escaped_frames() {
        for boundary in 0..4 {
            let mut routing = RxRouting::default();
            let route = McuRxRoute::Normal(vec![0x5a; 16]);
            stage(&mut routing, 0, &route);
            let escaped = routing.take_event().unwrap();
            routing
                .prepare(McuRxIrqRing::Wm, 1, 0, Ok(&route), None)
                .unwrap();
            match boundary {
                0 => assert!(
                    routing
                        .prepare(McuRxIrqRing::Wm, 1, 0, Ok(&route), None)
                        .is_err()
                ),
                1 => assert!(
                    routing
                        .repost(McuRxIrqRing::Wm, 0, || Err(Error::DeviceFault))
                        .is_err()
                ),
                2 => {
                    routing.repost(McuRxIrqRing::Wm, 0, || Ok(())).unwrap();
                    assert!(
                        routing
                            .publish(McuRxIrqRing::Wm, 1, || Err(Error::DeviceFault))
                            .is_err()
                    );
                }
                _ => assert!(routing.complete(McuRxIrqRing::Wm, 1).is_err()),
            }
            assert!(matches!(escaped.into_route(), Err(Error::StaleHandle)));
            assert!(routing.take_event().is_none());
            assert!(
                routing
                    .prepare(McuRxIrqRing::Wm, 1, 0, Ok(&route), None)
                    .is_err()
            );
        }
    }

    #[test]
    fn normal_tx_and_unsolicited_events_keep_their_identity_but_matched_response_is_not_queued() {
        let mut routing = RxRouting::default();
        let response = mt7921_core::FirmwareRx {
            response: mt7921_core::DownloadResponse {
                length: 16,
                packet_type: 0xe000,
                event_id: 0x13,
                sequence: 3,
                option: 0,
                extended_event_id: 0,
            },
            bytes: vec![0x13; 16],
        };
        let events = [
            McuRxRoute::Normal(vec![0x5a; 16]),
            McuRxRoute::TxFree(mt7921_core::Mt7921TxFree {
                wcid: Some(7),
                token: 11,
                dropped: false,
                attempts: 2,
                status: 0,
                pair_word: Some(0x8004c000),
                info_word: 1,
            }),
            McuRxRoute::TxStatus(mt7921_core::Mt7921TxStatus {
                wcid: 7,
                pid: 9,
                acked: true,
            }),
            McuRxRoute::Firmware(response.clone()),
        ];
        for (slot, event) in events.iter().enumerate() {
            stage(&mut routing, slot as u16, event);
            assert_eq!(routing.take_event().unwrap().into_route().unwrap(), *event);
        }
        // The second ring has its own slot lifecycle. Command matching returns
        // this response through LoaderMechanics, not through the event queue.
        routing
            .prepare(
                McuRxIrqRing::Wm2,
                0,
                7,
                Ok(&McuRxRoute::Firmware(response)),
                Some(FirmwareRxDisposition::Matched),
            )
            .unwrap();
        routing.repost(McuRxIrqRing::Wm2, 7, || Ok(())).unwrap();
        routing.publish(McuRxIrqRing::Wm2, 0, || Ok(())).unwrap();
        routing.complete(McuRxIrqRing::Wm2, 0).unwrap();
        assert!(routing.take_event().is_none());
    }

    #[test]
    fn dropping_the_owner_revokes_an_escaped_committed_occurrence() {
        let mut routing = RxRouting::default();
        stage(&mut routing, 0, &McuRxRoute::Normal(vec![1; 16]));
        let escaped = routing.take_event().unwrap();
        drop(routing);
        assert!(matches!(escaped.into_route(), Err(Error::StaleHandle)));
    }

    #[test]
    fn wrong_slot_or_duplicate_consumption_cannot_perform_hardware_publication() {
        for duplicate in [false, true] {
            let mut routing = RxRouting::default();
            let route = McuRxRoute::Normal(vec![1; 16]);
            stage(&mut routing, 0, &route);
            let escaped = routing.take_event().unwrap();
            if duplicate {
                assert!(
                    routing
                        .prepare(McuRxIrqRing::Wm, 0, 7, Ok(&route), None)
                        .is_err()
                );
            } else {
                routing
                    .prepare(McuRxIrqRing::Wm, 1, 0, Ok(&route), None)
                    .unwrap();
                assert!(
                    routing
                        .repost(McuRxIrqRing::Wm2, 0, || panic!(
                            "invalid route performed DMA write"
                        ))
                        .is_err()
                );
            }
            assert!(matches!(escaped.into_route(), Err(Error::StaleHandle)));
        }
    }

    #[test]
    fn bounded_backlog_overflow_is_terminal_and_clears_queued_events() {
        let mut routing = RxRouting::default();
        let route = McuRxRoute::Normal(vec![0x5a; 16]);
        for ordinal in 0..EVENT_CAPACITY {
            let slot = (ordinal % MT7921_MCU_RX_RING_COUNT) as u16;
            let posted =
                (slot + MT7921_MCU_RX_RING_COUNT as u16 - 1) % MT7921_MCU_RX_RING_COUNT as u16;
            routing
                .prepare(McuRxIrqRing::Wm, slot, posted, Ok(&route), None)
                .unwrap();
            routing.repost(McuRxIrqRing::Wm, posted, || Ok(())).unwrap();
            routing.publish(McuRxIrqRing::Wm, slot, || Ok(())).unwrap();
            routing.complete(McuRxIrqRing::Wm, slot).unwrap();
        }
        routing
            .prepare(McuRxIrqRing::Wm, 0, 7, Ok(&route), None)
            .unwrap();
        routing.repost(McuRxIrqRing::Wm, 7, || Ok(())).unwrap();
        routing.publish(McuRxIrqRing::Wm, 0, || Ok(())).unwrap();
        assert!(matches!(
            routing.complete(McuRxIrqRing::Wm, 0),
            Err(Error::Limit)
        ));
        assert!(routing.take_event().is_none());
    }
}
