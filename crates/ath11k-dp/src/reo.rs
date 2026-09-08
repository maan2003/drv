// PORT-MAP: reusable
//! REO command/status and per-peer receive-TID lifecycle.

use alloc::vec::Vec;
use ath11k_hal::{
    PacketNumberType, ReoCommand, ReoCommandKind, ReoCommandParams, ReoQueueDescriptor,
    ReoResources, ReoStatus, RingId, Rings, setup_wcn6750,
};
use ath11k_platform_backend::{Backend, Bidirectional, Device, MmioRegion};
use ath11k_wmi::Transport;
use ath11k_wmi::cmd::{EncodeCommand, PeerReorderQueueSetup};
use dma_pool::{DmaPool, DmaSegment};

use crate::DpError;

/// Non-coherent REO queue descriptor owned by one peer/TID.
pub struct ReoTid<B: Backend> {
    pub tid: u8,
    pub ba_window_size: u32,
    size: usize,
    dma: DmaSegment<B, Bidirectional>,
}

impl<B: Backend> ReoTid<B> {
    /// `ath11k_peer_rx_tid_setup`'s allocation, descriptor setup, and required
    /// post-write sync. The caller performs the WMI reorder-queue command.
    pub fn setup(
        pool: &DmaPool<B, Bidirectional>,
        tid: u8,
        ba_window_size: u32,
        start_sequence: u16,
        pn: PacketNumberType,
    ) -> Result<Self, DpError> {
        let descriptor =
            ReoQueueDescriptor::new(tid, ba_window_size, u32::from(start_sequence), pn);
        let mut dma = pool.allocate().map_err(|_| DpError::NoResources)?;
        dma.write(0, descriptor.bytes())
            .map_err(|_| DpError::DeviceFault)?;
        dma.sync_for_device(0, descriptor.bytes().len())
            .map_err(|_| DpError::DeviceFault)?;
        Ok(Self {
            tid,
            ba_window_size,
            size: descriptor.bytes().len(),
            dma,
        })
    }

    pub fn device_address(
        &self,
    ) -> Result<ath11k_platform_backend::DeviceAddress<'_, B, Bidirectional>, DpError> {
        self.dma.device_address(0).map_err(|_| DpError::DeviceFault)
    }

    fn device_address_at(
        &self,
        offset: usize,
    ) -> Result<ath11k_platform_backend::DeviceAddress<'_, B, Bidirectional>, DpError> {
        self.dma
            .device_address(offset)
            .map_err(|_| DpError::DeviceFault)
    }

    fn descriptor_size(&self) -> usize {
        self.size
    }
}

const REO_QUEUE_DESCRIPTOR_BYTES: usize = 512;
const REO_QUEUE_DESCRIPTOR_ALIGNMENT: usize = 128;
const REO_DESCRIPTOR_FREE_THRESHOLD: usize = 64;
const REO_DESCRIPTOR_FREE_TIMEOUT_MS: u64 = 1_000;
const UPDATE_VALID: u32 = 1 << 9;
const UPDATE_BA_WINDOW_SIZE: u32 = 1 << 18;
const UPDATE_START_SEQUENCE: u32 = 1 << 26;
const START_SEQUENCE_SHIFT: u32 = 11;
const RX_FRAGMENT_TIMEOUT_MS: u64 = 2_000;

#[derive(Debug, Eq, PartialEq)]
#[must_use = "a fragment link descriptor must be returned or retained exactly once"]
pub struct FragmentLinkDescriptor(u64);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u8)]
pub enum FragmentEncryption {
    Wep40 = 0,
    Wep104 = 1,
    TkipNoMic = 2,
    Wep128 = 3,
    TkipMic = 4,
    Wapi = 5,
    Ccmp128 = 6,
    Open = 7,
    Ccmp256 = 8,
    Gcmp128 = 9,
    AesGcmp256 = 10,
    WapiGcmSm4 = 11,
}

#[derive(Debug, Eq, PartialEq)]
pub struct FragmentInput {
    pub vdev_id: u32,
    pub peer_addr: [u8; 6],
    pub tid: u8,
    pub sequence: u16,
    pub fragment_number: u8,
    pub more_fragments: bool,
    pub multicast_broadcast: bool,
    pub sequence_control_valid: bool,
    pub frame_control_valid: bool,
    pub encryption: FragmentEncryption,
    pub decrypted: bool,
    pub packet_number: Option<u64>,
    pub bytes: Vec<u8>,
    pub link_descriptor: FragmentLinkDescriptor,
}

#[derive(Debug, Eq, PartialEq)]
#[must_use = "a completed fragment chain retains a link descriptor owner"]
pub struct CompletedFragmentChain {
    pub vdev_id: u32,
    pub peer_addr: [u8; 6],
    pub tid: u8,
    pub sequence: u16,
    pub fragments: Vec<FragmentPart>,
    pub first_link_descriptor: FragmentLinkDescriptor,
    /// Chains containing decrypted encrypted fragments still require the
    /// source's IV/MIC/ICV normalization and TKIP MMIC boundary before
    /// stage-2 reinjection.
    pub needs_crypto_normalization: bool,
}

#[derive(Debug, Eq, PartialEq)]
pub struct FragmentPart {
    pub fragment_number: u8,
    pub encryption: FragmentEncryption,
    pub decrypted: bool,
    pub packet_number: Option<u64>,
    pub bytes: Vec<u8>,
}

#[derive(Debug, Eq, PartialEq)]
#[must_use = "fragment link dispositions must be consumed"]
pub struct FragmentOutcome {
    pub chain: Result<Option<CompletedFragmentChain>, DpError>,
    pub return_links: Vec<FragmentLinkDescriptor>,
}

#[derive(Debug)]
struct StoredFragment {
    number: u8,
    encryption: FragmentEncryption,
    decrypted: bool,
    packet_number: Option<u64>,
    bytes: Vec<u8>,
}

struct PendingTid<B: Backend> {
    command_number: u16,
    vdev_id: u32,
    peer_addr: [u8; 6],
    tid: ReoTid<B>,
}

struct CachedTid<B: Backend> {
    queued_at_ms: u64,
    vdev_id: u32,
    peer_addr: [u8; 6],
    tid: ReoTid<B>,
}

struct ActiveTid<B: Backend> {
    vdev_id: u32,
    peer_addr: [u8; 6],
    tid: ReoTid<B>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct PeerKey {
    vdev_id: u32,
    peer_addr: [u8; 6],
}

struct FragmentState {
    key: PeerKey,
    tid: u8,
    current_sequence: u16,
    last_fragment: u8,
    bitmap: u16,
    frames: Vec<StoredFragment>,
    first_link: Option<FragmentLinkDescriptor>,
    deadline_ms: u64,
    timer_armed: bool,
}

struct PendingFragmentLink {
    key: PeerKey,
    link: FragmentLinkDescriptor,
}

/// Global peer receive-reorder queue coordinator. Every peer's pending
/// command is kept here because they share one REO status ring.
pub struct PeerRxTids<B: Backend> {
    pool: DmaPool<B, Bidirectional>,
    tids: Vec<ActiveTid<B>>,
    pending_delete: Vec<PendingTid<B>>,
    cached_delete: Vec<CachedTid<B>>,
    pending_flush: Vec<PendingTid<B>>,
    uncertain_setup: Vec<ActiveTid<B>>,
    failed_delete: Vec<ActiveTid<B>>,
    tearing_down: Vec<PeerKey>,
    fragments: Vec<FragmentState>,
    pending_fragment_links: Vec<PendingFragmentLink>,
    peers: Vec<PeerKey>,
}

impl<B: Backend> PeerRxTids<B> {
    pub fn new(device: Device<B>) -> Result<Self, DpError> {
        Ok(Self {
            pool: DmaPool::new(
                device,
                REO_QUEUE_DESCRIPTOR_BYTES,
                4096,
                REO_QUEUE_DESCRIPTOR_ALIGNMENT,
                REO_DESCRIPTOR_FREE_THRESHOLD,
            )
            .map_err(|_| DpError::NoResources)?,
            tids: Vec::new(),
            pending_delete: Vec::new(),
            cached_delete: Vec::new(),
            pending_flush: Vec::new(),
            uncertain_setup: Vec::new(),
            failed_delete: Vec::new(),
            tearing_down: Vec::new(),
            fragments: Vec::new(),
            pending_fragment_links: Vec::new(),
            peers: Vec::new(),
        })
    }

