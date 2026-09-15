//! Bounded management DMA ownership. A firmware grant permits publication;
//! it does not prove that either descriptor or payload ownership has returned.

use crate::{
    OwnedHardwareResources,
    radio::{FirmwareCommands, RadioResponse},
};
use drv_hardware::Backend;
use mt7921_core::{DMA_DESCRIPTOR_LEN, MT7921_BAND0_TX_RING_COUNT, Mt7921TxFree, Mt7921TxStatus};
use std::{
    collections::VecDeque,
    time::{Duration, Instant},
};
use wlan_softmac_host::OperationContext;

const CAPACITY: usize = 16;

struct QueuedFrame {
    context: OperationContext,
    bytes: Vec<u8>,
    rate: u8,
    channel: mt7921_core::CandidateChannel,
}

struct PublishedFrame {
    frame: QueuedFrame,
    slot: u16,
    next: u16,
    token: u16,
    pid: u8,
    deadline: Instant,
    descriptor_done: bool,
    freed: bool,
    status: Option<bool>,
}

enum RocPhase {
    Acquiring {
        commands: FirmwareCommands,
        deadline: Instant,
        grant: Option<Instant>,
    },
    Granted {
        until: Instant,
    },
    Releasing(FirmwareCommands),
}

struct Roc {
    token: u8,
    channel: mt7921_core::CandidateChannel,
    context: OperationContext,
    phase: RocPhase,
}

/// One descriptor/payload pair is in flight; later frames remain CPU-owned.
/// Only the exclusive driver may supply an unexpired, matching ROC grant.
pub(super) struct ManagementTx {
    queue: VecDeque<QueuedFrame>,
    pending: Option<PublishedFrame>,
    producer: u16,
    next_token: u16,
    next_pid: u8,
    failed: bool,
    roc: Option<Roc>,
    next_roc_token: u8,
}

impl Default for ManagementTx {
    fn default() -> Self {
        Self {
            queue: VecDeque::new(),
            pending: None,
            producer: 0,
            next_token: 0,
            next_pid: 3,
            failed: false,
            roc: None,
            next_roc_token: 1,
        }
    }
}

impl ManagementTx {
    pub fn idle(&self) -> bool {
        self.queue.is_empty() && self.pending.is_none() && self.roc.is_none()
    }

    pub fn enqueue(
        &mut self,
        context: OperationContext,
        bytes: &[u8],
        rate: u8,
        channel: mt7921_core::CandidateChannel,
    ) -> Result<(), zx::Status> {
        context.check(Instant::now())?;
        if self.failed {
            return Err(zx::Status::BAD_STATE);
        }
        if self.roc.as_ref().is_some_and(|roc| roc.channel != channel)
            || self
                .queue
                .front()
                .is_some_and(|frame| frame.channel != channel)
        {
            return Err(zx::Status::BAD_STATE);
        }
        if self.queue.len() + usize::from(self.pending.is_some()) == CAPACITY {
            return Err(zx::Status::NO_RESOURCES);
        }
        // This initial path handles only unfragmented, unprotected unicast
        // management. Keys, multicast/BIP and data require their own contract.
        if !(26..=4095).contains(&bytes.len())
            || !matches!(bytes[0], 0x00 | 0xb0 | 0xa0 | 0xc0)
            || bytes[1] & !0x08 != 0
            || bytes[4] & 1 != 0
            || bytes[22] & 0xf != 0
            || !crate::peer::OFDM_RATES.contains(&rate)
        {
            return Err(zx::Status::NOT_SUPPORTED);
        }
        if bytes[0] == 0xb0 && bytes.len() < 30 || bytes[0] == 0 && bytes.len() < 28 {
            return Err(zx::Status::INVALID_ARGS);
        }
        self.queue.push_back(QueuedFrame {
            context,
            bytes: bytes.to_vec(),
            rate,
            channel,
        });
        Ok(())
    }

    pub fn tx_free(&mut self, free: Mt7921TxFree) -> Result<(), zx::Status> {
        if let Some(pending) = self.pending.as_mut()
            && free.token == pending.token
        {
            if free.wcid.is_some_and(|wcid| wcid != 1) || pending.freed {
                self.failed = true;
                return Err(zx::Status::IO_DATA_INTEGRITY);
            }
            pending.freed = true;
        }
        Ok(())
    }

