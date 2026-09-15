//! Station firmware state and bounded observations needed for PHY/RA setup.
//! Authentication and information-element semantics beyond rate translation
//! remain with MLME/SME; this owner never verifies SAE or authorizes Ethernet.

use crate::radio::{FirmwareCommands, MacPreparation, RadioResponse};
use drv_hardware::Backend;
use mt7921_core::{CandidateChannel, Connac2RxFrame, PhysicalBand};
use std::time::{Duration, Instant};

pub(super) const OFDM_RATES: &[u8] = &[12, 18, 24, 36, 48, 72, 96, 108];
pub(super) const OBSERVATION_CAPACITY: usize = 64;

#[derive(Clone)]
pub(super) struct ObservedBss {
    pub bssid: [u8; 6],
    pub channel: CandidateChannel,
    pub beacon_period: u16,
    observed_at: Instant,
    basic_rates: u16,
    legacy_rates: u16,
}

impl ObservedBss {
    pub fn from_rx(frame: &Connac2RxFrame, now: Instant) -> Option<Self> {
        let bytes = &frame.bytes;
        let control = u16::from_le_bytes(bytes.get(..2)?.try_into().ok()?);
        if !matches!(control & 0x00fc, 0x0080 | 0x0050) || bytes.len() < 36 {
            return None;
        }
        let bssid: [u8; 6] = bytes[16..22].try_into().ok()?;
        if bssid == [0; 6] || bssid[0] & 1 != 0 || bytes[10..16] != bssid {
            return None;
        }
        // Only infrastructure advertisements can establish a client peer.
        if u16::from_le_bytes([bytes[34], bytes[35]]) & 3 != 1 {
            return None;
        }
        let mut rates = Vec::new();
        let mut ies = &bytes[36..];
        while !ies.is_empty() {
            let header = ies.get(..2)?;
            let body = ies.get(2..2 + usize::from(header[1]))?;
            if matches!(header[0], 1 | 50) {
                if rates.len() + body.len() > 32 {
                    return None;
                }
                rates.extend_from_slice(body);
            }
            ies = &ies[2 + usize::from(header[1])..];
        }
        if rates
            .iter()
            .any(|rate| rate & 0x80 != 0 && !OFDM_RATES.contains(&(rate & 0x7f)))
        {
            return None;
        }
        let (band, frequency_mhz) = match frame.band {
            PhysicalBand::Ghz2 => (
                0,
                if frame.channel == 14 {
                    2484
                } else {
                    2407 + u16::from(frame.channel) * 5
                },
            ),
            PhysicalBand::Ghz5 => (1, 5000 + u16::from(frame.channel) * 5),
            PhysicalBand::Ghz6 => return None,
        };
        let (basic_rates, legacy_rates) =
            mt7921_core::linux_preauth_rate_context_reference(band, OFDM_RATES, &rates).ok()?;
        let beacon_period = u16::from_le_bytes([bytes[32], bytes[33]]);
        if beacon_period == 0 {
            return None;
        }
        Some(Self {
            bssid,
            beacon_period,
            observed_at: now,
            basic_rates,
            legacy_rates,
            channel: CandidateChannel {
                band: frame.band,
                number: u16::from(frame.channel),
                frequency_mhz,
            },
        })
    }

    pub fn management_rate(&self) -> u8 {
        let ofdm = if self.channel.band == PhysicalBand::Ghz2 {
            self.basic_rates >> 4
        } else {
            self.basic_rates
        };
        OFDM_RATES[ofdm.trailing_zeros() as usize]
    }

    pub fn fresh(&self, now: Instant) -> bool {
        now.saturating_duration_since(self.observed_at) < Duration::from_secs(30)
    }
}

/// Linux mac_sta_add: clear peer WCID accounting, then publish one enabled
/// STATE_NONE station record. No legacy empty-WTBL pre-reset, preauth BSS/RLM
/// update or extra peer allocation is inserted into the pinned sequence.
pub(super) struct PeerJoin {
    pub context: wlan_softmac_class_support::OperationContext,
    pub bss: ObservedBss,
    clear: MacPreparation,
    commands: FirmwareCommands,
    pub reply: Option<futures_channel::oneshot::Sender<Result<(), zx::Status>>>,
}