    /// Minimal admission seam called only after firmware peer creation and
    /// the source's routing/initial-queue setup have succeeded. It is not a
    /// port of the broader `ath11k_dp_peer_setup` transaction.
    pub fn register_peer_after_firmware_create(
        &mut self,
        vdev_id: u32,
        peer_addr: [u8; 6],
    ) -> Result<(), DpError> {
        let key = PeerKey { vdev_id, peer_addr };
        if self.tearing_down.contains(&key) {
            return Err(DpError::WrongState);
        }
        if !self.peers.contains(&key) {
            self.peers.push(key);
        }
        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    pub fn ath11k_dp_rx_ampdu_start<R: Rings<B>, T: Transport>(
        &mut self,
        controller: &mut ReoController,
        rings: &mut R,
        wmi: &mut T,
        vdev_id: u32,
        peer_addr: [u8; 6],
        tid: u8,
        ba_window_size: u32,
        start_sequence: u16,
        pn: PacketNumberType,
    ) -> Result<(), DpError> {
        let key = PeerKey { vdev_id, peer_addr };
        if tid >= 16 || !self.peers.contains(&key) || self.tearing_down.contains(&key) {
            return Err(DpError::WrongState);
        }
        self.ath11k_peer_rx_tid_setup(
            controller,
            rings,
            wmi,
            vdev_id,
            peer_addr,
            tid,
            ba_window_size,
            start_sequence,
            pn,
        )
    }

    pub fn ath11k_dp_rx_ampdu_stop<R: Rings<B>, T: Transport>(
        &mut self,
        controller: &mut ReoController,
        rings: &mut R,
        wmi: &mut T,
        vdev_id: u32,
        peer_addr: [u8; 6],
        tid: u8,
    ) -> Result<(), DpError> {
        let key = PeerKey { vdev_id, peer_addr };
        if tid >= 16 || !self.peers.contains(&key) || self.tearing_down.contains(&key) {
            return Err(DpError::WrongState);
        }
        let Some(active) = self.tids.iter_mut().find(|active| {
            active.vdev_id == vdev_id && active.peer_addr == peer_addr && active.tid.tid == tid
        }) else {
            return Ok(());
        };
        controller.ath11k_dp_tx_send_reo_cmd(
            rings,
            ReoCommandKind::UpdateRxQueue,
            &active.tid,
            ReoCommandParams {
                update0: UPDATE_BA_WINDOW_SIZE,
                ba_window_size: 1,
                ..ReoCommandParams::default().need_status()
            },
        )?;
        active.tid.ba_window_size = 1;
        send_reorder_setup(wmi, vdev_id, peer_addr, &active.tid, 1)
    }

    /// Ports `ath11k_peer_rx_tid_setup`, including the already-active update
    /// path and the reorder-queue WMI publication boundary.
    #[allow(clippy::too_many_arguments)]
    pub fn ath11k_peer_rx_tid_setup<R: Rings<B>, T: Transport>(
        &mut self,
        controller: &mut ReoController,
        rings: &mut R,
        wmi: &mut T,
        vdev_id: u32,
        peer_addr: [u8; 6],
        tid: u8,
        ba_window_size: u32,
        start_sequence: u16,
        pn: PacketNumberType,
    ) -> Result<(), DpError> {
        if tid > 16 {
            return Err(DpError::WrongState);
        }
        let key = PeerKey { vdev_id, peer_addr };
        if !self.peers.contains(&key) || self.tearing_down.contains(&key) {
            return Err(DpError::WrongState);
        }
        if self
            .uncertain_setup
            .iter()
            .chain(&self.failed_delete)
            .any(|entry| {
                entry.vdev_id == vdev_id && entry.peer_addr == peer_addr && entry.tid.tid == tid
            })
        {
            return Err(DpError::WrongState);
        }
        if self.pending_delete.iter().any(|entry| {
            entry.vdev_id == vdev_id && entry.peer_addr == peer_addr && entry.tid.tid == tid
        }) {
            // The old queue can still be device-visible until its invalidate
            // status succeeds. Do not publish a replacement whose ownership
            // would become ambiguous if that status later reports failure.
            return Err(DpError::WrongState);
        }
        if let Some(active) = self.tids.iter_mut().find(|active| {
            active.vdev_id == vdev_id && active.peer_addr == peer_addr && active.tid.tid == tid
        }) {
            let params = ReoCommandParams {
                update0: UPDATE_BA_WINDOW_SIZE | UPDATE_START_SEQUENCE,
                update2: u32::from(start_sequence) << START_SEQUENCE_SHIFT,
                ba_window_size: ba_window_size.try_into().map_err(|_| DpError::WrongState)?,
                ..ReoCommandParams::default().need_status()
            };
            controller.ath11k_dp_tx_send_reo_cmd(
                rings,
                ReoCommandKind::UpdateRxQueue,
                &active.tid,
                params,
            )?;
            active.tid.ba_window_size = ba_window_size;
            return send_reorder_setup(wmi, vdev_id, peer_addr, &active.tid, ba_window_size);
        }

        let new_tid = ReoTid::setup(&self.pool, tid, ba_window_size, start_sequence, pn)?;
        if let Err(error) = send_reorder_setup(wmi, vdev_id, peer_addr, &new_tid, ba_window_size) {
            if !T::SEND_ERROR_IS_NON_VISIBLE {
                // The failed command may contain this IOVA. Quarantine the
                // owner until peer teardown rather than permit pool reuse.
                self.uncertain_setup.push(ActiveTid {
                    vdev_id,
                    peer_addr,
                    tid: new_tid,
                });
            }
            return Err(error);
        }
        self.tids.push(ActiveTid {
            vdev_id,
            peer_addr,
            tid: new_tid,
        });
        Ok(())
    }

    /// Ports `ath11k_peer_rx_tid_delete`: remove active publication first,
    /// invalidate the hardware queue, and retain DMA ownership until status.
    pub fn ath11k_peer_rx_tid_delete<R: Rings<B>>(
        &mut self,
        controller: &mut ReoController,
        rings: &mut R,
        vdev_id: u32,
        peer_addr: [u8; 6],
        tid: u8,
    ) -> Result<(), DpError> {
        let Some(index) = self.tids.iter().position(|active| {
            active.vdev_id == vdev_id && active.peer_addr == peer_addr && active.tid.tid == tid
        }) else {
            return Ok(());
        };
        let owned = self.tids.swap_remove(index).tid;
        let result = controller.ath11k_dp_tx_send_reo_cmd(
            rings,
            ReoCommandKind::UpdateRxQueue,
            &owned,
            ReoCommandParams {
                update0: UPDATE_VALID,
                ..ReoCommandParams::default().need_status()
            },
        );
        match result {
            Ok(command_number) => {
                self.pending_delete.push(PendingTid {
                    command_number,
                    vdev_id,
                    peer_addr,
                    tid: owned,
                });
                Ok(())
            }
            Err(error) => {
                self.failed_delete.push(ActiveTid {
                    vdev_id,
                    peer_addr,
                    tid: owned,
                });
                Err(error)
            }
        }
    }

    /// Peer disassociation transaction: prevent new queue publication,
    /// invalidate all TIDs, then purge fragment state. The pinned Linux
    /// cleanup does not send the otherwise-defined WMI reorder-remove command.
    /// Errors leave the peer blocked and every possibly-visible owner
    /// quarantined for reset-time release.
    pub fn ath11k_peer_rx_tid_cleanup<R: Rings<B>>(
        &mut self,
        controller: &mut ReoController,
        rings: &mut R,
        vdev_id: u32,
        peer_addr: [u8; 6],
    ) -> Result<(), DpError> {
        let key = PeerKey { vdev_id, peer_addr };
        if !self.tearing_down.contains(&key) {
            self.tearing_down.push(key);
        }
        self.peers.retain(|peer| *peer != key);

        let mut first_error = None;
        for tid in 0..=16 {
            if let Err(error) =
                self.ath11k_peer_rx_tid_delete(controller, rings, vdev_id, peer_addr, tid)
            {
                first_error.get_or_insert(error);
            }
            self.queue_fragment_cleanup(key, Some(tid));
        }
        self.try_retire_teardown(key);
        first_error.map_or(Ok(()), Err)
    }

    /// `ath11k_peer_frags_flush` / `ath11k_dp_rx_frags_cleanup` bookkeeping
    /// boundary. Full fragment reassembly remains outside this port.
    pub fn ath11k_peer_frags_flush(&mut self, vdev_id: u32, peer_addr: [u8; 6]) {
        let key = PeerKey { vdev_id, peer_addr };
        self.queue_fragment_cleanup(key, None);
    }

    fn queue_fragment_cleanup(&mut self, key: PeerKey, tid: Option<u8>) {
        let mut index = 0;
        while index < self.fragments.len() {
            if self.fragments[index].key == key
                && tid.is_none_or(|tid| self.fragments[index].tid == tid)
            {
                if let Some(link) = self.fragments.swap_remove(index).first_link {
                    self.pending_fragment_links
                        .push(PendingFragmentLink { key, link });
                }
            } else {
                index += 1;
            }
        }
    }

    /// Transfers retained first-link descriptors to the stage-2 WBM release
    /// owner. Until this is called, teardown admission remains closed.
    pub fn take_pending_fragment_link_returns(
        &mut self,
        vdev_id: u32,
        peer_addr: [u8; 6],
    ) -> Vec<FragmentLinkDescriptor> {
        let key = PeerKey { vdev_id, peer_addr };
        let mut links = Vec::new();
        let mut index = 0;
        while index < self.pending_fragment_links.len() {
            if self.pending_fragment_links[index].key == key {
                links.push(self.pending_fragment_links.swap_remove(index).link);
            } else {
                index += 1;
            }
        }
        self.try_retire_teardown(key);
        links
    }

    /// Stage-1 port of `ath11k_dp_rx_frag_h_mpdu`: validate and accumulate a
    /// keyed fragment sequence while making link-descriptor disposition
    /// explicit. The completed chain remains owned by the caller until the
    /// separately deferred hardware reinjection stage exists.
    pub fn ath11k_dp_rx_frag_h_mpdu(
        &mut self,
        input: FragmentInput,
        now_ms: u64,
    ) -> FragmentOutcome {
        let key = PeerKey {
            vdev_id: input.vdev_id,
            peer_addr: input.peer_addr,
        };
        let mut return_links = Vec::new();
        if let Some(index) = self.fragments.iter().position(|state| {
            state.key == key
                && state.tid == input.tid
                && state.timer_armed
                && now_ms >= state.deadline_ms
        }) {
            return_links.extend(self.fragments.swap_remove(index).first_link);
        }
        if input.multicast_broadcast
            || !input.sequence_control_valid
            || !input.frame_control_valid
            || input.tid > 16
            || input.fragment_number > 15
            || (input.fragment_number == 0 && !input.more_fragments)
            || !self.peers.contains(&key)
            || self.tearing_down.contains(&key)
        {
            return_links.push(input.link_descriptor);
            return FragmentOutcome {
                chain: Err(DpError::InvalidFrame),
                return_links,
            };
        }

        let replaced = self
            .fragments
            .iter()
            .position(|state| state.key == key && state.tid == input.tid)
            .filter(|&index| self.fragments[index].current_sequence != input.sequence);
        if let Some(index) = replaced {
            return_links.extend(self.fragments.swap_remove(index).first_link);
        }

        let state_index = self
            .fragments
            .iter()
            .position(|state| state.key == key && state.tid == input.tid)
            .unwrap_or_else(|| {
                self.fragments.push(FragmentState {
                    key,
                    tid: input.tid,
                    current_sequence: input.sequence,
                    last_fragment: 0,
                    bitmap: 0,
                    frames: Vec::new(),
                    first_link: None,
                    deadline_ms: 0,
                    timer_armed: false,
                });
                self.fragments.len() - 1
            });
        let state = &mut self.fragments[state_index];
        let bit = 1u16 << input.fragment_number;
        if state.bitmap & bit != 0 {
            return_links.push(input.link_descriptor);
            return FragmentOutcome {
                chain: Err(DpError::InvalidFrame),
                return_links,
            };
        }
        let insert_at = state
            .frames
            .iter()
            .position(|fragment| fragment.number > input.fragment_number)
            .unwrap_or(state.frames.len());
        state.frames.insert(
            insert_at,
            StoredFragment {
                number: input.fragment_number,
                encryption: input.encryption,
                decrypted: input.decrypted,
                packet_number: input.packet_number,
                bytes: input.bytes,
            },
        );
        state.bitmap |= bit;
        if input.fragment_number == 0 {
            state.first_link = Some(input.link_descriptor);
        } else {
            return_links.push(input.link_descriptor);
        }
        if !input.more_fragments {
            state.last_fragment = input.fragment_number;
        }
        let complete_mask = if state.last_fragment == 15 {
            u16::MAX
        } else {
            (1u16 << (state.last_fragment + 1)) - 1
        };
        if state.last_fragment == 0 || state.bitmap != complete_mask {
            state.timer_armed = true;
            state.deadline_ms = now_ms.saturating_add(RX_FRAGMENT_TIMEOUT_MS);
            return FragmentOutcome {
                chain: Ok(None),
                return_links,
            };
        }

        let state = self.fragments.swap_remove(state_index);
        let Some(first_link_descriptor) = state.first_link else {
            return FragmentOutcome {
                chain: Err(DpError::InvalidFrame),
                return_links,
            };
        };
        let encryption = state.frames[0].encryption;
        let needs_crypto_normalization = state
            .frames
            .iter()
            .any(|fragment| fragment.decrypted && fragment.encryption != FragmentEncryption::Open);
        if matches!(
            encryption,
            FragmentEncryption::Ccmp128
                | FragmentEncryption::Ccmp256
                | FragmentEncryption::Gcmp128
                | FragmentEncryption::AesGcmp256
        ) && !packet_numbers_are_consecutive(&state.frames)
        {
            return_links.push(first_link_descriptor);
            return FragmentOutcome {
                chain: Err(DpError::InvalidFrame),
                return_links,
            };
        }
        FragmentOutcome {
            chain: Ok(Some(CompletedFragmentChain {
                vdev_id: key.vdev_id,
                peer_addr: key.peer_addr,
                tid: state.tid,
                sequence: state.current_sequence,
                fragments: state
                    .frames
                    .into_iter()
                    .map(|fragment| FragmentPart {
                        fragment_number: fragment.number,
                        encryption: fragment.encryption,
                        decrypted: fragment.decrypted,
                        packet_number: fragment.packet_number,
                        bytes: fragment.bytes,
                    })
                    .collect(),
                first_link_descriptor,
                needs_crypto_normalization,
            })),
            return_links,
        }
    }

    /// Manual polling seam for `ath11k_dp_rx_frag_timer`; returns retained
    /// first-link owners which the caller must publish to the real WBM release
    /// path in stage 2. No production scheduler is wired yet.
    pub fn expire_incomplete_fragments(&mut self, now_ms: u64) -> Vec<FragmentLinkDescriptor> {
        let mut links = Vec::new();
        let mut index = 0;
        while index < self.fragments.len() {
            if self.fragments[index].timer_armed && now_ms >= self.fragments[index].deadline_ms {
                if let Some(link) = self.fragments.swap_remove(index).first_link {
                    links.push(link);
                }
            } else {
                index += 1;
            }
        }
        links
    }

    /// Release all possibly device-visible peer state only after reset has
    /// proven that firmware and REO can no longer dereference its DMA IOVAs.
    pub fn release_after_device_reset(self) {}

    /// Ports `ath11k_dp_rx_tid_del_func` and its aged REO cache invalidation.
    pub fn ath11k_dp_rx_tid_del_func<R: Rings<B>>(
        &mut self,
        controller: &mut ReoController,
        rings: &mut R,
        now_ms: u64,
    ) -> Result<Vec<ReoStatus>, DpError> {
        let mut statuses = Vec::new();
        while let Some(descriptor) = rings.consume(controller.status_ring).map_err(map_hal)? {
            let status = ReoStatus::decode(&descriptor).map_err(map_hal)?;
            self.apply_status(status, now_ms);
            statuses.push(status);
        }
        self.flush_aged(controller, rings, now_ms)?;
        Ok(statuses)
    }

    fn apply_status(&mut self, status: ReoStatus, now_ms: u64) {
        let mut completed_key = None;
        if let Some(index) = self
            .pending_delete
            .iter()
            .position(|pending| pending.command_number == status.header.command_number)
        {
            let pending = self.pending_delete.swap_remove(index);
            if status.header.execution_status == 0 {
                self.cached_delete.push(CachedTid {
                    queued_at_ms: now_ms,
                    vdev_id: pending.vdev_id,
                    peer_addr: pending.peer_addr,
                    tid: pending.tid,
                });
            } else {
                self.failed_delete.push(ActiveTid {
                    vdev_id: pending.vdev_id,
                    peer_addr: pending.peer_addr,
                    tid: pending.tid,
                });
            }
        } else if let Some(index) = self
            .pending_flush
            .iter()
            .position(|pending| pending.command_number == status.header.command_number)
        {
            // Success and failure both release the host owner, matching
            // ath11k_dp_reo_cmd_free's terminal callback behavior.
            let pending = self.pending_flush.swap_remove(index);
            completed_key = Some(PeerKey {
                vdev_id: pending.vdev_id,
                peer_addr: pending.peer_addr,
            });
        }
        if let Some(key) = completed_key {
            self.try_retire_teardown(key);
        }
    }

    pub fn flush_aged<R: Rings<B>>(
        &mut self,
        controller: &mut ReoController,
        rings: &mut R,
        now_ms: u64,
    ) -> Result<(), DpError> {
        let mut index = 0;
        while index < self.cached_delete.len() {
            let aged = now_ms.saturating_sub(self.cached_delete[index].queued_at_ms)
                > REO_DESCRIPTOR_FREE_TIMEOUT_MS;
            if self.cached_delete.len() > REO_DESCRIPTOR_FREE_THRESHOLD || aged {
                let cached = self.cached_delete.swap_remove(index);
                let key = PeerKey {
                    vdev_id: cached.vdev_id,
                    peer_addr: cached.peer_addr,
                };
                if let Err(error) = self.flush_one(controller, rings, cached) {
                    self.try_retire_teardown(key);
                    return Err(error);
                }
            } else {
                index += 1;
            }
        }
        Ok(())
    }

    fn flush_one<R: Rings<B>>(
        &mut self,
        controller: &mut ReoController,
        rings: &mut R,
        cached: CachedTid<B>,
    ) -> Result<(), DpError> {
        let tid = cached.tid;
        let mut offset = tid.descriptor_size();
        while offset > 128 {
            offset -= 128;
            let _ = controller.send_at(
                rings,
                ReoCommandKind::FlushCache,
                &tid,
                offset,
                ReoCommandParams::default(),
            );
        }
        let command_number = controller.send_at(
            rings,
            ReoCommandKind::FlushCache,
            &tid,
            0,
            ReoCommandParams::default().need_status(),
        )?;
        self.pending_flush.push(PendingTid {
            command_number,
            vdev_id: cached.vdev_id,
            peer_addr: cached.peer_addr,
            tid,
        });
        Ok(())
    }

    pub fn is_active(&self, vdev_id: u32, peer_addr: [u8; 6], tid: u8) -> bool {
        self.tids.iter().any(|active| {
            active.vdev_id == vdev_id && active.peer_addr == peer_addr && active.tid.tid == tid
        })
    }

    fn try_retire_teardown(&mut self, key: PeerKey) {
        let active = self
            .tids
            .iter()
            .any(|entry| entry.vdev_id == key.vdev_id && entry.peer_addr == key.peer_addr);
        let pending = self
            .pending_delete
            .iter()
            .chain(&self.pending_flush)
            .any(|entry| entry.vdev_id == key.vdev_id && entry.peer_addr == key.peer_addr);
        let cached = self
            .cached_delete
            .iter()
            .any(|entry| entry.vdev_id == key.vdev_id && entry.peer_addr == key.peer_addr);
        let quarantined = self
            .uncertain_setup
            .iter()
            .chain(&self.failed_delete)
            .any(|entry| entry.vdev_id == key.vdev_id && entry.peer_addr == key.peer_addr);
        let fragments = self.fragments.iter().any(|entry| entry.key == key);
        let fragment_links = self
            .pending_fragment_links
            .iter()
            .any(|entry| entry.key == key);
        if !(active || pending || cached || quarantined || fragments || fragment_links) {
            self.tearing_down.retain(|teardown| *teardown != key);
        }
    }
}

fn packet_numbers_are_consecutive(fragments: &[StoredFragment]) -> bool {
    fragments.windows(2).all(|pair| {
        pair[0]
            .packet_number
            .zip(pair[1].packet_number)
            .is_some_and(|(previous, current)| previous.checked_add(1) == Some(current))
    })
}

fn send_reorder_setup<B: Backend, T: Transport>(
    wmi: &mut T,
    vdev_id: u32,
    peer_addr: [u8; 6],
    tid: &ReoTid<B>,
    ba_window_size: u32,
) -> Result<(), DpError> {
    let request = PeerReorderQueueSetup {
        vdev_id,
        peer_addr,
        tid: tid.tid,
        queue_address: tid.device_address()?.bits(),
        ba_window_size_valid: 1,
        ba_window_size,
    };
    let command = request.encode_command().map_err(|_| DpError::DeviceFault)?;
    wmi.send(command).map_err(|_| DpError::DeviceFault)
}

pub struct ReoController {
    command_ring: RingId,
    status_ring: RingId,
    next_command_number: u16,
    resources: ReoResources,
}

impl ReoController {
    /// `ath11k_dp_pdev_reo_setup`, after the coherent command ring has been
    /// initialized by HAL's `initialize_command_ring` during ring allocation.
    pub fn ath11k_dp_pdev_reo_setup<B: Backend>(
        mmio: &MmioRegion<B>,
        command_ring: RingId,
        status_ring: RingId,
    ) -> Result<Self, DpError> {
        setup_wcn6750(mmio).map_err(map_hal)?;
        Ok(Self {
            command_ring,
            status_ring,
            next_command_number: 1,
            resources: ReoResources::default(),
        })
    }