    pub fn tx_status(&mut self, status: Mt7921TxStatus) -> Result<(), zx::Status> {
        if let Some(pending) = self.pending.as_mut()
            && status.pid == pending.pid
            && status.wcid == 1
        {
            if pending.status.is_some() {
                self.failed = true;
                return Err(zx::Status::IO_DATA_INTEGRITY);
            }
            pending.status = Some(status.acked);
        }
        Ok(())
    }

    pub fn roc_grant(
        &mut self,
        grant: mt7921_core::ClientJoinRocGrant,
        now: Instant,
    ) -> Result<(), zx::Status> {
        let Some(roc) = &mut self.roc else {
            return Ok(());
        };
        if grant.token != roc.token {
            return Ok(());
        }
        let RocPhase::Acquiring {
            grant: received, ..
        } = &mut roc.phase
        else {
            return Ok(());
        };
        let band = if roc.channel.band == mt7921_core::PhysicalBand::Ghz2 {
            1
        } else {
            2
        };
        if grant.bss_index != 0
            || grant.status != 0
            || grant.request_type != 0
            || grant.band != band
            || u16::from(grant.primary_channel) != roc.channel.number
            || grant.center_channel != grant.primary_channel
            || grant.bandwidth != 0
            || grant.max_interval_ms == 0
            || received.is_some()
        {
            self.failed = true;
            return Err(zx::Status::IO_DATA_INTEGRITY);
        }
        *received = Some(now + Duration::from_millis(u64::from(grant.max_interval_ms.min(2000))));
        Ok(())
    }

    pub fn drive<B: Backend>(
        &mut self,
        resources: &mut OwnedHardwareResources<B>,
        mechanics: &mut mt7921_core::LoaderMechanics,
        receive: &mut crate::receive::RxRouting,
        start: Instant,
        now: Instant,
    ) -> Result<bool, zx::Status> {
        if self.failed {
            return Err(zx::Status::BAD_STATE);
        }
        let result = (|| {
            let mut progressed = false;
            if self.roc.is_none()
                && let Some(frame) = self.queue.front()
            {
                frame.context.check(now)?;
                let duration = if frame.bytes[0] == 0xb0 && frame.bytes[24..26] == [3, 0] {
                    2000
                } else {
                    1000
                };
                let channel = mt7921_core::ClientPhysicalChannel {
                    band: if frame.channel.band == mt7921_core::PhysicalBand::Ghz2 {
                        0
                    } else {
                        1
                    },
                    primary: frame.channel.number,
                    center: frame.channel.number,
                    center2: 0,
                    bandwidth: 0,
                };
                let command = mt7921_core::encode_client_join_roc_acquire(
                    1,
                    0,
                    self.next_roc_token,
                    channel,
                    duration,
                )
                .map_err(|_| zx::Status::INVALID_ARGS)?;
                self.roc = Some(Roc {
                    token: self.next_roc_token,
                    channel: frame.channel,
                    context: frame.context.clone(),
                    phase: RocPhase::Acquiring {
                        commands: FirmwareCommands::new([(command, RadioResponse::None)].into()),
                        deadline: now + Duration::from_secs(1),
                        grant: None,
                    },
                });
                self.next_roc_token = self.next_roc_token.checked_add(1).unwrap_or(1);
                progressed = true;
            }
            if let Some(roc) = &mut self.roc {
                match &mut roc.phase {
                    RocPhase::Acquiring {
                        commands,
                        deadline,
                        grant,
                    } => {
                        roc.context.check(now)?;
                        if now >= *deadline {
                            return Err(zx::Status::TIMED_OUT);
                        }
                        progressed |= commands.drive(
                            resources,
                            mechanics,
                            receive,
                            start,
                            now,
                            Some(&roc.context),
                        )?;
                        if commands.ready()
                            && let Some(until) = *grant
                        {
                            roc.phase = RocPhase::Granted { until };
                            progressed = true;
                        }
                    }
                    RocPhase::Granted { until } => {
                        // Keep the response window after descriptor completion.
                        // Frames for this same joined BSS can reuse the grant;
                        // Linux mgd_prepare_tx likewise leaves an active ROC in place.
                        if now >= *until || roc.context.check(now).is_err() {
                            let command =
                                mt7921_core::encode_client_join_roc_abort(1, 0, roc.token)
                                    .map_err(|_| zx::Status::INTERNAL)?;
                            roc.phase = RocPhase::Releasing(FirmwareCommands::new(
                                [(command, RadioResponse::None)].into(),
                            ));
                            progressed = true;
                        }
                    }
                    RocPhase::Releasing(commands) => {
                        // Revocation is cleanup, not renewed transmit authority.
                        progressed |=
                            commands.drive(resources, mechanics, receive, start, now, None)?;
                        if commands.ready() {
                            self.roc = None;
                            progressed = true;
                        }
                    }
                }
            }
            let grant_until = self.roc.as_ref().and_then(|roc| match roc.phase {
                RocPhase::Granted { until } => Some(until),
                _ => None,
            });
            progressed |= self.drive_dma(resources, now, grant_until)?;
            Ok(progressed)
        })();
        if result.is_err() {
            self.failed = true;
        }
        result
    }