impl PeerJoin {
    pub fn new(
        context: wlan_softmac_class_support::OperationContext,
        bss: ObservedBss,
        reply: futures_channel::oneshot::Sender<Result<(), zx::Status>>,
    ) -> Result<Self, zx::Status> {
        let band = if bss.channel.band == PhysicalBand::Ghz2 {
            0
        } else {
            1
        };
        // mt7921_add_interface initializes RSSI EWMA to zero. Only auth/assoc
        // responses update it (mt792x_mac_assoc_rssi), not scan beacons.
        let rcpi = 220;
        let command = mt7921_core::encode_preauth_peer_wcid_command(
            1,
            0,
            1,
            bss.bssid,
            band,
            rcpi,
            bss.basic_rates,
            bss.legacy_rates,
        )
        .map_err(|_| zx::Status::INVALID_ARGS)?;
        Ok(Self {
            context,
            bss,
            clear: MacPreparation::for_wcid(1),
            commands: FirmwareCommands::new([(command, RadioResponse::Unified(3))].into()),
            reply: Some(reply),
        })
    }

    pub fn complete(&self) -> bool {
        self.clear.complete() && self.commands.ready()
    }

    pub fn drive<B: Backend>(
        &mut self,
        resources: &mut crate::OwnedHardwareResources<B>,
        mechanics: &mut mt7921_core::LoaderMechanics,
        receive: &mut crate::receive::RxRouting,
        start: Instant,
        now: Instant,
    ) -> Result<bool, zx::Status> {
        self.context.check(now)?;
        if !self.clear.complete() {
            return self.clear.drive(&resources.bar0, now);
        }
        self.commands.drive(
            resources,
            mechanics,
            receive,
            start,
            now,
            Some(&self.context),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::OwnedHardwareResources;
    use drv_hardware_backends::{DeterministicBackend, Operation};
    use mt7921_core::{DMA_DESCRIPTOR_LEN, DmaDescriptor, LoaderMechanics};

    fn advertisement() -> Connac2RxFrame {
        let mut bytes = vec![0; 36];
        bytes[0] = 0x80;
        bytes[4..10].fill(0xff);
        bytes[10..16].copy_from_slice(&[2, 3, 4, 5, 6, 7]);
        bytes[16..22].copy_from_slice(&[2, 3, 4, 5, 6, 7]);
        bytes[32..34].copy_from_slice(&100u16.to_le_bytes());
        bytes[34] = 1;
        bytes.extend_from_slice(&[1, 8, 0x8c, 18, 0x98, 36, 0xb0, 72, 96, 108]);
        Connac2RxFrame {
            bytes,
            band: PhysicalBand::Ghz5,
            channel: 149,
            rssi_dbm: -60,
            pn: None,
        }
    }

    #[test]
    fn observations_bind_rate_intersection_to_bss_channel_and_age() {
        let now = Instant::now();
        let mut frame = advertisement();
        let bss = ObservedBss::from_rx(&frame, now).unwrap();
        assert_eq!(bss.basic_rates, 0x15);
        assert_eq!(bss.legacy_rates, 0x3fc0);
        assert_eq!(bss.channel.frequency_mhz, 5745);
        assert!(bss.fresh(now + Duration::from_secs(29)));
        assert!(!bss.fresh(now + Duration::from_secs(30)));
        frame.bytes.push(50); // incomplete IE, not partial rate authority
        assert!(ObservedBss::from_rx(&frame, now).is_none());
        frame = advertisement();
        frame.bytes[10] ^= 2; // transmitter differs from BSSID
        assert!(ObservedBss::from_rx(&frame, now).is_none());
        frame = advertisement();
        frame.bytes[38] = 0x82; // unsupported mandatory CCK rate
        assert!(ObservedBss::from_rx(&frame, now).is_none());
        frame = advertisement();
        frame.bytes[34] = 2; // IBSS is not a client peer
        assert!(ObservedBss::from_rx(&frame, now).is_none());
    }

    #[test]
    fn peer_join_clears_wcid_before_one_full_record_and_waits_for_ack_and_reclaim() {
        for valid_cid in [true, false] {
            let (device, log, model) =
                DeterministicBackend::recording_mt7921_device_with_model(Default::default());
            let (mut resources, _) = OwnedHardwareResources::acquire(device).unwrap();
            resources.interrupt = Some(resources.device.open_interrupt(0).unwrap());
            let now = Instant::now();
            let (context, _) = wlan_softmac_class_support::conformance::operation_context(
                now + Duration::from_secs(1),
            );
            let (reply, _receiver) = futures_channel::oneshot::channel();
            let mut join = PeerJoin::new(
                context,
                ObservedBss::from_rx(&advertisement(), now).unwrap(),
                reply,
            )
            .unwrap();
            let mut mechanics = LoaderMechanics::default();
            let mut receive = crate::receive::RxRouting::default();
            // WTBL publication and busy-clear observation are separate turns.
            join.drive(&mut resources, &mut mechanics, &mut receive, now, now)
                .unwrap();
            join.drive(&mut resources, &mut mechanics, &mut receive, now, now)
                .unwrap();
            assert!(!log.borrow().iter().any(|op| matches!(
                op,
                Operation::WriteU32 {
                    offset: 0xd4418,
                    ..
                }
            )));
            join.drive(&mut resources, &mut mechanics, &mut receive, now, now)
                .unwrap();
            let mut tx = [0; DMA_DESCRIPTOR_LEN];
            resources.dma.mcu_tx_ring.read(0, &mut tx).unwrap();
            let control = u32::from_le_bytes(tx[4..8].try_into().unwrap()) | (1 << 31);
            tx[4..8].copy_from_slice(&control.to_le_bytes());
            let mut response = vec![0; 44];
            response[24..26].copy_from_slice(&20u16.to_le_bytes());
            response[28] = 1;
            response[29] = mechanics.sequence();
            response[36] = if valid_cid { 3 } else { 2 };
            let rx = DmaDescriptor {
                buf0: resources
                    .dma
                    .mcu_rx_buffers
                    .device_address(0)
                    .unwrap()
                    .bits() as u32,
                ctrl: (1 << 31) | (1 << 30) | (44 << 16),
                buf1: 0,
                info: 0,
            };
            model.write_dma(
                resources
                    .dma
                    .mcu_rx_buffers
                    .device_address(0)
                    .unwrap()
                    .bits(),
                response,
            );
            model.write_dma(
                resources.dma.mcu_rx_ring.device_address(0).unwrap().bits(),
                rx.to_le_bytes().to_vec(),
            );
            join.drive(&mut resources, &mut mechanics, &mut receive, now, now)
                .unwrap();
            assert!(!join.complete()); // response cannot retire DMA
            model.write_dma(
                resources.dma.mcu_tx_ring.device_address(0).unwrap().bits(),
                tx.to_vec(),
            );
            resources.bar0.write_u32(0xd441c, 1).unwrap();
            let result = join.drive(&mut resources, &mut mechanics, &mut receive, now, now);
            if valid_cid {
                assert!(result.unwrap());
                assert!(join.complete());
            } else {
                assert_eq!(result, Err(zx::Status::IO_DATA_INTEGRITY));
                assert!(!join.complete());
            }
            assert_eq!(
                log.borrow()
                    .iter()
                    .filter(|op| matches!(
                        op,
                        Operation::WriteU32 {
                            offset: 0xd4418,
                            value: 1,
                            ..
                        }
                    ))
                    .count(),
                1
            );
        }
    }

    #[test]
    fn revoked_join_does_not_publish_after_partial_wtbl_effect() {
        let (device, log) = DeterministicBackend::recording_mt7921_activation_device();
        let (mut resources, _) = OwnedHardwareResources::acquire(device).unwrap();
        let now = Instant::now();
        let (context, revoke) = wlan_softmac_class_support::conformance::operation_context(
            now + Duration::from_secs(1),
        );
        let (reply, _) = futures_channel::oneshot::channel();
        let mut join = PeerJoin::new(
            context,
            ObservedBss::from_rx(&advertisement(), now).unwrap(),
            reply,
        )
        .unwrap();
        let mut mechanics = LoaderMechanics::default();
        let mut receive = crate::receive::RxRouting::default();
        join.drive(&mut resources, &mut mechanics, &mut receive, now, now)
            .unwrap();
        revoke();
        let before = log.borrow().len();
        assert_eq!(
            join.drive(&mut resources, &mut mechanics, &mut receive, now, now),
            Err(zx::Status::CANCELED)
        );
        assert_eq!(log.borrow().len(), before);
        assert!(!join.complete());
    }
}