    pub fn ath11k_dp_pdev_reo_cleanup(self) {}

    pub fn ath11k_dp_tx_send_reo_cmd<B: Backend, R: Rings<B>>(
        &mut self,
        rings: &mut R,
        kind: ReoCommandKind,
        tid: &ReoTid<B>,
        params: ReoCommandParams,
    ) -> Result<u16, DpError> {
        self.send_at(rings, kind, tid, 0, params)
    }

    fn send_at<B: Backend, R: Rings<B>>(
        &mut self,
        rings: &mut R,
        kind: ReoCommandKind,
        tid: &ReoTid<B>,
        offset: usize,
        params: ReoCommandParams,
    ) -> Result<u16, DpError> {
        let command_number = self.next_command_number;
        self.next_command_number = self.next_command_number.wrapping_add(1).max(1);
        let command = ReoCommand::encode(
            command_number,
            kind,
            &tid.device_address_at(offset)?,
            params,
            &mut self.resources,
        )
        .map_err(map_hal)?;
        rings
            .publish(self.command_ring, command.into_descriptor())
            .map_err(map_hal)?;
        Ok(command_number)
    }
}

fn map_hal(error: ath11k_hal::HalError) -> DpError {
    match error {
        ath11k_hal::HalError::WrongDescriptorLength => DpError::MalformedDescriptor,
        ath11k_hal::HalError::NoResources => DpError::NoResources,
        ath11k_hal::HalError::DeviceFault => DpError::DeviceFault,
        ath11k_hal::HalError::Unsupported => DpError::UnsupportedDescriptor,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::collections::VecDeque;
    use alloc::vec;
    use ath11k_hal::{Descriptor, HalError, RingKind, RingMemory};
    use drv_hardware_backends::{DeterministicBackend, Operation};

    mod stateful_tests;

    #[derive(Default)]
    struct ModelRings {
        published: Vec<(RingId, Descriptor)>,
        status: VecDeque<Descriptor>,
        fail_publish: bool,
        fail_publish_attempt: Option<usize>,
        publish_attempts: usize,
    }

    impl Rings<DeterministicBackend> for ModelRings {
        fn create(
            &mut self,
            _: RingKind,
            _: RingMemory<DeterministicBackend>,
        ) -> Result<RingId, HalError> {
            Ok(RingId(0))
        }
        fn publish(&mut self, ring: RingId, descriptor: Descriptor) -> Result<(), HalError> {
            self.publish_attempts += 1;
            if self.fail_publish || self.fail_publish_attempt == Some(self.publish_attempts) {
                return Err(HalError::NoResources);
            }
            self.published.push((ring, descriptor));
            Ok(())
        }
        fn consume(&mut self, _: RingId) -> Result<Option<Descriptor>, HalError> {
            Ok(self.status.pop_front())
        }
    }

    #[test]
    fn tid_descriptor_is_synced_before_reo_command_publication() {
        let (device, operations) = DeterministicBackend::recording_noncoherent_device();
        let pool = DmaPool::new(device, 512, 4096, 128, 64).unwrap();
        let tid = ReoTid::setup(&pool, 3, 64, 0x123, PacketNumberType::Wpa).unwrap();
        assert!(matches!(
            operations.borrow().last(),
            Some(Operation::SyncForDevice { range, .. }) if range.end - range.start == 512
        ));
        let mut controller = ReoController {
            command_ring: RingId(8),
            status_ring: RingId(9),
            next_command_number: 1,
            resources: ReoResources::default(),
        };
        let mut rings = ModelRings::default();
        let number = controller
            .ath11k_dp_tx_send_reo_cmd(
                &mut rings,
                ReoCommandKind::QueueStats,
                &tid,
                ReoCommandParams::default().need_status(),
            )
            .unwrap();
        assert_eq!(number, 1);
        assert_eq!(rings.published[0].0, RingId(8));
        assert_eq!(rings.published[0].1.bytes().len(), 40);
    }

    #[derive(Default)]
    struct ModelWmi {
        commands: Vec<ath11k_wmi::Command>,
        fail: bool,
    }

    impl Transport for ModelWmi {
        const SEND_ERROR_IS_NON_VISIBLE: bool = true;

        fn send(&mut self, command: ath11k_wmi::Command) -> Result<(), ath11k_wmi::WmiError> {
            if self.fail {
                return Err(ath11k_wmi::WmiError::Transport);
            }
            self.commands.push(command);
            Ok(())
        }

        fn receive(&mut self, _: u64) -> Result<Option<ath11k_wmi::Event>, ath11k_wmi::WmiError> {
            Ok(None)
        }
    }

    #[derive(Default)]
    struct UncertainWmi;

    impl Transport for UncertainWmi {
        fn send(&mut self, _: ath11k_wmi::Command) -> Result<(), ath11k_wmi::WmiError> {
            Err(ath11k_wmi::WmiError::Transport)
        }

        fn receive(&mut self, _: u64) -> Result<Option<ath11k_wmi::Event>, ath11k_wmi::WmiError> {
            Ok(None)
        }
    }

    fn controller() -> ReoController {
        ReoController {
            command_ring: RingId(8),
            status_ring: RingId(9),
            next_command_number: 1,
            resources: ReoResources::default(),
        }
    }

    fn status(command_number: u16, kind: u32, execution_status: u8) -> Descriptor {
        let mut bytes = vec![0; 104];
        let header = (kind & 0x1ff) << 1 | 100 << 10;
        bytes[0..4].copy_from_slice(&header.to_le_bytes());
        let info = u32::from(command_number) | u32::from(execution_status) << 26;
        bytes[4..8].copy_from_slice(&info.to_le_bytes());
        Descriptor::new(bytes, 104).unwrap()
    }

    fn fragment(
        address: [u8; 6],
        sequence: u16,
        number: u8,
        more: bool,
        pn: Option<u64>,
        link: u64,
    ) -> FragmentInput {
        FragmentInput {
            vdev_id: 9,
            peer_addr: address,
            tid: 3,
            sequence,
            fragment_number: number,
            more_fragments: more,
            multicast_broadcast: false,
            sequence_control_valid: true,
            frame_control_valid: true,
            encryption: FragmentEncryption::Ccmp128,
            decrypted: true,
            packet_number: pn,
            bytes: vec![number],
            link_descriptor: FragmentLinkDescriptor(link),
        }
    }

    #[test]
    fn peer_tid_setup_syncs_then_publishes_typed_wmi_and_rolls_back_on_error() {
        let (device, operations) = DeterministicBackend::recording_noncoherent_device();
        let mut peer = PeerRxTids::new(device).unwrap();
        let mut rings = ModelRings::default();
        let mut reo = controller();
        let mut wmi = ModelWmi::default();
        assert_eq!(
            peer.ath11k_peer_rx_tid_setup(
                &mut reo,
                &mut rings,
                &mut wmi,
                4,
                [1, 2, 3, 4, 5, 6],
                3,
                64,
                0x123,
                PacketNumberType::Wpa,
            ),
            Err(DpError::WrongState)
        );
        assert!(wmi.commands.is_empty());
        peer.register_peer_after_firmware_create(4, [1, 2, 3, 4, 5, 6])
            .unwrap();
        peer.ath11k_peer_rx_tid_setup(
            &mut reo,
            &mut rings,
            &mut wmi,
            4,
            [1, 2, 3, 4, 5, 6],
            3,
            64,
            0x123,
            PacketNumberType::Wpa,
        )
        .unwrap();
        assert!(peer.is_active(4, [1, 2, 3, 4, 5, 6], 3));
        assert_eq!(wmi.commands.len(), 1);
        assert_eq!(
            wmi.commands[0].id,
            ath11k_wmi::tags::WMI_PEER_REORDER_QUEUE_SETUP_CMDID
        );
        assert!(
            matches!(operations.borrow().last(), Some(Operation::SyncForDevice { range, .. }) if range.end - range.start == 512)
        );

        let mut failed_wmi = ModelWmi {
            fail: true,
            ..ModelWmi::default()
        };
        assert_eq!(
            peer.ath11k_peer_rx_tid_setup(
                &mut reo,
                &mut rings,
                &mut failed_wmi,
                4,
                [1, 2, 3, 4, 5, 6],
                5,
                32,
                0,
                PacketNumberType::None,
            ),
            Err(DpError::DeviceFault)
        );
        assert!(!peer.is_active(4, [1, 2, 3, 4, 5, 6], 5));
        assert_eq!(peer.pool.free_segments(), 7);
    }

    #[test]
    fn delete_keeps_owner_until_update_and_flush_statuses() {
        let device = DeterministicBackend::device();
        let mut peer = PeerRxTids::new(device).unwrap();
        let mut rings = ModelRings::default();
        let mut reo = controller();
        let mut wmi = ModelWmi::default();
        peer.register_peer_after_firmware_create(1, [2; 6]).unwrap();
        peer.ath11k_peer_rx_tid_setup(
            &mut reo,
            &mut rings,
            &mut wmi,
            1,
            [2; 6],
            3,
            64,
            0,
            PacketNumberType::None,
        )
        .unwrap();
        peer.ath11k_peer_rx_tid_delete(&mut reo, &mut rings, 1, [2; 6], 3)
            .unwrap();
        assert!(!peer.is_active(1, [2; 6], 3));
        assert_eq!(peer.pending_delete.len(), 1);
        assert_eq!(peer.pool.free_segments(), 7);

        rings.status.push_back(status(1, 153, 0));
        peer.ath11k_dp_rx_tid_del_func(&mut reo, &mut rings, 0)
            .unwrap();
        assert_eq!(peer.pending_delete.len(), 0);
        assert_eq!(peer.cached_delete.len(), 1);
        peer.flush_aged(&mut reo, &mut rings, 1_001).unwrap();
        assert_eq!(peer.cached_delete.len(), 0);
        assert_eq!(peer.pending_flush.len(), 1);
        // Three extension flushes plus the base descriptor flush.
        assert_eq!(rings.published.len(), 5);

        rings.status.push_back(status(5, 313, 0));
        peer.ath11k_dp_rx_tid_del_func(&mut reo, &mut rings, 1_001)
            .unwrap();
        assert_eq!(peer.pending_flush.len(), 0);
        assert_eq!(peer.pool.free_segments(), 8);
    }

    #[test]
    fn setup_sync_and_delete_publication_failures_leak_no_owner() {
        let (device, failures) = DeterministicBackend::noncoherent_device_with_failures();
        let mut peer = PeerRxTids::new(device).unwrap();
        let mut reo = controller();
        let mut rings = ModelRings::default();
        let mut wmi = ModelWmi::default();
        peer.register_peer_after_firmware_create(1, [3; 6]).unwrap();
        failures.fail_next_sync_for_device();
        assert_eq!(
            peer.ath11k_peer_rx_tid_setup(
                &mut reo,
                &mut rings,
                &mut wmi,
                1,
                [3; 6],
                2,
                64,
                0,
                PacketNumberType::None,
            ),
            Err(DpError::NoResources)
        );
        assert!(!peer.is_active(1, [3; 6], 2));
        assert!(wmi.commands.is_empty());

        peer.ath11k_peer_rx_tid_setup(
            &mut reo,
            &mut rings,
            &mut wmi,
            1,
            [3; 6],
            2,
            64,
            0,
            PacketNumberType::None,
        )
        .unwrap();
        rings.fail_publish = true;
        assert_eq!(
            peer.ath11k_peer_rx_tid_delete(&mut reo, &mut rings, 1, [3; 6], 2),
            Err(DpError::NoResources)
        );
        assert!(!peer.is_active(1, [3; 6], 2));
        assert!(peer.pending_delete.is_empty());
    }

    #[test]
    fn uncertain_wmi_failure_quarantines_descriptor_without_active_publication() {
        let mut peer = PeerRxTids::new(DeterministicBackend::device()).unwrap();
        let mut reo = controller();
        let mut rings = ModelRings::default();
        peer.register_peer_after_firmware_create(1, [9; 6]).unwrap();
        assert_eq!(
            peer.ath11k_peer_rx_tid_setup(
                &mut reo,
                &mut rings,
                &mut UncertainWmi,
                1,
                [9; 6],
                4,
                64,
                0,
                PacketNumberType::None,
            ),
            Err(DpError::DeviceFault)
        );
        assert!(!peer.is_active(1, [9; 6], 4));
        assert_eq!(peer.uncertain_setup.len(), 1);
        assert_eq!(peer.pool.free_segments(), 7);
        let mut retry = ModelWmi::default();
        assert_eq!(
            peer.ath11k_peer_rx_tid_setup(
                &mut reo,
                &mut rings,
                &mut retry,
                1,
                [9; 6],
                4,
                64,
                0,
                PacketNumberType::None,
            ),
            Err(DpError::WrongState)
        );
        assert!(retry.commands.is_empty());
    }

    #[test]
    fn every_extension_flush_failure_still_gates_owner_on_base_status() {
        for failed_extension in 0..3 {
            let mut peer = PeerRxTids::new(DeterministicBackend::device()).unwrap();
            let mut reo = controller();
            let mut rings = ModelRings::default();
            let mut wmi = ModelWmi::default();
            peer.register_peer_after_firmware_create(1, [4; 6]).unwrap();
            peer.ath11k_peer_rx_tid_setup(
                &mut reo,
                &mut rings,
                &mut wmi,
                1,
                [4; 6],
                3,
                64,
                0,
                PacketNumberType::None,
            )
            .unwrap();
            peer.ath11k_peer_rx_tid_delete(&mut reo, &mut rings, 1, [4; 6], 3)
                .unwrap();
            rings.status.push_back(status(1, 153, 0));
            peer.ath11k_dp_rx_tid_del_func(&mut reo, &mut rings, 0)
                .unwrap();

            rings.fail_publish_attempt = Some(2 + failed_extension);
            peer.flush_aged(&mut reo, &mut rings, 1_001).unwrap();
            assert_eq!(peer.pending_flush.len(), 1);
            assert_eq!(peer.pool.free_segments(), 7);
            rings.status.push_back(status(5, 313, 0));
            peer.ath11k_dp_rx_tid_del_func(&mut reo, &mut rings, 1_001)
                .unwrap();
            assert!(peer.pending_flush.is_empty());
            assert_eq!(peer.pool.free_segments(), 8);
        }
    }

    #[test]
    fn malformed_status_does_not_discard_applied_completion_prefix() {
        let mut peer = PeerRxTids::new(DeterministicBackend::device()).unwrap();
        let mut reo = controller();
        let mut rings = ModelRings::default();
        let mut wmi = ModelWmi::default();
        peer.register_peer_after_firmware_create(1, [5; 6]).unwrap();
        peer.ath11k_peer_rx_tid_setup(
            &mut reo,
            &mut rings,
            &mut wmi,
            1,
            [5; 6],
            3,
            64,
            0,
            PacketNumberType::None,
        )
        .unwrap();
        peer.ath11k_peer_rx_tid_delete(&mut reo, &mut rings, 1, [5; 6], 3)
            .unwrap();
        rings.status.push_back(status(1, 153, 0));
        rings
            .status
            .push_back(Descriptor::new(vec![0; 40], 40).unwrap());
        assert_eq!(
            peer.ath11k_dp_rx_tid_del_func(&mut reo, &mut rings, 0),
            Err(DpError::MalformedDescriptor)
        );
        assert!(peer.pending_delete.is_empty());
        assert_eq!(peer.cached_delete.len(), 1);

        peer.flush_aged(&mut reo, &mut rings, 1_001).unwrap();
        rings.status.push_back(status(5, 313, 0));
        rings
            .status
            .push_back(Descriptor::new(vec![0; 40], 40).unwrap());
        assert_eq!(
            peer.ath11k_dp_rx_tid_del_func(&mut reo, &mut rings, 1_001),
            Err(DpError::MalformedDescriptor)
        );
        assert!(peer.pending_flush.is_empty());
        assert_eq!(peer.pool.free_segments(), 8);
    }

    #[test]
    fn global_status_dispatch_handles_two_peers_sharing_the_same_tid() {
        let mut peers = PeerRxTids::new(DeterministicBackend::device()).unwrap();
        let mut reo = controller();
        let mut rings = ModelRings::default();
        let mut wmi = ModelWmi::default();
        peers
            .register_peer_after_firmware_create(1, [6; 6])
            .unwrap();
        peers
            .register_peer_after_firmware_create(1, [7; 6])
            .unwrap();
        for addr in [[6; 6], [7; 6]] {
            peers
                .ath11k_peer_rx_tid_setup(
                    &mut reo,
                    &mut rings,
                    &mut wmi,
                    1,
                    addr,
                    3,
                    64,
                    0,
                    PacketNumberType::None,
                )
                .unwrap();
            peers
                .ath11k_peer_rx_tid_delete(&mut reo, &mut rings, 1, addr, 3)
                .unwrap();
        }
        rings.status.push_back(status(2, 153, 0));
        rings.status.push_back(status(1, 153, 0));
        peers
            .ath11k_dp_rx_tid_del_func(&mut reo, &mut rings, 0)
            .unwrap();
        assert!(peers.pending_delete.is_empty());
        assert_eq!(peers.cached_delete.len(), 2);
    }

    #[test]
    fn failed_invalidate_execution_quarantines_owner_until_reset() {
        let mut peers = PeerRxTids::new(DeterministicBackend::device()).unwrap();
        let mut reo = controller();
        let mut rings = ModelRings::default();
        let mut wmi = ModelWmi::default();
        peers
            .register_peer_after_firmware_create(1, [8; 6])
            .unwrap();
        peers
            .ath11k_peer_rx_tid_setup(
                &mut reo,
                &mut rings,
                &mut wmi,
                1,
                [8; 6],
                3,
                64,
                0,
                PacketNumberType::None,
            )
            .unwrap();
        peers
            .ath11k_peer_rx_tid_delete(&mut reo, &mut rings, 1, [8; 6], 3)
            .unwrap();
        assert_eq!(
            peers.ath11k_peer_rx_tid_setup(
                &mut reo,
                &mut rings,
                &mut wmi,
                1,
                [8; 6],
                3,
                64,
                0,
                PacketNumberType::None,
            ),
            Err(DpError::WrongState)
        );
        rings.status.push_back(status(1, 153, 2));
        peers
            .ath11k_dp_rx_tid_del_func(&mut reo, &mut rings, 0)
            .unwrap();
        assert!(peers.pending_delete.is_empty());
        assert_eq!(peers.failed_delete.len(), 1);
        assert_eq!(peers.pool.free_segments(), 7);
        assert_eq!(
            peers.ath11k_peer_rx_tid_setup(
                &mut reo,
                &mut rings,
                &mut wmi,
                1,
                [8; 6],
                3,
                64,
                0,
                PacketNumberType::None,
            ),
            Err(DpError::WrongState)
        );
    }

    #[test]
    fn peer_cleanup_invalidates_all_active_tids_and_is_idempotent() {
        let mut peers = PeerRxTids::new(DeterministicBackend::device()).unwrap();
        let mut reo = controller();
        let mut rings = ModelRings::default();
        let mut wmi = ModelWmi::default();
        peers
            .register_peer_after_firmware_create(2, [0xaa; 6])
            .unwrap();
        peers
            .register_peer_after_firmware_create(2, [0xbb; 6])
            .unwrap();
        for tid in [0, 8, 16] {
            peers
                .ath11k_peer_rx_tid_setup(
                    &mut reo,
                    &mut rings,
                    &mut wmi,
                    2,
                    [0xaa; 6],
                    tid,
                    1,
                    0,
                    PacketNumberType::None,
                )
                .unwrap();
        }
        peers
            .ath11k_peer_rx_tid_setup(
                &mut reo,
                &mut rings,
                &mut wmi,
                2,
                [0xbb; 6],
                8,
                1,
                0,
                PacketNumberType::None,
            )
            .unwrap();
        let first = peers.ath11k_dp_rx_frag_h_mpdu(
            FragmentInput {
                vdev_id: 2,
                tid: 8,
                encryption: FragmentEncryption::Open,
                packet_number: None,
                bytes: vec![1, 2],
                ..fragment([0xaa; 6], 4, 0, true, None, 600)
            },
            0,
        );
        assert_eq!(first.chain, Ok(None));
        let second = peers.ath11k_dp_rx_frag_h_mpdu(
            FragmentInput {
                vdev_id: 2,
                tid: 8,
                encryption: FragmentEncryption::Open,
                packet_number: None,
                bytes: vec![3, 4],
                ..fragment([0xbb; 6], 5, 0, true, None, 601)
            },
            0,
        );
        assert_eq!(second.chain, Ok(None));

        peers
            .ath11k_peer_rx_tid_cleanup(&mut reo, &mut rings, 2, [0xaa; 6])
            .unwrap();
        assert_eq!(rings.published.len(), 3);
        assert_eq!(peers.pending_delete.len(), 3);
        assert!(
            peers
                .fragments
                .iter()
                .all(|state| state.key.peer_addr == [0xbb; 6])
        );
        assert!(peers.is_active(2, [0xbb; 6], 8));
        assert_eq!(
            peers.ath11k_peer_rx_tid_setup(
                &mut reo,
                &mut rings,
                &mut wmi,
                2,
                [0xaa; 6],
                3,
                1,
                0,
                PacketNumberType::None,
            ),
            Err(DpError::WrongState)
        );
        let late = peers.ath11k_dp_rx_frag_h_mpdu(
            FragmentInput {
                vdev_id: 2,
                tid: 8,
                encryption: FragmentEncryption::Open,
                packet_number: None,
                bytes: vec![9],
                ..fragment([0xaa; 6], 6, 0, true, None, 602)
            },
            0,
        );
        assert_eq!(late.chain, Err(DpError::InvalidFrame));
        assert_eq!(late.return_links, [FragmentLinkDescriptor(602)]);
        peers
            .ath11k_peer_rx_tid_cleanup(&mut reo, &mut rings, 2, [0xaa; 6])
            .unwrap();
        assert_eq!(rings.published.len(), 3);
    }

    #[test]
    fn partial_cleanup_quarantines_failed_owner_until_reset() {
        let device = DeterministicBackend::device();
        let mut peers = PeerRxTids::new(device.clone()).unwrap();
        let mut reo = controller();
        let mut rings = ModelRings::default();
        let mut wmi = ModelWmi::default();
        peers
            .register_peer_after_firmware_create(3, [0xcc; 6])
            .unwrap();
        for tid in [0, 1, 2] {
            peers
                .ath11k_peer_rx_tid_setup(
                    &mut reo,
                    &mut rings,
                    &mut wmi,
                    3,
                    [0xcc; 6],
                    tid,
                    1,
                    0,
                    PacketNumberType::None,
                )
                .unwrap();
        }
        rings.fail_publish_attempt = Some(2);
        assert_eq!(
            peers.ath11k_peer_rx_tid_cleanup(&mut reo, &mut rings, 3, [0xcc; 6]),
            Err(DpError::NoResources)
        );
        assert_eq!(rings.published.len(), 2);
        assert_eq!(peers.pending_delete.len(), 2);
        assert_eq!(peers.failed_delete.len(), 1);
        assert_eq!(
            peers.ath11k_peer_rx_tid_setup(
                &mut reo,
                &mut rings,
                &mut wmi,
                3,
                [0xcc; 6],
                1,
                1,
                0,
                PacketNumberType::None,
            ),
            Err(DpError::WrongState)
        );

        device.reset().unwrap();
        peers.release_after_device_reset();
        let mut peers = PeerRxTids::new(device).unwrap();
        let mut reo = controller();
        let mut rings = ModelRings::default();
        peers
            .register_peer_after_firmware_create(3, [0xcc; 6])
            .unwrap();
        peers
            .ath11k_peer_rx_tid_setup(
                &mut reo,
                &mut rings,
                &mut wmi,
                3,
                [0xcc; 6],
                1,
                1,
                0,
                PacketNumberType::None,
            )
            .unwrap();
    }

    #[test]
    fn successful_teardown_retires_key_after_terminal_flush_status() {
        let mut peers = PeerRxTids::new(DeterministicBackend::device()).unwrap();
        let mut reo = controller();
        let mut rings = ModelRings::default();
        let mut wmi = ModelWmi::default();
        let key = [0xcd; 6];
        peers.register_peer_after_firmware_create(7, key).unwrap();
        peers
            .ath11k_peer_rx_tid_setup(
                &mut reo,
                &mut rings,
                &mut wmi,
                7,
                key,
                3,
                64,
                0,
                PacketNumberType::None,
            )
            .unwrap();
        peers
            .ath11k_peer_rx_tid_cleanup(&mut reo, &mut rings, 7, key)
            .unwrap();
        assert_eq!(
            peers.register_peer_after_firmware_create(7, key),
            Err(DpError::WrongState)
        );

        rings.status.push_back(status(1, 153, 0));
        peers
            .ath11k_dp_rx_tid_del_func(&mut reo, &mut rings, 0)
            .unwrap();
        peers.flush_aged(&mut reo, &mut rings, 1_001).unwrap();
        rings.status.push_back(status(5, 313, 0));
        peers
            .ath11k_dp_rx_tid_del_func(&mut reo, &mut rings, 1_001)
            .unwrap();
        peers.register_peer_after_firmware_create(7, key).unwrap();
    }

    #[test]
    fn ampdu_start_requires_peer_and_updates_negotiated_ba_and_ssn() {
        let mut peers = PeerRxTids::new(DeterministicBackend::device()).unwrap();
        let mut reo = controller();
        let mut rings = ModelRings::default();
        let mut wmi = ModelWmi::default();
        let addr = [0xd1; 6];
        assert_eq!(
            peers.ath11k_dp_rx_ampdu_start(
                &mut reo,
                &mut rings,
                &mut wmi,
                4,
                addr,
                5,
                64,
                0x123,
                PacketNumberType::Wpa,
            ),
            Err(DpError::WrongState)
        );
        assert!(rings.published.is_empty());
        assert!(wmi.commands.is_empty());

        peers.register_peer_after_firmware_create(4, addr).unwrap();
        assert_eq!(
            peers.ath11k_dp_rx_ampdu_start(
                &mut reo,
                &mut rings,
                &mut wmi,
                4,
                addr,
                16,
                64,
                0,
                PacketNumberType::None,
            ),
            Err(DpError::WrongState)
        );
        assert!(rings.published.is_empty());
        assert!(wmi.commands.is_empty());
        peers
            .ath11k_dp_rx_ampdu_start(
                &mut reo,
                &mut rings,
                &mut wmi,
                4,
                addr,
                5,
                64,
                0x123,
                PacketNumberType::Wpa,
            )
            .unwrap();
        assert_eq!(wmi.commands.len(), 1);
        assert!(rings.published.is_empty());

        peers
            .ath11k_dp_rx_ampdu_start(
                &mut reo,
                &mut rings,
                &mut wmi,
                4,
                addr,
                5,
                32,
                0x456,
                PacketNumberType::Wpa,
            )
            .unwrap();
        let bytes = rings.published[0].1.bytes();
        let update0 = u32::from_le_bytes(bytes[12..16].try_into().unwrap());
        let update2 = u32::from_le_bytes(bytes[20..24].try_into().unwrap());
        assert_ne!(update0 & UPDATE_BA_WINDOW_SIZE, 0);
        assert_ne!(update0 & UPDATE_START_SEQUENCE, 0);
        assert_eq!(update2 >> START_SEQUENCE_SHIFT & 0xfff, 0x456);
        assert_eq!(wmi.commands.len(), 2);
    }

    #[test]
    fn ampdu_stop_is_inactive_idempotent_and_preserves_state_on_failures() {
        let mut peers = PeerRxTids::new(DeterministicBackend::device()).unwrap();
        let mut reo = controller();
        let mut rings = ModelRings::default();
        let mut wmi = ModelWmi::default();
        let addr = [0xd2; 6];
        peers.register_peer_after_firmware_create(5, addr).unwrap();
        assert_eq!(
            peers.ath11k_dp_rx_ampdu_stop(&mut reo, &mut rings, &mut wmi, 5, addr, 16),
            Err(DpError::WrongState)
        );
        assert!(rings.published.is_empty());
        assert!(wmi.commands.is_empty());
        peers
            .ath11k_dp_rx_ampdu_stop(&mut reo, &mut rings, &mut wmi, 5, addr, 3)
            .unwrap();
        assert!(rings.published.is_empty());
        peers
            .ath11k_dp_rx_ampdu_start(
                &mut reo,
                &mut rings,
                &mut wmi,
                5,
                addr,
                3,
                64,
                1,
                PacketNumberType::None,
            )
            .unwrap();

        rings.fail_publish = true;
        assert_eq!(
            peers.ath11k_dp_rx_ampdu_stop(&mut reo, &mut rings, &mut wmi, 5, addr, 3),
            Err(DpError::NoResources)
        );
        assert_eq!(peers.tids[0].tid.ba_window_size, 64);
        assert_eq!(wmi.commands.len(), 1);

        rings.fail_publish = false;
        wmi.fail = true;
        assert_eq!(
            peers.ath11k_dp_rx_ampdu_stop(&mut reo, &mut rings, &mut wmi, 5, addr, 3),
            Err(DpError::DeviceFault)
        );
        assert_eq!(peers.tids[0].tid.ba_window_size, 1);
        assert_eq!(rings.published.len(), 1);
        let bytes = rings.published[0].1.bytes();
        let update0 = u32::from_le_bytes(bytes[12..16].try_into().unwrap());
        let update2 = u32::from_le_bytes(bytes[20..24].try_into().unwrap());
        assert_ne!(update0 & UPDATE_BA_WINDOW_SIZE, 0);
        assert_eq!(update0 & UPDATE_START_SEQUENCE, 0);
        assert_eq!(update2 >> START_SEQUENCE_SHIFT & 0xfff, 0);
    }

    #[test]
    fn ampdu_peer_gating_and_stop_are_key_isolated() {
        let mut peers = PeerRxTids::new(DeterministicBackend::device()).unwrap();
        let mut reo = controller();
        let mut rings = ModelRings::default();
        let mut wmi = ModelWmi::default();
        for addr in [[0xe1; 6], [0xe2; 6]] {
            peers.register_peer_after_firmware_create(6, addr).unwrap();
            peers
                .ath11k_dp_rx_ampdu_start(
                    &mut reo,
                    &mut rings,
                    &mut wmi,
                    6,
                    addr,
                    2,
                    64,
                    0,
                    PacketNumberType::None,
                )
                .unwrap();
        }
        peers
            .ath11k_dp_rx_ampdu_stop(&mut reo, &mut rings, &mut wmi, 6, [0xe1; 6], 2)
            .unwrap();
        assert_eq!(peers.tids[0].tid.ba_window_size, 1);
        assert_eq!(peers.tids[1].tid.ba_window_size, 64);

        peers
            .ath11k_peer_rx_tid_cleanup(&mut reo, &mut rings, 6, [0xe1; 6])
            .unwrap();
        assert_eq!(
            peers.ath11k_dp_rx_ampdu_stop(&mut reo, &mut rings, &mut wmi, 6, [0xe1; 6], 2,),
            Err(DpError::WrongState)
        );
        assert!(peers.is_active(6, [0xe2; 6], 2));
    }

    #[test]
    fn fragments_sort_complete_and_preserve_typed_link_dispositions() {
        let address = [0xf1; 6];
        let mut peers = PeerRxTids::new(DeterministicBackend::device()).unwrap();
        peers
            .register_peer_after_firmware_create(9, address)
            .unwrap();
        let last = peers.ath11k_dp_rx_frag_h_mpdu(fragment(address, 7, 2, false, Some(12), 102), 0);
        assert_eq!(last.chain, Ok(None));
        assert_eq!(last.return_links, [FragmentLinkDescriptor(102)]);
        let first = peers.ath11k_dp_rx_frag_h_mpdu(fragment(address, 7, 0, true, Some(10), 100), 1);
        assert_eq!(first.chain, Ok(None));
        assert!(first.return_links.is_empty());
        let mut middle_input = fragment(address, 7, 1, true, Some(11), 101);
        middle_input.decrypted = false;
        let middle = peers.ath11k_dp_rx_frag_h_mpdu(middle_input, 2);
        assert_eq!(middle.return_links, [FragmentLinkDescriptor(101)]);
        let chain = middle.chain.unwrap().unwrap();
        assert_eq!(chain.sequence, 7);
        assert_eq!(chain.first_link_descriptor, FragmentLinkDescriptor(100));
        assert_eq!(
            chain
                .fragments
                .iter()
                .map(|fragment| fragment.fragment_number)
                .collect::<Vec<_>>(),
            [0, 1, 2]
        );
        assert!(
            chain
                .fragments
                .iter()
                .all(|fragment| fragment.encryption == FragmentEncryption::Ccmp128)
        );
        assert_eq!(
            chain
                .fragments
                .iter()
                .map(|fragment| fragment.decrypted)
                .collect::<Vec<_>>(),
            [true, false, true]
        );
        assert!(chain.needs_crypto_normalization);
    }

    #[test]
    fn fragment_encryption_discriminants_match_pinned_hal() {
        assert_eq!(
            [
                FragmentEncryption::Wep40 as u8,
                FragmentEncryption::Wep104 as u8,
                FragmentEncryption::TkipNoMic as u8,
                FragmentEncryption::Wep128 as u8,
                FragmentEncryption::TkipMic as u8,
                FragmentEncryption::Wapi as u8,
                FragmentEncryption::Ccmp128 as u8,
                FragmentEncryption::Open as u8,
                FragmentEncryption::Ccmp256 as u8,
                FragmentEncryption::Gcmp128 as u8,
                FragmentEncryption::AesGcmp256 as u8,
                FragmentEncryption::WapiGcmSm4 as u8,
            ],
            [0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11]
        );
    }

    #[test]
    fn duplicate_sequence_switch_and_bad_pn_return_every_link_owner() {
        let address = [0xf2; 6];
        let mut peers = PeerRxTids::new(DeterministicBackend::device()).unwrap();
        peers
            .register_peer_after_firmware_create(9, address)
            .unwrap();
        let initial =
            peers.ath11k_dp_rx_frag_h_mpdu(fragment(address, 1, 0, true, Some(20), 200), 0);
        assert_eq!(initial.chain, Ok(None));
        assert!(initial.return_links.is_empty());
        let duplicate =
            peers.ath11k_dp_rx_frag_h_mpdu(fragment(address, 1, 0, true, Some(20), 201), 1);
        assert_eq!(duplicate.chain, Err(DpError::InvalidFrame));
        assert_eq!(duplicate.return_links, [FragmentLinkDescriptor(201)]);
        let switched =
            peers.ath11k_dp_rx_frag_h_mpdu(fragment(address, 2, 0, true, Some(30), 202), 2);
        assert_eq!(switched.return_links, [FragmentLinkDescriptor(200)]);
        let bad = peers.ath11k_dp_rx_frag_h_mpdu(fragment(address, 2, 1, false, Some(32), 203), 3);
        assert_eq!(bad.chain, Err(DpError::InvalidFrame));
        assert_eq!(
            bad.return_links,
            [FragmentLinkDescriptor(203), FragmentLinkDescriptor(202)]
        );
    }

    #[test]
    fn incomplete_fragment_timeout_returns_retained_first_link() {
        let address = [0xf3; 6];
        let mut peers = PeerRxTids::new(DeterministicBackend::device()).unwrap();
        peers
            .register_peer_after_firmware_create(9, address)
            .unwrap();
        let initial =
            peers.ath11k_dp_rx_frag_h_mpdu(fragment(address, 3, 0, true, Some(1), 300), 100);
        assert_eq!(initial.chain, Ok(None));
        assert!(initial.return_links.is_empty());
        assert!(peers.expire_incomplete_fragments(2_099).is_empty());
        assert_eq!(
            peers.expire_incomplete_fragments(2_100),
            [FragmentLinkDescriptor(300)]
        );
    }

    #[test]
    fn keyed_ingress_expires_due_chain_before_accumulating() {
        let address = [0xf6; 6];
        let mut peers = PeerRxTids::new(DeterministicBackend::device()).unwrap();
        peers
            .register_peer_after_firmware_create(9, address)
            .unwrap();
        let first = peers.ath11k_dp_rx_frag_h_mpdu(fragment(address, 8, 0, true, Some(10), 600), 0);
        assert_eq!(first.chain, Ok(None));

        let late = peers.ath11k_dp_rx_frag_h_mpdu(
            fragment(address, 8, 1, false, Some(11), 601),
            RX_FRAGMENT_TIMEOUT_MS + 1,
        );
        assert_eq!(late.chain, Ok(None));
        assert_eq!(
            late.return_links,
            [FragmentLinkDescriptor(600), FragmentLinkDescriptor(601)]
        );
    }

    #[test]
    fn fragment_validation_rejects_without_retaining_current_link() {
        let address = [0xf4; 6];
        let mut peers = PeerRxTids::new(DeterministicBackend::device()).unwrap();
        for invalid in [
            FragmentInput {
                multicast_broadcast: true,
                ..fragment(address, 4, 0, true, Some(1), 401)
            },
            FragmentInput {
                sequence_control_valid: false,
                ..fragment(address, 4, 0, true, Some(1), 401)
            },
            FragmentInput {
                frame_control_valid: false,
                ..fragment(address, 4, 0, true, Some(1), 401)
            },
            FragmentInput {
                fragment_number: 16,
                ..fragment(address, 4, 0, true, Some(1), 401)
            },
            FragmentInput {
                more_fragments: false,
                ..fragment(address, 4, 0, true, Some(1), 401)
            },
        ] {
            let outcome = peers.ath11k_dp_rx_frag_h_mpdu(invalid, 0);
            assert_eq!(outcome.chain, Err(DpError::InvalidFrame));
            assert_eq!(outcome.return_links, [FragmentLinkDescriptor(401)]);
        }
        assert!(peers.fragments.is_empty());
        peers
            .register_peer_after_firmware_create(9, address)
            .unwrap();
        peers.tearing_down.push(PeerKey {
            vdev_id: 9,
            peer_addr: address,
        });
        let outcome =
            peers.ath11k_dp_rx_frag_h_mpdu(fragment(address, 4, 0, true, Some(1), 400), 0);
        assert_eq!(outcome.chain, Err(DpError::InvalidFrame));
        assert_eq!(outcome.return_links, [FragmentLinkDescriptor(400)]);
    }

    #[test]
    fn teardown_queues_first_link_and_retires_only_after_transfer() {
        let address = [0xf5; 6];
        let mut peers = PeerRxTids::new(DeterministicBackend::device()).unwrap();
        peers
            .register_peer_after_firmware_create(9, address)
            .unwrap();
        let initial =
            peers.ath11k_dp_rx_frag_h_mpdu(fragment(address, 5, 0, true, Some(1), 500), 0);
        assert_eq!(initial.chain, Ok(None));
        assert!(initial.return_links.is_empty());
        let mut reo = controller();
        let mut rings = ModelRings::default();
        peers
            .ath11k_peer_rx_tid_cleanup(&mut reo, &mut rings, 9, address)
            .unwrap();
        assert_eq!(
            peers.register_peer_after_firmware_create(9, address),
            Err(DpError::WrongState)
        );
        assert_eq!(
            peers.take_pending_fragment_link_returns(9, address),
            [FragmentLinkDescriptor(500)]
        );
        peers
            .register_peer_after_firmware_create(9, address)
            .unwrap();
    }
}