    /// A turn performs one bounded publication or one completion observation.
    /// `grant_until` must come from the matching firmware ROC owner, never from
    /// a scan result, mailbox admission or a host-only channel setting.
    fn drive_dma<B: Backend>(
        &mut self,
        resources: &mut OwnedHardwareResources<B>,
        now: Instant,
        grant_until: Option<Instant>,
    ) -> Result<bool, zx::Status> {
        if self.failed {
            return Err(zx::Status::BAD_STATE);
        }
        let result = self.step(resources, now, grant_until);
        if result.is_err() {
            self.failed = true;
        }
        result
    }

    fn step<B: Backend>(
        &mut self,
        resources: &mut OwnedHardwareResources<B>,
        now: Instant,
        grant_until: Option<Instant>,
    ) -> Result<bool, zx::Status> {
        if let Some(pending) = self.pending.as_mut() {
            // Revocation never frees a published DMA buffer. Propagate the
            // fault to containment while retaining this entire pending entry.
            pending.frame.context.check(now)?;
            if now >= pending.deadline {
                return Err(zx::Status::TIMED_OUT);
            }
            let didx = resources
                .bar0
                .read_u32(0xd430c)
                .map_err(|_| zx::Status::IO)?;
            if didx != u32::from(pending.slot) && didx != u32::from(pending.next) {
                return Err(zx::Status::IO_DATA_INTEGRITY);
            }
            let mut bytes = [0; DMA_DESCRIPTOR_LEN];
            resources
                .dma
                .management_tx_ring
                .read(usize::from(pending.slot) * DMA_DESCRIPTOR_LEN, &mut bytes)
                .map_err(|_| zx::Status::IO)?;
            let descriptor = crate::active_mcu::descriptor_from_bytes(bytes);
            let changed = !pending.descriptor_done
                && didx == u32::from(pending.next)
                && descriptor.is_dma_done();
            pending.descriptor_done |= changed;
            if !(pending.descriptor_done && pending.freed && pending.status.is_some()) {
                return Ok(changed);
            }
            resources
                .dma
                .management_tx_ring
                .write(
                    usize::from(pending.slot) * DMA_DESCRIPTOR_LEN,
                    &mt7921_core::DmaDescriptor::reset().to_le_bytes(),
                )
                .map_err(|_| zx::Status::IO)?;
            resources
                .dma
                .management_frame
                .write(0, &[0; 4096][..pending.frame.bytes.len()])
                .map_err(|_| zx::Status::IO)?;
            resources
                .dma
                .management_txwi
                .write(0, &[0; 64])
                .map_err(|_| zx::Status::IO)?;
            self.pending = None;
            return Ok(true);
        }
        let Some(frame) = self.queue.front() else {
            return Ok(false);
        };
        frame.context.check(now)?;
        if grant_until.is_none_or(|deadline| now >= deadline) {
            return Ok(false);
        }
        let txwi_iova = resources
            .dma
            .management_txwi
            .device_address(0)
            .map_err(|_| zx::Status::IO)?
            .bits();
        let frame_iova = resources
            .dma
            .management_frame
            .device_address(0)
            .map_err(|_| zx::Status::IO)?
            .bits();
        let encoded = mt7921_core::encode_client_management_tx(
            &frame.bytes,
            txwi_iova,
            frame_iova,
            self.next_token,
            self.next_pid,
            1,
            frame.rate,
        )
        .map_err(|_| zx::Status::INVALID_ARGS)?;
        let didx = resources
            .bar0
            .read_u32(0xd430c)
            .map_err(|_| zx::Status::IO)?;
        let cidx = resources
            .bar0
            .read_u32(0xd4308)
            .map_err(|_| zx::Status::IO)?;
        if didx != u32::from(self.producer) || cidx != u32::from(self.producer) {
            return Err(zx::Status::IO_DATA_INTEGRITY);
        }
        // Retain ownership before the first DMA write, including ambiguous
        // publication failure. The caller must contain on any returned error.
        let pending = PublishedFrame {
            frame: self.queue.pop_front().unwrap(),
            slot: self.producer,
            next: ((u32::from(self.producer) + 1) % MT7921_BAND0_TX_RING_COUNT) as u16,
            token: self.next_token,
            pid: self.next_pid,
            deadline: now + Duration::from_secs(1),
            descriptor_done: false,
            freed: false,
            status: None,
        };
        self.producer = pending.next;
        self.next_token = (self.next_token + 1) % 8192;
        self.next_pid = if self.next_pid == 126 {
            3
        } else {
            self.next_pid + 1
        };
        self.pending = Some(pending);
        let pending = self.pending.as_ref().unwrap();
        resources
            .dma
            .management_frame
            .write(0, &pending.frame.bytes)
            .map_err(|_| zx::Status::IO)?;
        resources
            .dma
            .management_txwi
            .write(0, &encoded.txwi)
            .map_err(|_| zx::Status::IO)?;
        resources
            .dma
            .management_tx_ring
            .write(
                usize::from(pending.slot) * DMA_DESCRIPTOR_LEN,
                &encoded.descriptor.to_le_bytes(),
            )
            .map_err(|_| zx::Status::IO)?;
        // Hardware API orders the prior coherent DMA writes before MMIO.
        pending.frame.context.check(Instant::now())?;
        if grant_until.is_none_or(|deadline| Instant::now() >= deadline) {
            return Err(zx::Status::TIMED_OUT);
        }
        resources
            .bar0
            .write_u32(0xd4308, u32::from(pending.next))
            .map_err(|_| zx::Status::IO)?;
        Ok(true)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use drv_hardware_backends::{DeterministicBackend, Operation};

    fn channel() -> mt7921_core::CandidateChannel {
        mt7921_core::CandidateChannel {
            band: mt7921_core::PhysicalBand::Ghz5,
            number: 149,
            frequency_mhz: 5745,
        }
    }

    fn frame() -> Vec<u8> {
        let mut bytes = vec![0; 30];
        bytes[0] = 0xb0;
        bytes[4..10].copy_from_slice(&[2, 3, 4, 5, 6, 7]);
        bytes
    }

    fn mark_done(
        ring: &mut drv_hardware::CoherentDma<DeterministicBackend, drv_hardware::Bidirectional>,
        model: &drv_hardware_backends::DeviceModel,
        slot: usize,
    ) {
        let mut bytes = [0; DMA_DESCRIPTOR_LEN];
        ring.read(slot * DMA_DESCRIPTOR_LEN, &mut bytes).unwrap();
        bytes[7] |= 0x80;
        model.write_dma(
            ring.device_address(slot * DMA_DESCRIPTOR_LEN)
                .unwrap()
                .bits(),
            bytes.to_vec(),
        );
    }

    #[test]
    fn integrated_radio_turn_coordinates_roc_command_rx_tx_and_release() {
        let (device, log, model) =
            DeterministicBackend::recording_mt7921_device_with_model(Default::default());
        let (mut resources, _) = OwnedHardwareResources::acquire(device).unwrap();
        resources.interrupt = Some(resources.device.open_interrupt(0).unwrap());
        let now = Instant::now();
        let (context, _) =
            wlan_softmac_host::conformance::operation_context(now + Duration::from_secs(10));
        let mut tx = ManagementTx::default();
        let mut mechanics = mt7921_core::LoaderMechanics::default();
        let mut receive = crate::receive::RxRouting::default();
        let mut data_rx = crate::receive::DataRx::default();
        tx.enqueue(context, &frame(), 12, channel()).unwrap();
        crate::softmac::drive_radio_io(
            &mut resources,
            &mut mechanics,
            &mut receive,
            &mut data_rx,
            &mut tx,
            now,
            now,
        )
        .unwrap();
        assert!(tx.pending.is_none());
        let mut grant = mt7921_core::ClientJoinRocGrant {
            bss_index: 0,
            token: 99,
            status: 0,
            primary_channel: 149,
            band: 2,
            bandwidth: 0,
            center_channel: 149,
            request_type: 0,
            max_interval_ms: 1000,
        };
        tx.roc_grant(grant, now).unwrap();
        assert!(matches!(
            tx.roc.as_ref().unwrap().phase,
            RocPhase::Acquiring { grant: None, .. }
        ));
        grant.token = 1;
        tx.roc_grant(grant, now).unwrap();
        // Both independent data RX and events collected by the command pump
        // must escape this same turn even while MCU TX is not reclaimed.
        for (ring, buffers, token) in [
            (
                &resources.dma.mcu_rx_ring,
                &resources.dma.mcu_rx_buffers,
                77u32,
            ),
            (
                &resources.dma.data_rx_ring,
                &resources.dma.data_rx_buffers,
                78u32,
            ),
        ] {
            let address = buffers.device_address(0).unwrap().bits();
            let mut packet = vec![0; 12];
            packet[..4].copy_from_slice(&((6u32 << 27) | (1 << 16) | 12).to_le_bytes());
            packet[8..12].copy_from_slice(&(token << 16).to_le_bytes());
            model.write_dma(address, packet);
            model.write_dma(
                ring.device_address(0).unwrap().bits(),
                mt7921_core::DmaDescriptor {
                    buf0: address as u32,
                    ctrl: (1 << 31) | (1 << 30) | (12 << 16),
                    buf1: 0,
                    info: 0,
                }
                .to_le_bytes()
                .to_vec(),
            );
        }
        let (_, receive_idle, routes) = crate::softmac::drive_radio_io(
            &mut resources,
            &mut mechanics,
            &mut receive,
            &mut data_rx,
            &mut tx,
            now,
            now,
        )
        .unwrap();
        assert!(!receive_idle);
        assert_eq!(routes.len(), 2);
        assert!(
            routes
                .iter()
                .all(|route| matches!(route, mt7921_core::McuRxRoute::TxFree(_)))
        );
        assert!(tx.pending.is_none()); // Grant alone does not reclaim the MCU command.
        mark_done(&mut resources.dma.mcu_tx_ring, &model, 0);
        resources.bar0.write_u32(0xd441c, 1).unwrap();
        crate::softmac::drive_radio_io(
            &mut resources,
            &mut mechanics,
            &mut receive,
            &mut data_rx,
            &mut tx,
            now,
            now,
        )
        .unwrap();
        assert!(tx.pending.is_some());
        let log = log.borrow();
        let roc_publish = log
            .iter()
            .position(|op| {
                matches!(
                    op,
                    Operation::WriteU32 {
                        offset: 0xd4418,
                        value: 1,
                        ..
                    }
                )
            })
            .unwrap();
        let tx_publish = log
            .iter()
            .position(|op| {
                matches!(
                    op,
                    Operation::WriteU32 {
                        offset: 0xd4308,
                        value: 1,
                        ..
                    }
                )
            })
            .unwrap();
        assert!(roc_publish < tx_publish);
        drop(log);
        mark_done(&mut resources.dma.management_tx_ring, &model, 0);
        resources.bar0.write_u32(0xd430c, 1).unwrap();
        tx.tx_free(Mt7921TxFree {
            wcid: Some(1),
            token: 0,
            dropped: false,
            attempts: 1,
            status: 0,
            pair_word: None,
            info_word: 0,
        })
        .unwrap();
        tx.tx_status(Mt7921TxStatus {
            wcid: 1,
            pid: 3,
            acked: true,
        })
        .unwrap();
        crate::softmac::drive_radio_io(
            &mut resources,
            &mut mechanics,
            &mut receive,
            &mut data_rx,
            &mut tx,
            now,
            now,
        )
        .unwrap();
        assert!(tx.pending.is_none());
        assert!(matches!(
            tx.roc.as_ref().unwrap().phase,
            RocPhase::Granted { .. }
        ));
        assert!(!tx.idle());
        let expired = now + Duration::from_secs(1);
        crate::softmac::drive_radio_io(
            &mut resources,
            &mut mechanics,
            &mut receive,
            &mut data_rx,
            &mut tx,
            now,
            expired,
        )
        .unwrap();
        assert!(matches!(
            tx.roc.as_ref().unwrap().phase,
            RocPhase::Releasing(_)
        ));
        crate::softmac::drive_radio_io(
            &mut resources,
            &mut mechanics,
            &mut receive,
            &mut data_rx,
            &mut tx,
            now,
            expired,
        )
        .unwrap();
        assert!(!tx.idle());
        mark_done(&mut resources.dma.mcu_tx_ring, &model, 1);
        resources.bar0.write_u32(0xd441c, 2).unwrap();
        crate::softmac::drive_radio_io(
            &mut resources,
            &mut mechanics,
            &mut receive,
            &mut data_rx,
            &mut tx,
            now,
            expired,
        )
        .unwrap();
        assert!(tx.idle());
    }

    #[test]
    fn missing_or_invalid_roc_grant_never_publishes_management_dma() {
        for invalid_grant in [false, true] {
            let (device, log, _) =
                DeterministicBackend::recording_mt7921_device_with_model(Default::default());
            let (mut resources, _) = OwnedHardwareResources::acquire(device).unwrap();
            resources.interrupt = Some(resources.device.open_interrupt(0).unwrap());
            let now = Instant::now();
            let (context, _) =
                wlan_softmac_host::conformance::operation_context(now + Duration::from_secs(10));
            let mut tx = ManagementTx::default();
            let mut mechanics = mt7921_core::LoaderMechanics::default();
            let mut receive = crate::receive::RxRouting::default();
            tx.enqueue(context, &frame(), 12, channel()).unwrap();
            tx.drive(&mut resources, &mut mechanics, &mut receive, now, now)
                .unwrap();
            if invalid_grant {
                assert_eq!(
                    tx.roc_grant(
                        mt7921_core::ClientJoinRocGrant {
                            bss_index: 0,
                            token: 1,
                            status: 0,
                            primary_channel: 36,
                            band: 2,
                            bandwidth: 0,
                            center_channel: 36,
                            request_type: 0,
                            max_interval_ms: 1000,
                        },
                        now
                    ),
                    Err(zx::Status::IO_DATA_INTEGRITY)
                );
            } else {
                assert_eq!(
                    tx.drive(
                        &mut resources,
                        &mut mechanics,
                        &mut receive,
                        now,
                        now + Duration::from_secs(1)
                    ),
                    Err(zx::Status::TIMED_OUT)
                );
            }
            assert!(tx.roc.is_some());
            assert!(tx.pending.is_none());
            assert!(log.borrow().iter().all(|op| !matches!(
                op,
                Operation::WriteU32 {
                    offset: 0xd4308,
                    ..
                }
            )));
            assert!(tx.failed);
        }
    }

    #[test]
    fn dma_reuse_requires_didx_ddone_token_and_status_in_any_order() {
        for order in [[0, 1, 2, 3], [3, 2, 1, 0], [1, 3, 0, 2], [2, 0, 3, 1]] {
            let (device, _, model) =
                DeterministicBackend::recording_mt7921_device_with_model(Default::default());
            let (mut resources, _) = OwnedHardwareResources::acquire(device).unwrap();
            let now = Instant::now();
            let (context, _) =
                wlan_softmac_host::conformance::operation_context(now + Duration::from_secs(10));
            let mut tx = ManagementTx::default();
            tx.enqueue(context.clone(), &frame(), 12, channel())
                .unwrap();
            tx.enqueue(context, &frame(), 24, channel()).unwrap();
            assert!(
                tx.drive_dma(&mut resources, now, Some(now + Duration::from_secs(1)))
                    .unwrap()
            );
            let mut descriptor = [0; DMA_DESCRIPTOR_LEN];
            resources
                .dma
                .management_tx_ring
                .read(0, &mut descriptor)
                .unwrap();
            for (index, event) in order.into_iter().enumerate() {
                match event {
                    0 => resources.bar0.write_u32(0xd430c, 1).unwrap(),
                    1 => {
                        descriptor[7] |= 0x80;
                        model.write_dma(
                            resources
                                .dma
                                .management_tx_ring
                                .device_address(0)
                                .unwrap()
                                .bits(),
                            descriptor.to_vec(),
                        );
                    }
                    2 => tx
                        .tx_free(Mt7921TxFree {
                            wcid: Some(1),
                            token: 0,
                            dropped: false,
                            attempts: 1,
                            status: 0,
                            pair_word: None,
                            info_word: 0,
                        })
                        .unwrap(),
                    3 => tx
                        .tx_status(Mt7921TxStatus {
                            wcid: 1,
                            pid: 3,
                            acked: true,
                        })
                        .unwrap(),
                    _ => unreachable!(),
                }
                tx.drive_dma(&mut resources, now, None).unwrap();
                assert_eq!(tx.pending.is_none(), index == 3);
                assert_eq!(tx.queue.len(), 1);
                if let Some(pending) = &tx.pending {
                    assert_eq!(pending.frame.bytes, frame());
                }
            }
            assert!(!tx.drive_dma(&mut resources, now, None).unwrap());
            assert!(
                tx.drive_dma(&mut resources, now, Some(now + Duration::from_secs(1)))
                    .unwrap()
            );
            assert_eq!(tx.pending.as_ref().unwrap().token, 1);
            assert_eq!(tx.pending.as_ref().unwrap().pid, 4);
            tx.tx_status(Mt7921TxStatus {
                wcid: 1,
                pid: 3,
                acked: true,
            })
            .unwrap();
            assert_eq!(tx.pending.as_ref().unwrap().status, None);
        }
    }

    #[test]
    fn absent_grant_and_revoked_authority_never_publish_but_published_dma_is_retained() {
        for published in [false, true] {
            let (device, log, _) =
                DeterministicBackend::recording_mt7921_device_with_model(Default::default());
            let (mut resources, _) = OwnedHardwareResources::acquire(device).unwrap();
            let now = Instant::now();
            let (context, revocation) =
                wlan_softmac_host::conformance::operation_context(now + Duration::from_secs(10));
            let mut tx = ManagementTx::default();
            tx.enqueue(context, &frame(), 12, channel()).unwrap();
            assert!(!tx.drive_dma(&mut resources, now, None).unwrap());
            assert!(!tx.drive_dma(&mut resources, now, Some(now)).unwrap());
            if published {
                assert!(
                    tx.drive_dma(&mut resources, now, Some(now + Duration::from_secs(1)))
                        .unwrap()
                );
            }
            revocation();
            assert_eq!(
                tx.drive_dma(&mut resources, now, Some(now + Duration::from_secs(1))),
                Err(zx::Status::CANCELED)
            );
            assert_eq!(tx.pending.is_some(), published);
            assert_eq!(tx.queue.len(), usize::from(!published));
            assert_eq!(
                log.borrow()
                    .iter()
                    .filter(|op| matches!(
                        op,
                        Operation::WriteU32 {
                            offset: 0xd4308,
                            ..
                        }
                    ))
                    .count(),
                usize::from(published)
            );
            assert_eq!(
                tx.drive_dma(&mut resources, now, None),
                Err(zx::Status::BAD_STATE)
            );
        }
    }

    #[test]
    fn admission_is_bounded_and_unsupported_shapes_do_not_consume_slots() {
        let now = Instant::now();
        let (context, _) =
            wlan_softmac_host::conformance::operation_context(now + Duration::from_secs(10));
        let mut tx = ManagementTx::default();
        for (offset, bit) in [(1, 0x40), (1, 0x80), (1, 4), (4, 1), (22, 1)] {
            let mut invalid = frame();
            invalid[offset] |= bit;
            assert_eq!(
                tx.enqueue(context.clone(), &invalid, 12, channel()),
                Err(zx::Status::NOT_SUPPORTED)
            );
        }
        assert!(tx.idle());
        for _ in 0..CAPACITY {
            tx.enqueue(context.clone(), &frame(), 12, channel())
                .unwrap();
        }
        assert_eq!(
            tx.enqueue(context, &frame(), 12, channel()),
            Err(zx::Status::NO_RESOURCES)
        );
    }
}
