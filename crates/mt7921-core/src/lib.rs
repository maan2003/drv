#![no_std]

//! Hardware-independent pieces of the Linux mt76 Connac2 data formats.
//!
//! This is an exploratory port, not an MT7921/MT7922 device driver. The source
//! correspondence and the boundary deliberately left out are documented in
//! the crate README.

extern crate alloc;

use alloc::{boxed::Box, format, string::String, vec, vec::Vec};
use core::num::NonZeroU64;
use core::sync::atomic::{AtomicU64, Ordering};

pub use driver_runtime::{
    Completion as FirmwareCompletion, CompletionTracker as FirmwareCompletionTracker,
    IrqCapability as PciIrqCapability, IrqKind as PciIrqKind, IrqLifecycle, IrqLifecycleError,
    select_irq as select_vfio_irq,
};
pub use mt76_core::*;

/// A buffer segment representable by the MT7921 PCI DMA setup.
///
/// Linux selects a 32-bit DMA mask in `mt7921_pci_probe`, so this spike rejects
/// IOVAs and lengths which the descriptor would otherwise silently truncate.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DmaSegment {
    pub iova: u64,
    pub len: u16,
}

/// MT7921-compatible view of one Linux `struct mt76_desc`.
///
/// The local type preserves the original MT7921 API and applies the device's
/// 32-bit PCI DMA mask before delegating byte layout to `mt76-core`.
///
/// ```
/// use mt7921_core::{DmaDescriptor, DmaSegment};
///
/// let segment = DmaSegment { iova: 0x1020_3000, len: 64 };
/// let tx = DmaDescriptor::tx(segment, None, 0).unwrap();
/// let rx = DmaDescriptor::rx(segment).unwrap();
/// assert_eq!(tx.buf0, 0x1020_3000);
/// assert_eq!(rx.buf0, 0x1020_3000);
/// ```
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DmaDescriptor {
    pub buf0: u32,
    pub ctrl: u32,
    pub buf1: u32,
    pub info: u32,
}

impl DmaDescriptor {
    pub fn tx(
        first: DmaSegment,
        second: Option<DmaSegment>,
        info: u32,
    ) -> Result<Self, DescriptorError> {
        mt7921_dma_tx(first, second, info)
    }

    pub fn rx(buffer: DmaSegment) -> Result<Self, DescriptorError> {
        mt7921_dma_rx(buffer)
    }

    pub const fn reset() -> Self {
        Self::from_mt76(mt76_core::DmaDescriptor::reset())
    }

    pub const fn to_le_bytes(self) -> [u8; DMA_DESCRIPTOR_LEN] {
        self.into_mt76().to_le_bytes()
    }

    pub const fn is_dma_done(self) -> bool {
        self.into_mt76().is_dma_done()
    }

    const fn from_mt76(descriptor: mt76_core::DmaDescriptor) -> Self {
        Self {
            buf0: descriptor.buf0,
            ctrl: descriptor.ctrl,
            buf1: descriptor.buf1,
            info: descriptor.info,
        }
    }

    const fn into_mt76(self) -> mt76_core::DmaDescriptor {
        mt76_core::DmaDescriptor {
            buf0: self.buf0,
            ctrl: self.ctrl,
            buf1: self.buf1,
            info: self.info,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DescriptorError {
    IovaAbove32Bits,
    SegmentTooLong,
    InvalidArena,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RingAllocation {
    pub id: u64,
    pub iova: u64,
    pub len: usize,
}

pub trait Low32RingMemory {
    type Error;
    fn allocate_low32(&mut self, size: usize, align: usize) -> Result<RingAllocation, Self::Error>;
    fn free(&mut self, allocation: RingAllocation);
}

pub trait RingPublisher {
    type Error;
    fn write_descriptor(
        &mut self,
        index: u16,
        descriptor: DmaDescriptor,
    ) -> Result<(), Self::Error>;
    fn release_fence(&mut self);
    fn publish_producer(&mut self, index: u16) -> Result<(), Self::Error>;
    fn acquire_fence(&mut self);
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum RingError<E> {
    InvalidCount,
    Allocation(E),
    AllocationTooSmall,
    AllocationAbove32Bits,
    Full,
    Descriptor(DescriptorError),
    Publish(E),
}

/// Inactive MT7921 WFDMA TX ring model ported from pinned Linux mt76 `dma.c`.
///
/// Allocation is constrained to addresses representable by the MT7921 PCI
/// 32-bit DMA mask. Descriptors start CPU-owned (`DMA_DONE`), enqueue clears
/// that bit, and producer publication follows a release fence like
/// `mt76_dma_kick_queue`. Reclaim advances only from the consumer tail after a
/// device completion and acquire fence. This type has no MMIO enable method.
pub struct WfdmaRing {
    allocation: RingAllocation,
    descriptors: Vec<DmaDescriptor>,
    producer: u16,
    consumer: u16,
    queued: u16,
}

impl WfdmaRing {
    pub fn allocate<M: Low32RingMemory>(
        memory: &mut M,
        count: u16,
    ) -> Result<Self, RingError<M::Error>> {
        if count < 2 {
            return Err(RingError::InvalidCount);
        }
        let size = usize::from(count)
            .checked_mul(DMA_DESCRIPTOR_LEN)
            .ok_or(RingError::InvalidCount)?;
        let allocation = memory
            .allocate_low32(size, DMA_DESCRIPTOR_LEN)
            .map_err(RingError::Allocation)?;
        if allocation.len < size {
            memory.free(allocation);
            return Err(RingError::AllocationTooSmall);
        }
        let end = allocation
            .iova
            .checked_add(size as u64 - 1)
            .filter(|end| *end <= u64::from(u32::MAX));
        if allocation.iova % DMA_DESCRIPTOR_LEN as u64 != 0 || end.is_none() {
            memory.free(allocation);
            return Err(RingError::AllocationAbove32Bits);
        }
        Ok(Self {
            allocation,
            descriptors: vec![DmaDescriptor::reset(); usize::from(count)],
            producer: 0,
            consumer: 0,
            queued: 0,
        })
    }

    pub const fn allocation(&self) -> RingAllocation {
        self.allocation
    }
    pub const fn producer(&self) -> u16 {
        self.producer
    }
    pub const fn consumer(&self) -> u16 {
        self.consumer
    }
    pub const fn queued(&self) -> u16 {
        self.queued
    }

    pub fn enqueue<P: RingPublisher>(
        &mut self,
        publisher: &mut P,
        first: DmaSegment,
        second: Option<DmaSegment>,
        info: u32,
    ) -> Result<u16, RingError<P::Error>> {
        if usize::from(self.queued) == self.descriptors.len() {
            return Err(RingError::Full);
        }
        let index = self.producer;
        let descriptor = mt7921_dma_tx(first, second, info).map_err(RingError::Descriptor)?;
        publisher
            .write_descriptor(index, descriptor)
            .map_err(RingError::Publish)?;
        publisher.release_fence();
        let next = (usize::from(index) + 1) % self.descriptors.len();
        publisher
            .publish_producer(next as u16)
            .map_err(RingError::Publish)?;
        self.descriptors[usize::from(index)] = descriptor;
        self.producer = next as u16;
        self.queued += 1;
        Ok(index)
    }

    /// Model the device's DMA_DONE write for deterministic tests/backends.
    pub fn complete(&mut self, index: u16) -> bool {
        let Some(descriptor) = self.descriptors.get_mut(usize::from(index)) else {
            return false;
        };
        descriptor.ctrl |= 1 << 31;
        true
    }

    pub fn reclaim_one<P: RingPublisher>(&mut self, publisher: &mut P) -> Option<u16> {
        if self.queued == 0 {
            return None;
        }
        let index = self.consumer;
        if !self.descriptors[usize::from(index)].is_dma_done() {
            return None;
        }
        publisher.acquire_fence();
        self.descriptors[usize::from(index)] = DmaDescriptor::reset();
        self.consumer = ((usize::from(index) + 1) % self.descriptors.len()) as u16;
        self.queued -= 1;
        Some(index)
    }

    pub fn teardown<M: Low32RingMemory>(mut self, memory: &mut M) {
        self.descriptors.fill(DmaDescriptor::reset());
        self.queued = 0;
        memory.free(self.allocation);
    }
}

pub fn mt7921_dma_tx(
    first: DmaSegment,
    second: Option<DmaSegment>,
    info: u32,
) -> Result<DmaDescriptor, DescriptorError> {
    validate_mt7921_segment(first)?;
    if let Some(segment) = second {
        validate_mt7921_segment(segment)?;
    }
    mt76_core::DmaDescriptor::tx(
        (first.iova, first.len),
        second.map(|segment| (segment.iova, segment.len)),
        info,
    )
    .map(DmaDescriptor::from_mt76)
    .map_err(map_descriptor_error)
}
pub fn mt7921_dma_rx(buffer: DmaSegment) -> Result<DmaDescriptor, DescriptorError> {
    validate_mt7921_segment(buffer)?;
    mt76_core::DmaDescriptor::rx((buffer.iova, buffer.len))
        .map(DmaDescriptor::from_mt76)
        .map_err(map_descriptor_error)
}
fn map_descriptor_error(error: mt76_core::DescriptorError) -> DescriptorError {
    match error {
        mt76_core::DescriptorError::AddressAbove36Bits => DescriptorError::IovaAbove32Bits,
        mt76_core::DescriptorError::SegmentTooLong => DescriptorError::SegmentTooLong,
    }
}
fn validate_mt7921_segment(segment: DmaSegment) -> Result<(), DescriptorError> {
    if segment.iova > u64::from(u32::MAX) {
        return Err(DescriptorError::IovaAbove32Bits);
    }
    if segment.len > 0x3fff {
        return Err(DescriptorError::SegmentTooLong);
    }
    Ok(())
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct ClcDiscovery {
    pub segment_count: u16,
    pub selected_power_segments: u16,
    pub selected_power_rules: u16,
    pub channel_segments: u16,
    pub channel_rules: u16,
    pub unique_country_codes: u16,
    pub world_domain_available: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ClcDiscoveryError {
    TruncatedSegmentHeader,
    InvalidSegmentLength(u32),
    UnsupportedSegmentIndex(u8),
    TruncatedRule { segment: u16 },
    CountOverflow,
    MissingWorldRule,
    RuleTooLarge,
}

/// Inventory the local CLC region exactly up to (but not including) Linux's
/// mutating `SET_CLC` call. Power segment selection uses the EEPROM hardware
/// encapsulation bit; channel rules remain opaque firmware input.
pub fn discover_clc(
    firmware: Firmware<'_>,
    hardware: EepromHardwareInfo,
) -> Result<ClcDiscovery, ClcDiscoveryError> {
    let Some(region) = firmware.regions().find(FirmwareRegion::is_clc) else {
        return Ok(ClcDiscovery::default());
    };
    let mut discovery = ClcDiscovery::default();
    let mut countries = Vec::<[u8; 2]>::new();
    let mut accepted = [false; 2];
    let mut offset = 0usize;
    while offset < region.payload.len() {
        let header = region
            .payload
            .get(offset..offset + 16)
            .ok_or(ClcDiscoveryError::TruncatedSegmentHeader)?;
        let length = u32::from_le_bytes(header[0..4].try_into().expect("fixed field"));
        let length_usize = length as usize;
        if length_usize < 16
            || offset
                .checked_add(length_usize)
                .is_none_or(|end| end > region.payload.len())
        {
            return Err(ClcDiscoveryError::InvalidSegmentLength(length));
        }
        let index = header[4];
        if index > 1 {
            return Err(ClcDiscoveryError::UnsupportedSegmentIndex(index));
        }
        discovery.segment_count = discovery
            .segment_count
            .checked_add(1)
            .ok_or(ClcDiscoveryError::CountOverflow)?;
        let selected = !accepted[index as usize]
            && (index == 1 || ((header[7] & 1 != 0) == hardware.encapsulated_calibration));
        if selected {
            accepted[index as usize] = true;
        }
        if index == 0 && selected {
            discovery.selected_power_segments = discovery
                .selected_power_segments
                .checked_add(1)
                .ok_or(ClcDiscoveryError::CountOverflow)?;
        } else if index == 1 {
            discovery.channel_segments = discovery
                .channel_segments
                .checked_add(1)
                .ok_or(ClcDiscoveryError::CountOverflow)?;
        }
        let end = offset + length_usize;
        let mut rule_offset = offset + 16;
        // Pinned Linux stops when no more than 16 bytes remain in a segment.
        while end - rule_offset > 16 {
            let rule = region.payload.get(rule_offset..rule_offset + 6).ok_or(
                ClcDiscoveryError::TruncatedRule {
                    segment: discovery.segment_count - 1,
                },
            )?;
            let data_length = u16::from_le_bytes([rule[4], rule[5]]) as usize;
            let rule_length = 6usize
                .checked_add(data_length)
                .ok_or(ClcDiscoveryError::CountOverflow)?;
            if rule_offset
                .checked_add(rule_length)
                .is_none_or(|rule_end| rule_end > end)
            {
                return Err(ClcDiscoveryError::TruncatedRule {
                    segment: discovery.segment_count - 1,
                });
            }
            if selected {
                if index == 0 {
                    discovery.selected_power_rules = discovery
                        .selected_power_rules
                        .checked_add(1)
                        .ok_or(ClcDiscoveryError::CountOverflow)?;
                } else {
                    discovery.channel_rules = discovery
                        .channel_rules
                        .checked_add(1)
                        .ok_or(ClcDiscoveryError::CountOverflow)?;
                }
                let alpha2 = [rule[0], rule[1]];
                if alpha2 == *b"00" {
                    discovery.world_domain_available = true;
                }
                if !countries.contains(&alpha2) {
                    countries.push(alpha2);
                }
            }
            rule_offset += rule_length;
        }
        offset = end;
    }
    discovery.unique_country_codes =
        u16::try_from(countries.len()).map_err(|_| ClcDiscoveryError::CountOverflow)?;
    Ok(discovery)
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ClcSetCommand {
    pub index: u8,
    pub environment: u8,
    pub acpi_configuration: u8,
    pub capability: u8,
    pub alpha2: [u8; 2],
    pub rule_type: [u8; 2],
    pub environment_6ghz: u8,
    pub mtcl_configuration: u8,
    pub data: Vec<u8>,
}

impl ClcSetCommand {
    /// Linux passes this exact predicate as `wait_resp` to
    /// `mt76_mcu_skb_send_and_get_msg` for every selected rule.
    pub const fn expects_response(&self) -> bool {
        self.capability & 1 != 0
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ClcSetResponse {
    pub tag: u16,
    pub length: u16,
    pub special_unii_mask: u8,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ClcSetResponseError {
    Truncated,
    InvalidLength(u16),
    InvalidMask(u8),
}

/// Select every opaque `"00"` rule from Linux's accepted CLC records. This
/// intentionally supplies no ACPI/MTCL overrides and never interprets data.
pub fn world_clc_commands(
    firmware: Firmware<'_>,
    hardware: EepromHardwareInfo,
    chip_capability: u64,
    acpi_configuration: u8,
) -> Result<Vec<ClcSetCommand>, ClcDiscoveryError> {
    if acpi_configuration > 1 {
        return Err(ClcDiscoveryError::RuleTooLarge);
    }
    let Some(region) = firmware.regions().find(FirmwareRegion::is_clc) else {
        return Err(ClcDiscoveryError::MissingWorldRule);
    };
    let mut commands = Vec::new();
    let mut accepted = [false; 2];
    let mut offset = 0usize;
    while offset < region.payload.len() {
        let header = region
            .payload
            .get(offset..offset + 16)
            .ok_or(ClcDiscoveryError::TruncatedSegmentHeader)?;
        let length = u32::from_le_bytes(header[0..4].try_into().expect("fixed field"));
        let length_usize = length as usize;
        if length_usize < 16
            || offset
                .checked_add(length_usize)
                .is_none_or(|end| end > region.payload.len())
        {
            return Err(ClcDiscoveryError::InvalidSegmentLength(length));
        }
        let index = header[4];
        if index > 1 {
            return Err(ClcDiscoveryError::UnsupportedSegmentIndex(index));
        }
        let selected = !accepted[index as usize]
            && (index == 1 || ((header[7] & 1 != 0) == hardware.encapsulated_calibration));
        if selected {
            accepted[index as usize] = true;
        }
        let end = offset + length_usize;
        let mut rule_offset = offset + 16;
        while end - rule_offset > 16 {
            let rule = region.payload.get(rule_offset..rule_offset + 6).ok_or(
                ClcDiscoveryError::TruncatedRule {
                    segment: index as u16,
                },
            )?;
            let data_length = u16::from_le_bytes([rule[4], rule[5]]) as usize;
            let rule_end = rule_offset
                .checked_add(6)
                .and_then(|start| start.checked_add(data_length))
                .filter(|rule_end| *rule_end <= end)
                .ok_or(ClcDiscoveryError::TruncatedRule {
                    segment: index as u16,
                })?;
            if selected && &rule[..2] == b"00" {
                commands.push(ClcSetCommand {
                    index,
                    environment: 1,
                    acpi_configuration,
                    capability: u8::from(chip_capability & 1 != 0),
                    alpha2: *b"00",
                    rule_type: [rule[2], rule[3]],
                    environment_6ghz: 0,
                    // mt792x_acpi_get_mtcl_conf returns u32::MAX when no
                    // ACPI SAR country table exists; assignment to the
                    // packed u8 request field retains 0xff.
                    mtcl_configuration: 0xff,
                    data: region.payload[rule_offset + 6..rule_end].to_vec(),
                });
            }
            rule_offset = rule_end;
        }
        offset = end;
    }
    if commands.is_empty() {
        Err(ClcDiscoveryError::MissingWorldRule)
    } else {
        Ok(commands)
    }
}

pub fn parse_clc_set_response(bytes: &[u8]) -> Result<ClcSetResponse, ClcSetResponseError> {
    let response = bytes.get(4..72).ok_or(ClcSetResponseError::Truncated)?;
    let length = u16::from_le_bytes([response[2], response[3]]);
    if length != 68 {
        return Err(ClcSetResponseError::InvalidLength(length));
    }
    let special_unii_mask = response[4];
    if special_unii_mask & !0x1f != 0 {
        return Err(ClcSetResponseError::InvalidMask(special_unii_mask));
    }
    Ok(ClcSetResponse {
        tag: u16::from_le_bytes([response[0], response[1]]),
        length,
        special_unii_mask,
    })
}

/// A bounds-checked view of the Connac2 RAM firmware layout consumed by
/// `mt76_connac_mcu_send_ram_firmware` and `mt7921_load_clc`.

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ReadRegister {
    McuCommand,
    HostInterruptStatus,
    WfdmaGlobalConfig,
    ConnOnLowPowerControl,
    ConnOnMisc,
}

impl ReadRegister {
    pub const ALL: [Self; 5] = [
        Self::McuCommand,
        Self::HostInterruptStatus,
        Self::WfdmaGlobalConfig,
        Self::ConnOnLowPowerControl,
        Self::ConnOnMisc,
    ];

    pub const fn bar_offset(self) -> usize {
        match self {
            // MT_WFDMA0_BASE (0xd4000) plus register offset.
            Self::McuCommand => 0xd41f0,
            Self::HostInterruptStatus => 0xd4200,
            Self::WfdmaGlobalConfig => 0xd4208,
            // Linux fixed-map 0x7c060000 -> BAR 0xe0000.
            Self::ConnOnLowPowerControl => 0xe0010,
            Self::ConnOnMisc => 0xe00f0,
        }
    }

    pub const fn name(self) -> &'static str {
        match self {
            Self::McuCommand => "mcu_command",
            Self::HostInterruptStatus => "host_interrupt_status",
            Self::WfdmaGlobalConfig => "wfdma_global_config",
            Self::ConnOnLowPowerControl => "conn_on_low_power_control",
            Self::ConnOnMisc => "conn_on_misc",
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ReadOnlyStatus {
    pub firmware_powered: bool,
    pub firmware_n9_ready: bool,
    pub firmware_owns_device: bool,
    pub tx_dma_enabled: bool,
    pub tx_dma_busy: bool,
    pub rx_dma_enabled: bool,
    pub rx_dma_busy: bool,
}

// Pinned Linux mt792x_regs.h names these bits PCIE_LPCR_HOST_{SET,CLR}_OWN and
// PCIE_LPCR_HOST_OWN_SYNC. __mt792xe_mcu_drv_pmctrl writes CLR_OWN and polls
// OWN_SYNC clear up to ten times, with a 50 ms poll per attempt at 1 ms ticks.
pub const PCIE_LPCR_HOST_SET_OWN: u32 = 1 << 0;
pub const PCIE_LPCR_HOST_CLR_OWN: u32 = 1 << 1;
pub const PCIE_LPCR_HOST_OWN_SYNC: u32 = 1 << 2;
pub const DRIVER_OWN_ATTEMPTS: u8 = 10;
pub const DRIVER_OWN_ATTEMPT_MS: u64 = 50;
pub const DRIVER_OWN_POLL_MS: u64 = 1;
pub const DRIVER_OWN_HARD_DEADLINE_MS: u64 = DRIVER_OWN_ATTEMPTS as u64 * DRIVER_OWN_ATTEMPT_MS;
pub const DRIVER_OWN_ASPM_DELAY_MIN_US: u64 = 2_000;
pub const DRIVER_OWN_ASPM_DELAY_MAX_US: u64 = 3_000;
pub const DRIVER_OWN_ASPM_HARD_DEADLINE_MS: u64 = DRIVER_OWN_HARD_DEADLINE_MS
    + DRIVER_OWN_ATTEMPTS as u64 * DRIVER_OWN_ASPM_DELAY_MAX_US.div_ceil(1_000);

pub trait OwnershipTransport {
    type Error;

    fn now_ms(&self) -> u64;
    fn write_clear_own(&mut self) -> Result<(), Self::Error>;
    fn read_low_power_control(&mut self) -> Result<u32, Self::Error>;
    fn sleep_ms(&mut self, milliseconds: u64);
    fn sleep_us_range(&mut self, _minimum: u64, maximum: u64) {
        self.sleep_ms(maximum.div_ceil(1_000));
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum OwnershipEvent {
    ClearOwnBefore {
        attempt: u8,
        at_ms: u64,
    },
    ClearOwnWritten {
        attempt: u8,
        at_ms: u64,
    },
    AspmDelay {
        attempt: u8,
        at_ms: u64,
        minimum_us: u64,
        maximum_us: u64,
    },
    StatusRead {
        attempt: u8,
        at_ms: u64,
        raw: u32,
    },
    StatusReadBefore {
        attempt: u8,
        at_ms: u64,
    },
    AttemptExpired {
        attempt: u8,
        at_ms: u64,
    },
    Acquired {
        attempt: u8,
        at_ms: u64,
    },
    UnexpectedState {
        attempt: u8,
        at_ms: u64,
        raw: u32,
    },
    TimedOut {
        at_ms: u64,
    },
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum OwnershipError<E> {
    Transport(E),
    ClockOverflow,
    UnexpectedState(u32),
    Timeout,
}

/// Acquire PCIe driver ownership using the exact bounded Linux retry shape.
///
/// The transport contract exposes only the one CLR_OWN write, the matching
/// status read, a monotonic clock, and bounded sleep. The event sink makes each
/// state transition durable at the host adapter without coupling this logic to
/// a logger or component runtime.
pub fn acquire_driver_ownership<T, F>(
    transport: &mut T,
    event: F,
) -> Result<(), OwnershipError<T::Error>>
where
    T: OwnershipTransport,
    F: FnMut(OwnershipEvent),
{
    acquire_driver_ownership_with_aspm(transport, false, event)
}

pub fn acquire_driver_ownership_with_aspm<T, F>(
    transport: &mut T,
    aspm_supported: bool,
    mut event: F,
) -> Result<(), OwnershipError<T::Error>>
where
    T: OwnershipTransport,
    F: FnMut(OwnershipEvent),
{
    let start = transport.now_ms();
    for attempt in 1..=DRIVER_OWN_ATTEMPTS {
        let now = transport.now_ms();
        event(OwnershipEvent::ClearOwnBefore {
            attempt,
            at_ms: now.saturating_sub(start),
        });
        let clear_result = transport.write_clear_own();
        if clear_result.is_ok() {
            event(OwnershipEvent::ClearOwnWritten {
                attempt,
                at_ms: transport.now_ms().saturating_sub(start),
            });
        }
        if aspm_supported {
            transport.sleep_us_range(DRIVER_OWN_ASPM_DELAY_MIN_US, DRIVER_OWN_ASPM_DELAY_MAX_US);
            event(OwnershipEvent::AspmDelay {
                attempt,
                at_ms: transport.now_ms().saturating_sub(start),
                minimum_us: DRIVER_OWN_ASPM_DELAY_MIN_US,
                maximum_us: DRIVER_OWN_ASPM_DELAY_MAX_US,
            });
        }
        clear_result.map_err(OwnershipError::Transport)?;
        let attempt_deadline = transport.now_ms().saturating_add(DRIVER_OWN_ATTEMPT_MS);
        loop {
            event(OwnershipEvent::StatusReadBefore {
                attempt,
                at_ms: transport.now_ms().saturating_sub(start),
            });
            let raw = transport
                .read_low_power_control()
                .map_err(OwnershipError::Transport)?;
            let now = transport.now_ms();
            event(OwnershipEvent::StatusRead {
                attempt,
                at_ms: now.saturating_sub(start),
                raw,
            });
            if raw & PCIE_LPCR_HOST_OWN_SYNC == 0 {
                event(OwnershipEvent::Acquired {
                    attempt,
                    at_ms: now.saturating_sub(start),
                });
                return Ok(());
            }
            if now >= attempt_deadline {
                event(OwnershipEvent::AttemptExpired {
                    attempt,
                    at_ms: now.saturating_sub(start),
                });
                break;
            }
            transport.sleep_ms(DRIVER_OWN_POLL_MS.min(attempt_deadline - now));
        }
    }
    let at_ms = transport.now_ms().saturating_sub(start);
    event(OwnershipEvent::TimedOut { at_ms });
    Err(OwnershipError::Timeout)
}

pub trait OwnershipRoundTripTransport: OwnershipTransport {
    fn write_set_own(&mut self) -> Result<(), Self::Error>;
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum OwnershipState {
    DriverOwned,
    FirmwareOwned,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FirmwareOwnershipEvent {
    SetOwnBefore { attempt: u8, at_ms: u64 },
    SetOwnWritten { attempt: u8, at_ms: u64 },
    StatusReadBefore { attempt: u8, at_ms: u64 },
    StatusRead { attempt: u8, at_ms: u64, raw: u32 },
    AttemptExpired { attempt: u8, at_ms: u64 },
    Restored { attempt: u8, at_ms: u64 },
    UnexpectedState { attempt: u8, at_ms: u64, raw: u32 },
    TimedOut { at_ms: u64 },
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum FirmwareOwnershipError<E> {
    Transport(E),
    TransportAndTimeout(E),
    ClockOverflow,
    UnexpectedState(u32),
    Timeout,
}

fn restore_firmware_ownership<T, F>(
    transport: &mut T,
    mut event: F,
) -> Result<(), FirmwareOwnershipError<T::Error>>
where
    T: OwnershipRoundTripTransport,
    F: FnMut(FirmwareOwnershipEvent),
{
    let start = transport.now_ms();
    let mut first_transport_error = None;
    for attempt in 1..=DRIVER_OWN_ATTEMPTS {
        let now = transport.now_ms();
        event(FirmwareOwnershipEvent::SetOwnBefore {
            attempt,
            at_ms: now.saturating_sub(start),
        });
        match transport.write_set_own() {
            Ok(()) => event(FirmwareOwnershipEvent::SetOwnWritten {
                attempt,
                at_ms: transport.now_ms().saturating_sub(start),
            }),
            Err(error) => {
                if first_transport_error.is_none() {
                    first_transport_error = Some(error);
                }
            }
        }
        let attempt_deadline = transport.now_ms().saturating_add(DRIVER_OWN_ATTEMPT_MS);
        loop {
            event(FirmwareOwnershipEvent::StatusReadBefore {
                attempt,
                at_ms: transport.now_ms().saturating_sub(start),
            });
            let raw = match transport.read_low_power_control() {
                Ok(raw) => raw,
                Err(error) => {
                    if first_transport_error.is_none() {
                        first_transport_error = Some(error);
                    }
                    let now = transport.now_ms();
                    if now >= attempt_deadline {
                        event(FirmwareOwnershipEvent::AttemptExpired {
                            attempt,
                            at_ms: now.saturating_sub(start),
                        });
                        break;
                    }
                    transport.sleep_ms(DRIVER_OWN_POLL_MS.min(attempt_deadline - now));
                    continue;
                }
            };
            let now = transport.now_ms();
            event(FirmwareOwnershipEvent::StatusRead {
                attempt,
                at_ms: now.saturating_sub(start),
                raw,
            });
            if raw & PCIE_LPCR_HOST_OWN_SYNC != 0 {
                event(FirmwareOwnershipEvent::Restored {
                    attempt,
                    at_ms: now.saturating_sub(start),
                });
                return match first_transport_error {
                    Some(error) => Err(FirmwareOwnershipError::Transport(error)),
                    None => Ok(()),
                };
            }
            if now >= attempt_deadline {
                event(FirmwareOwnershipEvent::AttemptExpired {
                    attempt,
                    at_ms: now.saturating_sub(start),
                });
                break;
            }
            transport.sleep_ms(DRIVER_OWN_POLL_MS.min(attempt_deadline - now));
        }
    }
    let at_ms = transport.now_ms().saturating_sub(start);
    event(FirmwareOwnershipEvent::TimedOut { at_ms });
    match first_transport_error {
        Some(error) => Err(FirmwareOwnershipError::TransportAndTimeout(error)),
        None => Err(FirmwareOwnershipError::Timeout),
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum OwnershipRoundTripEvent {
    SnapshotReadBefore,
    Snapshot { raw: u32, state: OwnershipState },
    Driver(OwnershipEvent),
    Firmware(FirmwareOwnershipEvent),
    Complete { restored: OwnershipState },
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum OwnershipRoundTripError<E> {
    SnapshotTransport(E),
    SnapshotAllOnes,
    SnapshotCommandBits(u32),
    Acquire(OwnershipError<E>),
    Restore(FirmwareOwnershipError<E>),
    AcquireAndRestore {
        acquire: OwnershipError<E>,
        restore: FirmwareOwnershipError<E>,
    },
}

/// Round-trip the conn-on ownership command while restoring the semantic
/// initial state. Once CLR is issued from firmware-owned state, SET rollback
/// is attempted regardless of the acquisition result.
pub fn round_trip_driver_ownership<T, F>(
    transport: &mut T,
    aspm_supported: bool,
    mut event: F,
) -> Result<OwnershipState, OwnershipRoundTripError<T::Error>>
where
    T: OwnershipRoundTripTransport,
    F: FnMut(OwnershipRoundTripEvent),
{
    event(OwnershipRoundTripEvent::SnapshotReadBefore);
    let raw = transport
        .read_low_power_control()
        .map_err(OwnershipRoundTripError::SnapshotTransport)?;
    if raw == u32::MAX {
        return Err(OwnershipRoundTripError::SnapshotAllOnes);
    }
    if raw & (PCIE_LPCR_HOST_SET_OWN | PCIE_LPCR_HOST_CLR_OWN) != 0 {
        return Err(OwnershipRoundTripError::SnapshotCommandBits(raw));
    }
    let initial = if raw & PCIE_LPCR_HOST_OWN_SYNC == 0 {
        OwnershipState::DriverOwned
    } else {
        OwnershipState::FirmwareOwned
    };
    event(OwnershipRoundTripEvent::Snapshot {
        raw,
        state: initial,
    });
    let acquire = acquire_driver_ownership_with_aspm(transport, aspm_supported, |item| {
        event(OwnershipRoundTripEvent::Driver(item))
    });
    if initial == OwnershipState::DriverOwned {
        acquire.map_err(OwnershipRoundTripError::Acquire)?;
        event(OwnershipRoundTripEvent::Complete { restored: initial });
        return Ok(initial);
    }
    let restore = restore_firmware_ownership(transport, |item| {
        event(OwnershipRoundTripEvent::Firmware(item))
    });
    match (acquire, restore) {
        (Ok(()), Ok(())) => {
            event(OwnershipRoundTripEvent::Complete { restored: initial });
            Ok(initial)
        }
        (Err(acquire), Ok(())) => Err(OwnershipRoundTripError::Acquire(acquire)),
        (Ok(()), Err(restore)) => Err(OwnershipRoundTripError::Restore(restore)),
        (Err(acquire), Err(restore)) => {
            Err(OwnershipRoundTripError::AcquireAndRestore { acquire, restore })
        }
    }
}

pub const MT_HIF_REMAP_L1_BAR_OFFSET: usize = 0xfe24c;
pub const MT_HIF_REMAP_WINDOW_BAR_OFFSET: usize = 0x40000;
const MT_HIF_REMAP_L1_MASK: u32 = 0xffff;

pub trait DynamicL1Transport {
    type Error;
    fn read_selector(&mut self) -> Result<u32, Self::Error>;
    fn write_selector(&mut self, value: u32) -> Result<(), Self::Error>;
    fn read_window(&mut self, offset: u16) -> Result<u32, Self::Error>;
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DynamicL1Event {
    SelectorSaved {
        raw: u32,
    },
    SelectorWritten {
        base: u16,
        raw: u32,
    },
    SelectorVerified {
        base: u16,
        raw: u32,
    },
    RegisterRead {
        name: &'static str,
        physical: u32,
        value: u32,
    },
    SelectorRestored {
        raw: u32,
    },
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum DynamicL1Error<E> {
    Transport(E),
    SelectorMismatch { expected_base: u16, raw: u32 },
    Restore(E),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DynamicIdentityStatus {
    pub chip_id: u32,
    pub revision: u32,
    pub hardware_bound: u32,
    pub top_low_power_control: u32,
}

/// Execute the first bounded dynamic-L1 transaction used by pinned mt7921.
///
/// The physical targets are closed over here rather than accepted from the
/// caller. The selector is restored on both success and read/select failure.
pub fn read_dynamic_identity_status<T, F>(
    transport: &mut T,
    mut event: F,
) -> Result<DynamicIdentityStatus, DynamicL1Error<T::Error>>
where
    T: DynamicL1Transport,
    F: FnMut(DynamicL1Event),
{
    let saved = transport
        .read_selector()
        .map_err(DynamicL1Error::Transport)?;
    event(DynamicL1Event::SelectorSaved { raw: saved });
    let operation = (|| {
        select_l1(transport, saved, 0x7001, &mut event)?;
        let chip_id = read_l1(transport, "chip_id", 0x7001_0200, &mut event)?;
        let hardware_bound = read_l1(transport, "hardware_bound", 0x7001_0020, &mut event)?;
        let revision = read_l1(transport, "revision", 0x7001_0204, &mut event)?;
        select_l1(transport, saved, 0x1806, &mut event)?;
        let top_low_power_control =
            read_l1(transport, "top_low_power_control", 0x1806_0010, &mut event)?;
        Ok(DynamicIdentityStatus {
            chip_id,
            revision,
            hardware_bound,
            top_low_power_control,
        })
    })();
    if let Err(error) = transport.write_selector(saved) {
        return Err(DynamicL1Error::Restore(error));
    }
    event(DynamicL1Event::SelectorRestored { raw: saved });
    operation
}

fn select_l1<T, F>(
    transport: &mut T,
    saved: u32,
    base: u16,
    event: &mut F,
) -> Result<(), DynamicL1Error<T::Error>>
where
    T: DynamicL1Transport,
    F: FnMut(DynamicL1Event),
{
    let selected = (saved & !MT_HIF_REMAP_L1_MASK) | u32::from(base);
    transport
        .write_selector(selected)
        .map_err(DynamicL1Error::Transport)?;
    event(DynamicL1Event::SelectorWritten {
        base,
        raw: selected,
    });
    // Linux reads MT_HIF_REMAP_L1 to push the selector write.
    let verified = transport
        .read_selector()
        .map_err(DynamicL1Error::Transport)?;
    event(DynamicL1Event::SelectorVerified {
        base,
        raw: verified,
    });
    if verified & MT_HIF_REMAP_L1_MASK != u32::from(base) {
        return Err(DynamicL1Error::SelectorMismatch {
            expected_base: base,
            raw: verified,
        });
    }
    Ok(())
}

fn read_l1<T, F>(
    transport: &mut T,
    name: &'static str,
    physical: u32,
    event: &mut F,
) -> Result<u32, DynamicL1Error<T::Error>>
where
    T: DynamicL1Transport,
    F: FnMut(DynamicL1Event),
{
    let value = transport
        .read_window(physical as u16)
        .map_err(DynamicL1Error::Transport)?;
    event(DynamicL1Event::RegisterRead {
        name,
        physical,
        value,
    });
    Ok(value)
}

pub const MT_TOP_LPCR_HOST_FW_OWN: u32 = 1 << 0;
pub const MT_TOP_LPCR_HOST_DRV_OWN: u32 = 1 << 1;
pub const TOP_DRIVER_OWN_DEADLINE_MS: u64 = 500;

pub trait TopOwnershipTransport {
    type Error;
    fn now_ms(&self) -> u64;
    fn read_selector(&mut self) -> Result<u32, Self::Error>;
    fn write_selector(&mut self, value: u32) -> Result<(), Self::Error>;
    fn write_top_driver_own(&mut self) -> Result<(), Self::Error>;
    fn read_top_low_power_control(&mut self) -> Result<u32, Self::Error>;
    fn sleep_ms(&mut self, milliseconds: u64);
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TopOwnershipEvent {
    SelectorSaved { raw: u32 },
    SelectorWritten { raw: u32 },
    SelectorVerified { raw: u32 },
    DriverOwnWritten { at_ms: u64 },
    StatusRead { at_ms: u64, raw: u32 },
    Acquired { at_ms: u64 },
    UnexpectedState { at_ms: u64, raw: u32 },
    TimedOut { at_ms: u64 },
    SelectorRestored { raw: u32 },
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum TopOwnershipError<E> {
    Transport(E),
    ClockOverflow,
    SelectorMismatch(u32),
    UnexpectedState(u32),
    Timeout,
    Restore(E),
}

/// Port pinned Linux `mt7921e_driver_own` with mandatory remap restoration.
pub fn acquire_top_driver_ownership<T, F>(
    transport: &mut T,
    mut event: F,
) -> Result<(), TopOwnershipError<T::Error>>
where
    T: TopOwnershipTransport,
    F: FnMut(TopOwnershipEvent),
{
    let saved = transport
        .read_selector()
        .map_err(TopOwnershipError::Transport)?;
    event(TopOwnershipEvent::SelectorSaved { raw: saved });
    let start = transport.now_ms();
    let deadline = start
        .checked_add(TOP_DRIVER_OWN_DEADLINE_MS)
        .ok_or(TopOwnershipError::ClockOverflow)?;
    let operation = (|| {
        let selected = (saved & !MT_HIF_REMAP_L1_MASK) | 0x1806;
        transport
            .write_selector(selected)
            .map_err(TopOwnershipError::Transport)?;
        event(TopOwnershipEvent::SelectorWritten { raw: selected });
        let verified = transport
            .read_selector()
            .map_err(TopOwnershipError::Transport)?;
        event(TopOwnershipEvent::SelectorVerified { raw: verified });
        if verified & MT_HIF_REMAP_L1_MASK != 0x1806 {
            return Err(TopOwnershipError::SelectorMismatch(verified));
        }
        transport
            .write_top_driver_own()
            .map_err(TopOwnershipError::Transport)?;
        event(TopOwnershipEvent::DriverOwnWritten {
            at_ms: transport.now_ms().saturating_sub(start),
        });
        loop {
            let raw = transport
                .read_top_low_power_control()
                .map_err(TopOwnershipError::Transport)?;
            let now = transport.now_ms();
            event(TopOwnershipEvent::StatusRead {
                at_ms: now.saturating_sub(start),
                raw,
            });
            if raw & MT_TOP_LPCR_HOST_DRV_OWN != 0 {
                event(TopOwnershipEvent::UnexpectedState {
                    at_ms: now.saturating_sub(start),
                    raw,
                });
                return Err(TopOwnershipError::UnexpectedState(raw));
            }
            if raw & MT_TOP_LPCR_HOST_FW_OWN == 0 {
                event(TopOwnershipEvent::Acquired {
                    at_ms: now.saturating_sub(start),
                });
                return Ok(());
            }
            if now >= deadline {
                event(TopOwnershipEvent::TimedOut {
                    at_ms: now.saturating_sub(start),
                });
                return Err(TopOwnershipError::Timeout);
            }
            transport.sleep_ms(DRIVER_OWN_POLL_MS.min(deadline - now));
        }
    })();
    if let Err(error) = transport.write_selector(saved) {
        return Err(TopOwnershipError::Restore(error));
    }
    event(TopOwnershipEvent::SelectorRestored { raw: saved });
    operation
}

pub const MT7921_FWDL_RING_COUNT: u32 = 128;
pub const MT7921_FWDL_RING_BYTES: usize = MT7921_FWDL_RING_COUNT as usize * DMA_DESCRIPTOR_LEN;
pub const MT7921_INT_TX_DONE_FWDL: u32 = 1 << 26;
pub const MT7921_FWDL_CHUNK_BYTES: usize = 4096;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DisabledFwdlRegister {
    HostInterruptEnable,
    WfdmaGlobalConfig,
    DescriptorBase,
    DescriptorCount,
    CpuIndex,
    DmaIndex,
}
impl DisabledFwdlRegister {
    pub const fn bar_offset(self) -> usize {
        match self {
            Self::HostInterruptEnable => 0xd4204,
            Self::WfdmaGlobalConfig => 0xd4208,
            // MT_TX_RING_BASE 0xd4300 + MT7921_TXQ_FWDL(16) * 0x10.
            Self::DescriptorBase => 0xd4400,
            Self::DescriptorCount => 0xd4404,
            Self::CpuIndex => 0xd4408,
            Self::DmaIndex => 0xd440c,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DisabledFwdlWrite {
    DescriptorBase,
    DescriptorCount,
    CpuIndex,
}

pub trait DisabledFwdlRingTransport {
    type Error;
    fn read(&mut self, register: DisabledFwdlRegister) -> Result<u32, Self::Error>;
    fn write(&mut self, register: DisabledFwdlWrite, value: u32) -> Result<(), Self::Error>;
    fn release_fence(&mut self);
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct FwdlRingRegisters {
    pub descriptor_base: u32,
    pub descriptor_count: u32,
    pub cpu_index: u32,
    pub dma_index: u32,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DisabledFwdlEvent {
    DisabledVerified {
        global_config: u32,
        interrupt_enable: u32,
    },
    Snapshot(FwdlRingRegisters),
    DescriptorFence,
    RegisterWritten {
        register: DisabledFwdlWrite,
        value: u32,
    },
    Programmed(FwdlRingRegisters),
    Restored(FwdlRingRegisters),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum DisabledFwdlError<E> {
    InvalidArena,
    DmaOrInterruptActive {
        global_config: u32,
        interrupt_enable: u32,
    },
    Transport(E),
    Readback {
        expected: FwdlRingRegisters,
        actual: FwdlRingRegisters,
    },
    Restore(E),
}

/// Temporarily program the inactive MT7921 firmware-download TX ring.
///
/// The transaction refuses to run unless TX/RX DMA-enable bits and every host
/// interrupt-enable bit are clear. Descriptor initialization must precede the
/// release fence supplied here. Only base/count/CPU index are written; the
/// device DMA index, WFDMA configuration, and interrupt registers are never
/// written. Original values are restored before return.
pub fn program_disabled_fwdl_ring<T, F>(
    transport: &mut T,
    arena_iova: u64,
    mut event: F,
) -> Result<FwdlRingRegisters, DisabledFwdlError<T::Error>>
where
    T: DisabledFwdlRingTransport,
    F: FnMut(DisabledFwdlEvent),
{
    if !arena_iova.is_multiple_of(4096)
        || arena_iova
            .checked_add(MT7921_FWDL_RING_BYTES as u64 - 1)
            .is_none_or(|end| end > u64::from(u32::MAX))
    {
        return Err(DisabledFwdlError::InvalidArena);
    }
    let interrupt_enable = transport
        .read(DisabledFwdlRegister::HostInterruptEnable)
        .map_err(DisabledFwdlError::Transport)?;
    let global_config = transport
        .read(DisabledFwdlRegister::WfdmaGlobalConfig)
        .map_err(DisabledFwdlError::Transport)?;
    if interrupt_enable != 0 || global_config & 0x5 != 0 {
        return Err(DisabledFwdlError::DmaOrInterruptActive {
            global_config,
            interrupt_enable,
        });
    }
    event(DisabledFwdlEvent::DisabledVerified {
        global_config,
        interrupt_enable,
    });
    let snapshot = read_fwdl_registers(transport).map_err(DisabledFwdlError::Transport)?;
    event(DisabledFwdlEvent::Snapshot(snapshot));
    transport.release_fence();
    event(DisabledFwdlEvent::DescriptorFence);
    let expected = FwdlRingRegisters {
        descriptor_base: arena_iova as u32,
        descriptor_count: MT7921_FWDL_RING_COUNT,
        cpu_index: 0,
        dma_index: snapshot.dma_index,
    };
    let operation = (|| {
        for (register, value) in [
            (DisabledFwdlWrite::DescriptorBase, expected.descriptor_base),
            (
                DisabledFwdlWrite::DescriptorCount,
                expected.descriptor_count,
            ),
            (DisabledFwdlWrite::CpuIndex, expected.cpu_index),
        ] {
            transport
                .write(register, value)
                .map_err(DisabledFwdlError::Transport)?;
            event(DisabledFwdlEvent::RegisterWritten { register, value });
        }
        let actual = read_fwdl_registers(transport).map_err(DisabledFwdlError::Transport)?;
        if actual != expected {
            return Err(DisabledFwdlError::Readback { expected, actual });
        }
        event(DisabledFwdlEvent::Programmed(actual));
        Ok(actual)
    })();
    for (register, value) in [
        (DisabledFwdlWrite::CpuIndex, snapshot.cpu_index),
        (
            DisabledFwdlWrite::DescriptorCount,
            snapshot.descriptor_count,
        ),
        (DisabledFwdlWrite::DescriptorBase, snapshot.descriptor_base),
    ] {
        if let Err(error) = transport.write(register, value) {
            return Err(DisabledFwdlError::Restore(error));
        }
    }
    event(DisabledFwdlEvent::Restored(snapshot));
    operation
}

fn read_fwdl_registers<T: DisabledFwdlRingTransport>(
    transport: &mut T,
) -> Result<FwdlRingRegisters, T::Error> {
    Ok(FwdlRingRegisters {
        descriptor_base: transport.read(DisabledFwdlRegister::DescriptorBase)?,
        descriptor_count: transport.read(DisabledFwdlRegister::DescriptorCount)?,
        cpu_index: transport.read(DisabledFwdlRegister::CpuIndex)?,
        dma_index: transport.read(DisabledFwdlRegister::DmaIndex)?,
    })
}

pub trait DisabledFwdlInterruptTransport {
    type Error;
    fn read_global_config(&mut self) -> Result<u32, Self::Error>;
    fn read_interrupt_enable(&mut self) -> Result<u32, Self::Error>;
    fn write_interrupt_enable(&mut self, value: u32) -> Result<(), Self::Error>;
    fn read_interrupt_status(&mut self) -> Result<u32, Self::Error>;
    fn acknowledge_interrupt_status(&mut self, value: u32) -> Result<(), Self::Error>;
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DisabledInterruptEvent {
    Snapshot {
        global_config: u32,
        enable: u32,
        status: u32,
    },
    Masked,
    Acknowledged {
        value: u32,
    },
    Readback {
        status: u32,
    },
    MaskRestored {
        value: u32,
    },
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum DisabledInterruptError<E> {
    DmaActive(u32),
    InterruptsEnabled(u32),
    Transport(E),
    MaskReadback(u32),
    AckDidNotClear(u32),
    Restore(E),
}

/// Port the firmware-download subset of pinned Linux `mt792x_irq_tasklet`.
///
/// The physical slice starts with a zero host mask, writes that same zero mask,
/// and acknowledges only TX ring 16's W1C status bit. No unrelated pending bit
/// is acknowledged and no interrupt source is enabled or armed.
pub fn mask_ack_disabled_fwdl_interrupt<T, F>(
    transport: &mut T,
    mut event: F,
) -> Result<u32, DisabledInterruptError<T::Error>>
where
    T: DisabledFwdlInterruptTransport,
    F: FnMut(DisabledInterruptEvent),
{
    let global_config = transport
        .read_global_config()
        .map_err(DisabledInterruptError::Transport)?;
    if global_config & 0x5 != 0 {
        return Err(DisabledInterruptError::DmaActive(global_config));
    }
    let enable = transport
        .read_interrupt_enable()
        .map_err(DisabledInterruptError::Transport)?;
    if enable != 0 {
        return Err(DisabledInterruptError::InterruptsEnabled(enable));
    }
    let status = transport
        .read_interrupt_status()
        .map_err(DisabledInterruptError::Transport)?;
    event(DisabledInterruptEvent::Snapshot {
        global_config,
        enable,
        status,
    });
    transport
        .write_interrupt_enable(0)
        .map_err(DisabledInterruptError::Transport)?;
    event(DisabledInterruptEvent::Masked);
    let operation = (|| {
        let mask_readback = transport
            .read_interrupt_enable()
            .map_err(DisabledInterruptError::Transport)?;
        if mask_readback != 0 {
            return Err(DisabledInterruptError::MaskReadback(mask_readback));
        }
        let acknowledged = status & MT7921_INT_TX_DONE_FWDL;
        transport
            .acknowledge_interrupt_status(acknowledged)
            .map_err(DisabledInterruptError::Transport)?;
        event(DisabledInterruptEvent::Acknowledged {
            value: acknowledged,
        });
        let readback = transport
            .read_interrupt_status()
            .map_err(DisabledInterruptError::Transport)?;
        event(DisabledInterruptEvent::Readback { status: readback });
        if readback & acknowledged != 0 {
            return Err(DisabledInterruptError::AckDidNotClear(readback));
        }
        Ok(readback)
    })();
    if let Err(error) = transport.write_interrupt_enable(enable) {
        return Err(DisabledInterruptError::Restore(error));
    }
    event(DisabledInterruptEvent::MaskRestored { value: enable });
    operation
}

pub trait DisabledFirmwareStageTransport {
    type Error;
    fn write_payload(&mut self, bytes: &[u8]) -> Result<(), Self::Error>;
    fn write_descriptor(&mut self, descriptor: DmaDescriptor) -> Result<(), Self::Error>;
    fn release_fence(&mut self);
    fn read_descriptor(&mut self) -> Result<DmaDescriptor, Self::Error>;
    fn reset_descriptor(&mut self) -> Result<(), Self::Error>;
    fn zero_payload(&mut self, length: usize) -> Result<(), Self::Error>;
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DisabledFirmwareStageEvent {
    PayloadWritten { bytes: usize, iova: u64 },
    DescriptorWritten(DmaDescriptor),
    DescriptorFence,
    DescriptorVerified(DmaDescriptor),
    DescriptorReset,
    PayloadZeroed { bytes: usize },
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum DisabledFirmwareStageError<E> {
    InvalidPayload,
    InvalidIova,
    Descriptor(DescriptorError),
    Transport(E),
    DescriptorReadback {
        expected: DmaDescriptor,
        actual: DmaDescriptor,
    },
    Reset(E),
}

/// Stage one raw `FW_SCATTER` chunk without publishing a producer index.
///
/// Pinned Linux limits PCI firmware chunks to 4096 bytes. `FW_SCATTER` skips
/// the normal MCU TX header, so the ring descriptor directly names the bounded
/// artifact payload. Cleanup is mandatory on success and readback failure.
pub fn stage_disabled_firmware_chunk<T, F>(
    transport: &mut T,
    payload_iova: u64,
    payload: &[u8],
    mut event: F,
) -> Result<DmaDescriptor, DisabledFirmwareStageError<T::Error>>
where
    T: DisabledFirmwareStageTransport,
    F: FnMut(DisabledFirmwareStageEvent),
{
    if payload.is_empty() || payload.len() > MT7921_FWDL_CHUNK_BYTES {
        return Err(DisabledFirmwareStageError::InvalidPayload);
    }
    if payload_iova
        .checked_add(payload.len() as u64 - 1)
        .is_none_or(|end| end > u64::from(u32::MAX))
    {
        return Err(DisabledFirmwareStageError::InvalidIova);
    }
    let descriptor = mt7921_dma_tx(
        DmaSegment {
            iova: payload_iova,
            len: payload.len() as u16,
        },
        None,
        0,
    )
    .map_err(DisabledFirmwareStageError::Descriptor)?;
    let operation = (|| {
        transport
            .write_payload(payload)
            .map_err(DisabledFirmwareStageError::Transport)?;
        event(DisabledFirmwareStageEvent::PayloadWritten {
            bytes: payload.len(),
            iova: payload_iova,
        });
        transport
            .write_descriptor(descriptor)
            .map_err(DisabledFirmwareStageError::Transport)?;
        event(DisabledFirmwareStageEvent::DescriptorWritten(descriptor));
        transport.release_fence();
        event(DisabledFirmwareStageEvent::DescriptorFence);
        match transport.read_descriptor() {
            Ok(actual) if actual == descriptor => {
                event(DisabledFirmwareStageEvent::DescriptorVerified(actual));
                Ok(descriptor)
            }
            Ok(actual) => Err(DisabledFirmwareStageError::DescriptorReadback {
                expected: descriptor,
                actual,
            }),
            Err(error) => Err(DisabledFirmwareStageError::Transport(error)),
        }
    })();
    let descriptor_reset = transport.reset_descriptor();
    if descriptor_reset.is_ok() {
        event(DisabledFirmwareStageEvent::DescriptorReset);
    }
    let payload_zeroed = transport.zero_payload(payload.len());
    if payload_zeroed.is_ok() {
        event(DisabledFirmwareStageEvent::PayloadZeroed {
            bytes: payload.len(),
        });
    }
    descriptor_reset.map_err(DisabledFirmwareStageError::Reset)?;
    payload_zeroed.map_err(DisabledFirmwareStageError::Reset)?;
    operation
}

pub const MT7921_TX_RING_SLOTS: usize = 18;
pub const MT7921_FWDL_RING_INDEX: usize = 16;
pub const MT7921_MCU_TX_RING_INDEX: usize = 17;
pub const MT7921_MCU_TX_RING_COUNT: u32 = 256;
pub const MT7921_MCU_RX_RING_COUNT: usize = 8;
pub const MT7921_MCU_RX_BUFFER_BYTES: usize = 2048;
pub const MT7921_RX_RING_SLOTS: usize = 8;
pub const MT7921_RESET_ALL_TX_INDICES: u32 = u32::MAX;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct McuRxRing {
    pub descriptors: [DmaDescriptor; MT7921_MCU_RX_RING_COUNT],
    pub producer_index: u32,
}

/// Build Linux's eight-entry pre-firmware MCU response ring while retaining
/// one empty descriptor so producer and consumer indices cannot alias full.
pub fn prepare_mcu_rx_ring(
    ring_iova: u64,
    buffers_iova: u64,
) -> Result<McuRxRing, DescriptorError> {
    let buffers_bytes = MT7921_MCU_RX_RING_COUNT * MT7921_MCU_RX_BUFFER_BYTES;
    if !ring_iova.is_multiple_of(4096)
        || !buffers_iova.is_multiple_of(4096)
        || ring_iova
            .checked_add(4095)
            .is_none_or(|end| end > u64::from(u32::MAX))
        || buffers_iova
            .checked_add(buffers_bytes as u64 - 1)
            .is_none_or(|end| end > u64::from(u32::MAX))
        || (ring_iova < buffers_iova + buffers_bytes as u64 && buffers_iova <= ring_iova + 4095)
    {
        return Err(DescriptorError::InvalidArena);
    }
    let mut descriptors = [DmaDescriptor::reset(); MT7921_MCU_RX_RING_COUNT];
    for (index, descriptor) in descriptors
        .iter_mut()
        .enumerate()
        .take(MT7921_MCU_RX_RING_COUNT - 1)
    {
        *descriptor = mt7921_dma_rx(DmaSegment {
            iova: buffers_iova + (index * MT7921_MCU_RX_BUFFER_BYTES) as u64,
            len: MT7921_MCU_RX_BUFFER_BYTES as u16,
        })?;
    }
    Ok(McuRxRing {
        descriptors,
        producer_index: (MT7921_MCU_RX_RING_COUNT - 1) as u32,
    })
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct McuRxRegisters {
    pub descriptor_base: u32,
    pub descriptor_count: u32,
    pub cpu_index: u32,
    pub dma_index: u32,
}

pub trait DisabledMcuRxTransport {
    type Error;
    fn read_global_config(&mut self) -> Result<u32, Self::Error>;
    fn read_interrupt_enable(&mut self) -> Result<u32, Self::Error>;
    fn read_registers(&mut self) -> Result<McuRxRegisters, Self::Error>;
    fn read_registers_at(&mut self, index: usize) -> Result<McuRxRegisters, Self::Error>;
    fn write_initial(
        &mut self,
        descriptor_base: u32,
        descriptor_count: u32,
    ) -> Result<(), Self::Error>;
    fn publish_cpu_index(&mut self, cpu_index: u32) -> Result<(), Self::Error>;
    fn write_ring_initial(
        &mut self,
        index: usize,
        descriptor_base: u32,
        descriptor_count: u32,
    ) -> Result<(), Self::Error>;
    fn publish_ring_cpu_index(&mut self, index: usize, cpu_index: u32) -> Result<(), Self::Error>;
    fn release_fence(&mut self);
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DisabledMcuRxEvent {
    Snapshot(McuRxRegisters),
    SnapshotAt { index: usize, state: McuRxRegisters },
    DescriptorFence,
    Programmed(McuRxRegisters),
    ProgrammedAt { index: usize, state: McuRxRegisters },
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum DisabledMcuRxError<E> {
    InvalidArena,
    InvalidMmio,
    ActiveState {
        global_config: u32,
        interrupt_enable: u32,
    },
    DirtyRing(McuRxRegisters),
    Transport(E),
    Readback(McuRxRegisters),
}

/// Program the pre-firmware MCU response ring without enabling RX DMA.
pub fn program_disabled_mcu_rx_ring<T, F>(
    transport: &mut T,
    ring_iova: u64,
    mut event: F,
) -> Result<McuRxRegisters, DisabledMcuRxError<T::Error>>
where
    T: DisabledMcuRxTransport,
    F: FnMut(DisabledMcuRxEvent),
{
    if !ring_iova.is_multiple_of(4096)
        || ring_iova
            .checked_add(4095)
            .is_none_or(|end| end > u64::from(u32::MAX))
    {
        return Err(DisabledMcuRxError::InvalidArena);
    }
    let global_config = transport
        .read_global_config()
        .map_err(DisabledMcuRxError::Transport)?;
    let interrupt_enable = transport
        .read_interrupt_enable()
        .map_err(DisabledMcuRxError::Transport)?;
    if global_config & 0xf != 0 || interrupt_enable != 0 {
        return Err(DisabledMcuRxError::ActiveState {
            global_config,
            interrupt_enable,
        });
    }
    let snapshot = transport
        .read_registers()
        .map_err(DisabledMcuRxError::Transport)?;
    event(DisabledMcuRxEvent::Snapshot(snapshot));
    if snapshot.cpu_index != snapshot.dma_index {
        return Err(DisabledMcuRxError::DirtyRing(snapshot));
    }
    let initial = McuRxRegisters {
        descriptor_base: ring_iova as u32,
        descriptor_count: MT7921_MCU_RX_RING_COUNT as u32,
        cpu_index: 0,
        dma_index: 0,
    };
    transport
        .write_initial(initial.descriptor_base, initial.descriptor_count)
        .map_err(DisabledMcuRxError::Transport)?;
    if transport
        .read_registers()
        .map_err(DisabledMcuRxError::Transport)?
        != initial
    {
        return Err(DisabledMcuRxError::Readback(
            transport
                .read_registers()
                .map_err(DisabledMcuRxError::Transport)?,
        ));
    }
    transport.release_fence();
    event(DisabledMcuRxEvent::DescriptorFence);
    transport
        .publish_cpu_index((MT7921_MCU_RX_RING_COUNT - 1) as u32)
        .map_err(DisabledMcuRxError::Transport)?;
    let expected = McuRxRegisters {
        cpu_index: (MT7921_MCU_RX_RING_COUNT - 1) as u32,
        ..initial
    };
    let actual = transport
        .read_registers()
        .map_err(DisabledMcuRxError::Transport)?;
    if actual != expected {
        return Err(DisabledMcuRxError::Readback(actual));
    }
    event(DisabledMcuRxEvent::Programmed(actual));
    Ok(actual)
}

/// Replace every MT7921 RX ring slot with owned backing while RX DMA is off.
///
/// Ring zero receives the seven-buffer MCU response queue. All other slots
/// point at a CPU-owned guard page with equal producer and consumer indices,
/// so globally enabling RX DMA cannot follow stale kernel mappings.
pub fn prepare_global_rx_rings<T, F>(
    transport: &mut T,
    guard_iova: u64,
    mcu_ring_iova: u64,
    mut event: F,
) -> Result<[McuRxRegisters; MT7921_RX_RING_SLOTS], DisabledMcuRxError<T::Error>>
where
    T: DisabledMcuRxTransport,
    F: FnMut(DisabledMcuRxEvent),
{
    let valid_page = |iova: u64| {
        iova.is_multiple_of(4096)
            && iova
                .checked_add(4095)
                .is_some_and(|end| end <= u64::from(u32::MAX))
    };
    if !valid_page(guard_iova)
        || !valid_page(mcu_ring_iova)
        || guard_iova.abs_diff(mcu_ring_iova) < 4096
    {
        return Err(DisabledMcuRxError::InvalidArena);
    }
    let global_config = transport
        .read_global_config()
        .map_err(DisabledMcuRxError::Transport)?;
    let interrupt_enable = transport
        .read_interrupt_enable()
        .map_err(DisabledMcuRxError::Transport)?;
    if global_config & 0xf != 0 || interrupt_enable != 0 {
        return Err(DisabledMcuRxError::ActiveState {
            global_config,
            interrupt_enable,
        });
    }
    let mut owned = [McuRxRegisters {
        descriptor_base: 0,
        descriptor_count: 0,
        cpu_index: 0,
        dma_index: 0,
    }; MT7921_RX_RING_SLOTS];
    for index in 0..MT7921_RX_RING_SLOTS {
        let snapshot = transport
            .read_registers_at(index)
            .map_err(DisabledMcuRxError::Transport)?;
        event(DisabledMcuRxEvent::SnapshotAt {
            index,
            state: snapshot,
        });
        if [
            snapshot.descriptor_base,
            snapshot.descriptor_count,
            snapshot.cpu_index,
            snapshot.dma_index,
        ]
        .contains(&u32::MAX)
        {
            return Err(DisabledMcuRxError::InvalidMmio);
        }
        if snapshot.cpu_index != snapshot.dma_index {
            return Err(DisabledMcuRxError::DirtyRing(snapshot));
        }
    }
    for (index, expected) in owned.iter_mut().enumerate() {
        *expected = McuRxRegisters {
            descriptor_base: if index == 0 {
                mcu_ring_iova as u32
            } else {
                guard_iova as u32
            },
            descriptor_count: MT7921_MCU_RX_RING_COUNT as u32,
            cpu_index: if index == 0 {
                (MT7921_MCU_RX_RING_COUNT - 1) as u32
            } else {
                0
            },
            dma_index: 0,
        };
        transport
            .write_ring_initial(index, expected.descriptor_base, expected.descriptor_count)
            .map_err(DisabledMcuRxError::Transport)?;
    }
    transport.release_fence();
    event(DisabledMcuRxEvent::DescriptorFence);
    for (index, expected) in owned.iter().enumerate() {
        transport
            .publish_ring_cpu_index(index, expected.cpu_index)
            .map_err(DisabledMcuRxError::Transport)?;
        let actual = transport
            .read_registers_at(index)
            .map_err(DisabledMcuRxError::Transport)?;
        if actual != *expected {
            return Err(DisabledMcuRxError::Readback(actual));
        }
        event(DisabledMcuRxEvent::ProgrammedAt {
            index,
            state: actual,
        });
    }
    Ok(owned)
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct TxRingState {
    pub descriptor_base: u32,
    pub descriptor_count: u32,
    pub cpu_index: u32,
    pub dma_index: u32,
}

pub trait GlobalTxRingTransport {
    type Error;
    fn read_global_config(&mut self) -> Result<u32, Self::Error>;
    fn read_interrupt_enable(&mut self) -> Result<u32, Self::Error>;
    fn read_tx_ring(&mut self, index: usize) -> Result<TxRingState, Self::Error>;
    fn write_tx_ring(
        &mut self,
        index: usize,
        descriptor_base: u32,
        descriptor_count: u32,
        cpu_index: u32,
    ) -> Result<(), Self::Error>;
    fn reset_tx_indices(&mut self, value: u32) -> Result<(), Self::Error>;
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum GlobalTxRingEvent {
    Snapshot { index: usize, state: TxRingState },
    RingOwned { index: usize, descriptor_base: u32 },
    IndicesReset,
    RingVerified { index: usize, state: TxRingState },
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum GlobalTxRingError<E> {
    InvalidArena,
    ActiveState {
        global_config: u32,
        interrupt_enable: u32,
    },
    InvalidMmio,
    DirtyRing {
        index: usize,
        state: TxRingState,
    },
    Transport(E),
    Readback {
        index: usize,
        state: TxRingState,
    },
}

/// Replace every globally enabled TX ring with owned, pinned backing.
///
/// TX DMA remains disabled throughout this transition. All eighteen hardware
/// ring slots are inspected, must be idle, and are then pointed either at the
/// target firmware ring or a page-sized guard ring. Linux's all-ring DTX reset
/// is issued only after every base/count/CPU index is safe, then every DIDX is
/// required to read zero. Old kernel DMA bases are deliberately not restored.
pub fn prepare_global_tx_rings<T, F>(
    transport: &mut T,
    guard_iova: u64,
    fwdl_iova: u64,
    mcu_iova: u64,
    mut event: F,
) -> Result<[TxRingState; MT7921_TX_RING_SLOTS], GlobalTxRingError<T::Error>>
where
    T: GlobalTxRingTransport,
    F: FnMut(GlobalTxRingEvent),
{
    let page_end = |iova: u64| {
        iova.is_multiple_of(4096)
            .then(|| iova.checked_add(4095))
            .flatten()
            .filter(|end| *end <= u64::from(u32::MAX))
    };
    let Some(guard_end) = page_end(guard_iova) else {
        return Err(GlobalTxRingError::InvalidArena);
    };
    let Some(fwdl_end) = page_end(fwdl_iova) else {
        return Err(GlobalTxRingError::InvalidArena);
    };
    let Some(mcu_end) = page_end(mcu_iova) else {
        return Err(GlobalTxRingError::InvalidArena);
    };
    if (guard_iova <= fwdl_end && fwdl_iova <= guard_end)
        || (guard_iova <= mcu_end && mcu_iova <= guard_end)
        || (fwdl_iova <= mcu_end && mcu_iova <= fwdl_end)
    {
        return Err(GlobalTxRingError::InvalidArena);
    }
    let global_config = transport
        .read_global_config()
        .map_err(GlobalTxRingError::Transport)?;
    let interrupt_enable = transport
        .read_interrupt_enable()
        .map_err(GlobalTxRingError::Transport)?;
    if global_config == u32::MAX || interrupt_enable == u32::MAX {
        return Err(GlobalTxRingError::InvalidMmio);
    }
    if global_config & 0xf != 0 || interrupt_enable != 0 {
        return Err(GlobalTxRingError::ActiveState {
            global_config,
            interrupt_enable,
        });
    }
    for index in 0..MT7921_TX_RING_SLOTS {
        let state = transport
            .read_tx_ring(index)
            .map_err(GlobalTxRingError::Transport)?;
        if [
            state.descriptor_base,
            state.descriptor_count,
            state.cpu_index,
            state.dma_index,
        ]
        .contains(&u32::MAX)
        {
            return Err(GlobalTxRingError::InvalidMmio);
        }
        event(GlobalTxRingEvent::Snapshot { index, state });
        if state.cpu_index != 0 || state.dma_index != 0 {
            return Err(GlobalTxRingError::DirtyRing { index, state });
        }
    }
    for index in 0..MT7921_TX_RING_SLOTS {
        let descriptor_base = if index == MT7921_FWDL_RING_INDEX {
            fwdl_iova as u32
        } else if index == MT7921_MCU_TX_RING_INDEX {
            mcu_iova as u32
        } else {
            guard_iova as u32
        };
        let descriptor_count = if index == MT7921_MCU_TX_RING_INDEX {
            MT7921_MCU_TX_RING_COUNT
        } else {
            MT7921_FWDL_RING_COUNT
        };
        transport
            .write_tx_ring(index, descriptor_base, descriptor_count, 0)
            .map_err(GlobalTxRingError::Transport)?;
        event(GlobalTxRingEvent::RingOwned {
            index,
            descriptor_base,
        });
    }
    transport
        .reset_tx_indices(MT7921_RESET_ALL_TX_INDICES)
        .map_err(GlobalTxRingError::Transport)?;
    event(GlobalTxRingEvent::IndicesReset);
    let mut owned = [TxRingState {
        descriptor_base: 0,
        descriptor_count: 0,
        cpu_index: 0,
        dma_index: 0,
    }; MT7921_TX_RING_SLOTS];
    for (index, state) in owned.iter_mut().enumerate() {
        *state = transport
            .read_tx_ring(index)
            .map_err(GlobalTxRingError::Transport)?;
        let expected_base = if index == MT7921_FWDL_RING_INDEX {
            fwdl_iova as u32
        } else if index == MT7921_MCU_TX_RING_INDEX {
            mcu_iova as u32
        } else {
            guard_iova as u32
        };
        let expected_count = if index == MT7921_MCU_TX_RING_INDEX {
            MT7921_MCU_TX_RING_COUNT
        } else {
            MT7921_FWDL_RING_COUNT
        };
        if *state
            != (TxRingState {
                descriptor_base: expected_base,
                descriptor_count: expected_count,
                cpu_index: 0,
                dma_index: 0,
            })
        {
            return Err(GlobalTxRingError::Readback {
                index,
                state: *state,
            });
        }
        event(GlobalTxRingEvent::RingVerified {
            index,
            state: *state,
        });
    }
    Ok(owned)
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DownloadCommand {
    NicPowerControl,
    GetNicCapability,
    ReadEepromBlock {
        address: u32,
    },
    PatchSemaphoreGet,
    PatchSemaphoreRelease,
    PatchFinish,
    FirmwareStart {
        address: u32,
        option: u32,
    },
    PatchStart {
        address: u32,
        length: u32,
        mode: u32,
    },
    TargetAddressLength {
        address: u32,
        length: u32,
        mode: u32,
    },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DownloadCommandError {
    InvalidSequence,
    InvalidLength,
    InvalidFirmwareStart,
    InvalidEepromAddress,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct NicPhyCapability {
    pub ht: bool,
    pub vht: bool,
    pub has_5ghz: bool,
    pub max_bandwidth: u8,
    pub spatial_streams: u8,
    pub hardware_path: u8,
    pub he: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct NicCapability {
    pub element_count: u16,
    pub mac_address: Option<[u8; 6]>,
    pub phy: Option<NicPhyCapability>,
    pub has_6ghz: Option<bool>,
    pub chip_capability: Option<u64>,
    pub unknown_elements: u16,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PhysicalBand {
    Ghz2,
    Ghz5,
    Ghz6,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct CandidateChannel {
    pub band: PhysicalBand,
    pub number: u16,
    pub frequency_mhz: u16,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct CandidateChannelSummary {
    pub ghz2: u16,
    pub ghz5: u16,
    pub ghz6: u16,
}

const PASSIVE_5GHZ: [u16; 25] = [
    36, 40, 44, 48, 52, 56, 60, 64, 100, 104, 108, 112, 116, 120, 124, 128, 132, 136, 140, 144,
    149, 153, 157, 161, 165,
];

/// One enabled channel in pinned Linux `mt76_connac_mcu_channel_domain`.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ChannelDomainChannel {
    pub band: PhysicalBand,
    pub number: u16,
    pub flags: u32,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ChannelDomainCommand {
    pub alpha2: [u8; 2],
    pub indoor: bool,
    pub special_unii_mask: u8,
    pub channels: Vec<ChannelDomainChannel>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ChannelDomainError {
    NonWorldDomain,
    OutdoorEnvironment,
    NonzeroSpecialUniiMask,
    MissingBandCapabilities,
    InvalidSequence,
    InvalidChannelSet,
}

/// Generate only the fail-closed world/indoor passive channel intersection.
/// Every entry carries Linux `IEEE80211_CHAN_NO_IR`; SET_CHAN_DOMAIN therefore
/// cannot authorize transmission before the later passive-scan boundary.
pub fn conservative_channel_domain(
    capability: NicCapability,
    alpha2: [u8; 2],
    indoor: bool,
    special_unii_mask: u8,
) -> Result<ChannelDomainCommand, ChannelDomainError> {
    if alpha2 != *b"00" {
        return Err(ChannelDomainError::NonWorldDomain);
    }
    if !indoor {
        return Err(ChannelDomainError::OutdoorEnvironment);
    }
    if special_unii_mask != 0 {
        return Err(ChannelDomainError::NonzeroSpecialUniiMask);
    }
    if capability.phy.is_none() {
        return Err(ChannelDomainError::MissingBandCapabilities);
    }
    let channels: Vec<ChannelDomainChannel> = candidate_channels(capability)
        .into_iter()
        .filter(|channel| {
            matches!(channel.band, PhysicalBand::Ghz2) && channel.number <= 14
                || matches!(channel.band, PhysicalBand::Ghz5)
                    && PASSIVE_5GHZ.contains(&channel.number)
        })
        .map(|channel| ChannelDomainChannel {
            band: channel.band,
            number: channel.number,
            flags: 1 << 1,
        })
        .collect();
    if channels.is_empty() {
        return Err(ChannelDomainError::MissingBandCapabilities);
    }
    Ok(ChannelDomainCommand {
        alpha2,
        indoor,
        special_unii_mask,
        channels,
    })
}

/// Build the physical candidate universe installed by pinned mt76. These are
/// not regulatory-valid channels until regdb, platform limits and CLC output
/// have been applied.
pub fn candidate_channels(capability: NicCapability) -> Vec<CandidateChannel> {
    const CHANNELS_5GHZ: [u16; 28] = [
        36, 40, 44, 48, 52, 56, 60, 64, 100, 104, 108, 112, 116, 120, 124, 128, 132, 136, 140, 144,
        149, 153, 157, 161, 165, 169, 173, 177,
    ];
    let mut channels = Vec::new();
    let Some(phy) = capability.phy else {
        return channels;
    };
    if phy.hardware_path & 1 != 0 {
        for number in 1..=14 {
            channels.push(CandidateChannel {
                band: PhysicalBand::Ghz2,
                number,
                frequency_mhz: if number == 14 {
                    2484
                } else {
                    2407 + 5 * number
                },
            });
        }
    }
    if phy.hardware_path & 2 != 0 {
        channels.extend(CHANNELS_5GHZ.into_iter().map(|number| CandidateChannel {
            band: PhysicalBand::Ghz5,
            number,
            frequency_mhz: 5000 + 5 * number,
        }));
    }
    if capability.has_6ghz == Some(true) {
        channels.extend((1..=233).step_by(4).map(|number| CandidateChannel {
            band: PhysicalBand::Ghz6,
            number,
            frequency_mhz: 5950 + 5 * number,
        }));
    }
    channels
}

pub fn candidate_channel_summary(capability: NicCapability) -> CandidateChannelSummary {
    let mut summary = CandidateChannelSummary::default();
    for channel in candidate_channels(capability) {
        match channel.band {
            PhysicalBand::Ghz2 => summary.ghz2 += 1,
            PhysicalBand::Ghz5 => summary.ghz5 += 1,
            PhysicalBand::Ghz6 => summary.ghz6 += 1,
        }
    }
    summary
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum NicCapabilityError {
    TruncatedHeader,
    TruncatedElementHeader { index: u16 },
    TruncatedElement { index: u16, length: u32 },
    InvalidKnownElement { index: u16, kind: u32 },
}

pub const MT7921_EEPROM_BLOCK_SIZE: usize = 16;
pub const MT7921_EEPROM_HW_TYPE: u32 = 0x55b;
pub const MT7921_EEPROM_HW_TYPE_BLOCK: u32 = 0x550;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct EepromBlock {
    pub address: u32,
    pub valid: u32,
    pub data: [u8; MT7921_EEPROM_BLOCK_SIZE],
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum EepromBlockError {
    Truncated,
    AddressMismatch { expected: u32, actual: u32 },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct EepromHardwareInfo {
    pub raw_type: u8,
    pub encapsulated_calibration: bool,
}

impl EepromBlock {
    pub fn hardware_info(&self) -> Result<EepromHardwareInfo, EepromBlockError> {
        if self.address != MT7921_EEPROM_HW_TYPE_BLOCK {
            return Err(EepromBlockError::AddressMismatch {
                expected: MT7921_EEPROM_HW_TYPE_BLOCK,
                actual: self.address,
            });
        }
        let raw_type = self.data[(MT7921_EEPROM_HW_TYPE - MT7921_EEPROM_HW_TYPE_BLOCK) as usize];
        Ok(EepromHardwareInfo {
            raw_type,
            encapsulated_calibration: raw_type & 1 != 0,
        })
    }
}

/// Parse pinned Linux `struct mt7921_mcu_eeprom_info` after the MCU RXD.
pub fn parse_eeprom_block(
    bytes: &[u8],
    expected_address: u32,
) -> Result<EepromBlock, EepromBlockError> {
    let response = bytes.get(..24).ok_or(EepromBlockError::Truncated)?;
    let address = u32::from_le_bytes(response[0..4].try_into().expect("fixed field"));
    if address != expected_address {
        return Err(EepromBlockError::AddressMismatch {
            expected: expected_address,
            actual: address,
        });
    }
    Ok(EepromBlock {
        address,
        valid: u32::from_le_bytes(response[4..8].try_into().expect("fixed field")),
        data: response[8..24].try_into().expect("fixed EEPROM block"),
    })
}

/// Parse the TLV body returned by pinned Linux GET_NIC_CAPAB.
pub fn parse_nic_capability(bytes: &[u8]) -> Result<NicCapability, NicCapabilityError> {
    let header = bytes.get(..4).ok_or(NicCapabilityError::TruncatedHeader)?;
    let element_count = u16::from_le_bytes([header[0], header[1]]);
    let mut offset = 4usize;
    let mut capability = NicCapability {
        element_count,
        mac_address: None,
        phy: None,
        has_6ghz: None,
        chip_capability: None,
        unknown_elements: 0,
    };
    for index in 0..element_count {
        let tlv = bytes
            .get(offset..offset + 8)
            .ok_or(NicCapabilityError::TruncatedElementHeader { index })?;
        let kind = u32::from_le_bytes(tlv[0..4].try_into().expect("fixed field"));
        let length = u32::from_le_bytes(tlv[4..8].try_into().expect("fixed field"));
        offset += 8;
        let end = offset
            .checked_add(length as usize)
            .filter(|end| *end <= bytes.len())
            .ok_or(NicCapabilityError::TruncatedElement { index, length })?;
        let value = &bytes[offset..end];
        offset = end;
        match kind {
            7 if value.len() >= 6 => {
                capability.mac_address = Some(value[..6].try_into().expect("checked MAC length"));
            }
            8 if value.len() >= 12 => {
                capability.phy = Some(NicPhyCapability {
                    ht: value[0] != 0,
                    vht: value[1] != 0,
                    has_5ghz: value[2] != 0,
                    max_bandwidth: value[3],
                    spatial_streams: value[4],
                    hardware_path: value[10],
                    he: value[11] != 0,
                });
            }
            0x18 if !value.is_empty() => capability.has_6ghz = Some(value[0] != 0),
            0x20 if value.len() >= 8 => {
                capability.chip_capability = Some(u64::from_le_bytes(
                    value[..8].try_into().expect("checked u64"),
                ));
            }
            7 | 8 | 0x18 | 0x20 => {
                return Err(NicCapabilityError::InvalidKnownElement { index, kind });
            }
            _ => capability.unknown_elements += 1,
        }
    }
    Ok(capability)
}

/// Encode the non-scatter MCU command which must precede firmware DMA.
///
/// This is the exact legacy Connac2 long command header produced by pinned
/// Linux `mt76_connac2_mcu_fill_message`, followed by the little-endian request
/// payload used by patch semaphore or download initialization.
pub fn encode_download_command(
    command: DownloadCommand,
    sequence: u8,
) -> Result<Vec<u8>, DownloadCommandError> {
    if sequence == 0 || sequence > 15 {
        return Err(DownloadCommandError::InvalidSequence);
    }
    let (cid, set_query, ext_cid, ext_cid_ack, payload): (u8, u8, u8, u8, Vec<u8>) = match command {
        DownloadCommand::NicPowerControl => (0x04, 3, 0, 0, vec![1, 0, 0, 0]),
        DownloadCommand::GetNicCapability => (0x8a, 1, 0, 0, vec![]),
        DownloadCommand::ReadEepromBlock { address } => {
            if address & 0xf != 0 || address > 0x9f0 {
                return Err(DownloadCommandError::InvalidEepromAddress);
            }
            let mut payload = vec![0; 24];
            payload[..4].copy_from_slice(&address.to_le_bytes());
            (0xed, 0, 0x01, 1, payload)
        }
        DownloadCommand::PatchSemaphoreGet => (0x10, 3, 0, 0, 1u32.to_le_bytes().to_vec()),
        DownloadCommand::PatchSemaphoreRelease => (0x10, 3, 0, 0, 0u32.to_le_bytes().to_vec()),
        DownloadCommand::PatchFinish => (0x07, 3, 0, 0, vec![0; 4]),
        DownloadCommand::FirmwareStart { address, option } => {
            if address != 0x0091_5000 || option != 1 {
                return Err(DownloadCommandError::InvalidFirmwareStart);
            }
            let mut payload = Vec::with_capacity(8);
            payload.extend_from_slice(&option.to_le_bytes());
            payload.extend_from_slice(&address.to_le_bytes());
            (0x02, 3, 0, 0, payload)
        }
        DownloadCommand::PatchStart {
            address,
            length,
            mode,
        } => {
            if address != 0x0090_0000 || length == 0 {
                return Err(DownloadCommandError::InvalidLength);
            }
            let mut payload = Vec::with_capacity(12);
            payload.extend_from_slice(&address.to_le_bytes());
            payload.extend_from_slice(&length.to_le_bytes());
            payload.extend_from_slice(&mode.to_le_bytes());
            (0x05, 3, 0, 0, payload)
        }
        DownloadCommand::TargetAddressLength {
            address,
            length,
            mode,
        } => {
            if length == 0 {
                return Err(DownloadCommandError::InvalidLength);
            }
            let mut payload = Vec::with_capacity(12);
            payload.extend_from_slice(&address.to_le_bytes());
            payload.extend_from_slice(&length.to_le_bytes());
            payload.extend_from_slice(&mode.to_le_bytes());
            (0x01, 3, 0, 0, payload)
        }
    };
    let total = CONNAC2_MCU_TXD_BYTES + payload.len();
    let mut bytes = vec![0; total];
    let txd0 = (total as u32) | (2 << 23) | (0x20 << 25);
    let txd1 = (1u32 << 31) | (1 << 16);
    bytes[0..4].copy_from_slice(&txd0.to_le_bytes());
    bytes[4..8].copy_from_slice(&txd1.to_le_bytes());
    bytes[32..34].copy_from_slice(&((total - 32) as u16).to_le_bytes());
    bytes[34..36].copy_from_slice(&0x8000u16.to_le_bytes());
    bytes[36] = cid;
    bytes[37] = 0xa0;
    bytes[38] = set_query;
    bytes[39] = sequence;
    bytes[41] = ext_cid;
    bytes[43] = ext_cid_ack;
    bytes[CONNAC2_MCU_TXD_BYTES..].copy_from_slice(&payload);
    Ok(bytes)
}

/// Encode pinned Linux `MCU_CE_CMD(SET_CLC)` for one opaque CLC rule.
pub fn encode_clc_set_command(
    command: &ClcSetCommand,
    sequence: u8,
) -> Result<Vec<u8>, DownloadCommandError> {
    if sequence == 0 || sequence > 15 {
        return Err(DownloadCommandError::InvalidSequence);
    }
    if command.index > 1
        || command.environment != 1
        || command.acpi_configuration > 1
        || command.capability & !1 != 0
        || command.alpha2 != *b"00"
        || command.environment_6ghz != 0
        || command.mtcl_configuration != 0xff
        || command.data.is_empty()
    {
        return Err(DownloadCommandError::InvalidLength);
    }
    let request_length = 76usize
        .checked_add(command.data.len())
        .filter(|length| *length <= u16::MAX as usize)
        .ok_or(DownloadCommandError::InvalidLength)?;
    let total = CONNAC2_MCU_TXD_BYTES + request_length;
    let mut bytes = vec![0; total];
    let txd0 = (total as u32) | (2 << 23) | (0x20 << 25);
    let txd1 = (1u32 << 31) | (1 << 16);
    bytes[0..4].copy_from_slice(&txd0.to_le_bytes());
    bytes[4..8].copy_from_slice(&txd1.to_le_bytes());
    bytes[32..34].copy_from_slice(&((total - 32) as u16).to_le_bytes());
    bytes[34..36].copy_from_slice(&0x8000u16.to_le_bytes());
    bytes[36..40].copy_from_slice(&[0x5c, 0xa0, 1, sequence]);
    let request = &mut bytes[CONNAC2_MCU_TXD_BYTES..];
    request[0] = 1;
    request[2..4].copy_from_slice(&(request_length as u16).to_le_bytes());
    request[4] = command.index;
    request[5] = command.environment;
    request[6] = command.acpi_configuration;
    request[7] = command.capability;
    request[8..10].copy_from_slice(&command.alpha2);
    request[10..12].copy_from_slice(&command.rule_type);
    request[12] = command.environment_6ghz;
    request[13] = command.mtcl_configuration;
    request[76..].copy_from_slice(&command.data);
    Ok(bytes)
}

/// Encode pinned Linux `MCU_CE_CMD(SET_CHAN_DOMAIN)` with its packed header
/// and channel records. This command intentionally requests no MCU response.
pub fn encode_channel_domain_command(
    command: &ChannelDomainCommand,
    sequence: u8,
) -> Result<Vec<u8>, ChannelDomainError> {
    if sequence == 0 || sequence > 15 {
        return Err(ChannelDomainError::InvalidSequence);
    }
    if command.alpha2 != *b"00" {
        return Err(ChannelDomainError::NonWorldDomain);
    }
    if !command.indoor {
        return Err(ChannelDomainError::OutdoorEnvironment);
    }
    if command.special_unii_mask != 0 {
        return Err(ChannelDomainError::NonzeroSpecialUniiMask);
    }
    let mut n_2ch = 0u8;
    let mut n_5ch = 0u8;
    let mut previous = None;
    for channel in &command.channels {
        let valid = match channel.band {
            PhysicalBand::Ghz2 => channel.number >= 1 && channel.number <= 14,
            PhysicalBand::Ghz5 => PASSIVE_5GHZ.contains(&channel.number),
            PhysicalBand::Ghz6 => false,
        };
        let order = match channel.band {
            PhysicalBand::Ghz2 => channel.number,
            PhysicalBand::Ghz5 => 256 + channel.number,
            PhysicalBand::Ghz6 => u16::MAX,
        };
        if !valid || channel.flags != 1 << 1 || previous.is_some_and(|value| value >= order) {
            return Err(ChannelDomainError::InvalidChannelSet);
        }
        previous = Some(order);
        match channel.band {
            PhysicalBand::Ghz2 => n_2ch = n_2ch.saturating_add(1),
            PhysicalBand::Ghz5 => n_5ch = n_5ch.saturating_add(1),
            PhysicalBand::Ghz6 => unreachable!("6 GHz was rejected"),
        }
    }
    if command.channels.is_empty() {
        return Err(ChannelDomainError::InvalidChannelSet);
    }
    let request_length = 12 + command.channels.len() * 8;
    let total = CONNAC2_MCU_TXD_BYTES + request_length;
    let mut bytes = vec![0; total];
    let txd0 = (total as u32) | (2 << 23) | (0x20 << 25);
    let txd1 = (1u32 << 31) | (1 << 16);
    bytes[0..4].copy_from_slice(&txd0.to_le_bytes());
    bytes[4..8].copy_from_slice(&txd1.to_le_bytes());
    bytes[32..34].copy_from_slice(&((total - 32) as u16).to_le_bytes());
    bytes[34..36].copy_from_slice(&0x8000u16.to_le_bytes());
    bytes[36..40].copy_from_slice(&[0x0f, 0xa0, 1, sequence]);
    let request = &mut bytes[CONNAC2_MCU_TXD_BYTES..];
    request[..2].copy_from_slice(&command.alpha2);
    request[4..8].copy_from_slice(&[0, 3, 3, 0]);
    request[8..12].copy_from_slice(&[n_2ch, n_5ch, 0, 0]);
    for (index, channel) in command.channels.iter().enumerate() {
        let offset = 12 + index * 8;
        request[offset..offset + 2].copy_from_slice(&channel.number.to_le_bytes());
        request[offset + 4..offset + 8].copy_from_slice(&channel.flags.to_le_bytes());
    }
    Ok(bytes)
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum PassiveMcuCommand {
    EepromBufferMode,
    MacEnable,
    SetRxPath {
        channel: CandidateChannel,
        antenna_mask: u8,
    },
    ChannelSwitch {
        channel: CandidateChannel,
        center_channel: u8,
        bandwidth: u8,
        center_channel2: u8,
        antenna_mask: u8,
    },
    AddDevice {
        mac: [u8; 6],
    },
    AddBss,
    SetPassiveRxFilter,
    StartScan {
        scan_sequence: u8,
        channel: CandidateChannel,
    },
    CancelScan {
        scan_sequence: u8,
    },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PassiveMcuCommandError {
    InvalidSequence,
    UnsupportedChannel,
    InvalidAntennaMask,
    InvalidScanSequence,
    ActiveScanMaterial,
}

impl PassiveMcuCommand {
    pub fn expects_response(&self) -> bool {
        matches!(
            self,
            Self::EepromBufferMode
                | Self::MacEnable
                | Self::SetRxPath { .. }
                | Self::ChannelSwitch { .. }
                | Self::AddDevice { .. }
                | Self::AddBss
        )
    }
}

fn encode_legacy_mcu(cid: u8, ext_cid: u8, payload: &[u8], sequence: u8) -> Vec<u8> {
    let total = CONNAC2_MCU_TXD_BYTES + payload.len();
    let mut bytes = vec![0; total];
    let txd0 = (total as u32) | (2 << 23) | (0x20 << 25);
    let txd1 = (1u32 << 31) | (1 << 16);
    bytes[0..4].copy_from_slice(&txd0.to_le_bytes());
    bytes[4..8].copy_from_slice(&txd1.to_le_bytes());
    bytes[32..34].copy_from_slice(&((total - 32) as u16).to_le_bytes());
    bytes[34..36].copy_from_slice(&0x8000u16.to_le_bytes());
    bytes[36..44].copy_from_slice(&[
        cid,
        0xa0,
        1,
        sequence,
        0,
        ext_cid,
        0,
        u8::from(ext_cid != 0),
    ]);
    bytes[CONNAC2_MCU_TXD_BYTES..].copy_from_slice(payload);
    bytes
}

fn encode_uni_mcu(cid: u16, payload: &[u8], sequence: u8) -> Vec<u8> {
    const UNI_TXD_BYTES: usize = 48;
    let total = UNI_TXD_BYTES + payload.len();
    let mut bytes = vec![0; total];
    let txd0 = (total as u32) | (2 << 23) | (0x20 << 25);
    let txd1 = (1u32 << 31) | (1 << 16);
    bytes[0..4].copy_from_slice(&txd0.to_le_bytes());
    bytes[4..8].copy_from_slice(&txd1.to_le_bytes());
    bytes[32..34].copy_from_slice(&((total - 32) as u16).to_le_bytes());
    bytes[34..36].copy_from_slice(&cid.to_le_bytes());
    bytes[37] = 0xa0;
    bytes[39] = sequence;
    bytes[43] = 0x07;
    bytes[UNI_TXD_BYTES..].copy_from_slice(payload);
    bytes
}

/// Linux v7.1 `mt76_connac_mcu_uni_add_dev` for the first station VIF.
///
/// This establishes OMAC/BSS index 0 and its reserved interface WCID 19 before
/// authentication, including the exact public VIF address used by MLME/SME.
pub fn encode_client_interface_commands(
    client: [u8; 6],
    enable: bool,
    dev_sequence: u8,
    bss_sequence: u8,
) -> Result<[Vec<u8>; 2], String> {
    if client == [0; 6]
        || client[0] & 3 != 2
        || !(1..=15).contains(&dev_sequence)
        || !(1..=15).contains(&bss_sequence)
        || dev_sequence == bss_sequence
    {
        return Err("client interface identity or sequence is invalid".into());
    }
    let mut dev = vec![0; 16];
    // omac_idx=0, band_idx=0, DEV_INFO_ACTIVE, link_idx=0.
    dev[4..8].copy_from_slice(&[0, 0, 12, 0]);
    dev[8] = u8::from(enable);
    dev[10..16].copy_from_slice(&client);

    let mut bss = vec![0; 36];
    // bss_idx=0, UNI_BSS_INFO_BASIC, first station VIF/WMM/band/OMAC.
    bss[4..8].copy_from_slice(&[0, 0, 32, 0]);
    bss[8] = u8::from(enable);
    bss[12..16].copy_from_slice(&0x0001_0001u32.to_le_bytes());
    bss[16] = 1;
    bss[24..26].copy_from_slice(&19u16.to_le_bytes());
    bss[30..32].copy_from_slice(&19u16.to_le_bytes());

    let dev = encode_uni_mcu(1, &dev, dev_sequence);
    let bss = encode_uni_mcu(2, &bss, bss_sequence);
    Ok(if enable { [dev, bss] } else { [bss, dev] })
}

/// Encode only the pinned Linux commands required by the conservative passive
/// one-channel milestone. START_HW_SCAN has no SSID, probe, IE, random-MAC, or
/// transmit material and uses Connac2's firmware-selected dwell fields (zero).
pub fn encode_passive_mcu_command(
    command: &PassiveMcuCommand,
    sequence: u8,
) -> Result<Vec<u8>, PassiveMcuCommandError> {
    if sequence == 0 || sequence > 15 {
        return Err(PassiveMcuCommandError::InvalidSequence);
    }
    let channel_payload = |channel: CandidateChannel,
                           center_channel: u8,
                           bandwidth: u8,
                           center_channel2: u8,
                           antenna_mask: u8,
                           switch_reason: u8,
                           channel_switch: bool|
     -> Result<Vec<u8>, PassiveMcuCommandError> {
        let channel_band = match channel.band {
            PhysicalBand::Ghz2 if (1..=14).contains(&channel.number) => 0,
            PhysicalBand::Ghz5 if PASSIVE_5GHZ.contains(&channel.number) => 1,
            _ => return Err(PassiveMcuCommandError::UnsupportedChannel),
        };
        if channel.frequency_mhz
            != match channel.band {
                PhysicalBand::Ghz2 if channel.number == 14 => 2484,
                PhysicalBand::Ghz2 => 2407 + 5 * channel.number,
                PhysicalBand::Ghz5 => 5000 + 5 * channel.number,
                PhysicalBand::Ghz6 => unreachable!("6 GHz rejected above"),
            }
        {
            return Err(PassiveMcuCommandError::UnsupportedChannel);
        }
        if antenna_mask != 3 {
            return Err(PassiveMcuCommandError::InvalidAntennaMask);
        }
        if !matches!(bandwidth, 0..=3 | 6)
            || center_channel == 0
            || (bandwidth == 0 && center_channel != channel.number as u8)
            || (bandwidth != 6 && center_channel2 != 0)
            || (bandwidth == 6 && center_channel2 == 0)
        {
            return Err(PassiveMcuCommandError::UnsupportedChannel);
        }
        let mut payload = vec![0; 76];
        payload[0] = channel.number as u8;
        payload[1] = center_channel;
        payload[2] = bandwidth;
        payload[3] = 2;
        payload[4] = if channel_switch { 2 } else { antenna_mask };
        payload[5] = switch_reason;
        payload[7] = center_channel2;
        payload[10] = channel_band;
        Ok(payload)
    };
    Ok(match command {
        PassiveMcuCommand::EepromBufferMode => {
            encode_legacy_mcu(0xed, 0x21, &[1, 0, 0, 0], sequence)
        }
        PassiveMcuCommand::MacEnable => encode_legacy_mcu(0xed, 0x46, &[1, 0, 0, 0], sequence),
        PassiveMcuCommand::SetRxPath {
            channel,
            antenna_mask,
        } => encode_legacy_mcu(
            0xed,
            0x4e,
            &channel_payload(
                *channel,
                channel.number as u8,
                0,
                0,
                *antenna_mask,
                0,
                false,
            )?,
            sequence,
        ),
        PassiveMcuCommand::ChannelSwitch {
            channel,
            center_channel,
            bandwidth,
            center_channel2,
            antenna_mask,
        } => encode_legacy_mcu(
            0xed,
            0x08,
            &channel_payload(
                *channel,
                *center_channel,
                *bandwidth,
                *center_channel2,
                *antenna_mask,
                9,
                true,
            )?,
            sequence,
        ),
        PassiveMcuCommand::AddDevice { mac } => {
            let mut payload = vec![0; 16];
            payload[4..8].copy_from_slice(&[0, 0, 12, 0]);
            payload[8] = 1;
            payload[10..16].copy_from_slice(mac);
            encode_uni_mcu(1, &payload, sequence)
        }
        PassiveMcuCommand::AddBss => {
            let mut payload = vec![0; 36];
            payload[4..8].copy_from_slice(&[0, 0, 32, 0]);
            payload[8] = 1;
            payload[12..16].copy_from_slice(&0x0001_0001u32.to_le_bytes());
            payload[16] = 1;
            payload[24..26].copy_from_slice(&19u16.to_le_bytes());
            payload[30..32].copy_from_slice(&19u16.to_le_bytes());
            encode_uni_mcu(2, &payload, sequence)
        }
        PassiveMcuCommand::SetPassiveRxFilter => {
            let mut payload = vec![0; 68];
            payload[4] = 1;
            payload[8..12].copy_from_slice(&0x8000_0040u32.to_le_bytes());
            encode_legacy_mcu(0x0a, 0, &payload, sequence)
        }
        PassiveMcuCommand::StartScan {
            scan_sequence,
            channel,
        } => {
            if *scan_sequence > 0x7f {
                return Err(PassiveMcuCommandError::InvalidScanSequence);
            }
            let scan_band = match channel.band {
                PhysicalBand::Ghz2 if (1..=14).contains(&channel.number) => 1,
                PhysicalBand::Ghz5 if PASSIVE_5GHZ.contains(&channel.number) => 2,
                _ => return Err(PassiveMcuCommandError::UnsupportedChannel),
            };
            let mut payload = vec![0; 1186];
            payload[0] = *scan_sequence;
            payload[3] = 1;
            payload[7] = 1;
            payload[158] = 4;
            payload[159] = 1;
            payload[160] = scan_band;
            payload[161] = channel.number as u8;
            payload[6] = 1 << 5;
            encode_legacy_mcu(0x03, 0, &payload, sequence)
        }
        PassiveMcuCommand::CancelScan { scan_sequence } => {
            if *scan_sequence > 0x7f {
                return Err(PassiveMcuCommandError::InvalidScanSequence);
            }
            encode_legacy_mcu(0x1b, 0, &[*scan_sequence, 0, 0, 0], sequence)
        }
    })
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PassiveScanDone {
    pub scan_sequence: u8,
    pub completed_channels: u8,
    pub beacon_scan_count: u32,
    pub alpha2: [u8; 2],
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PassiveAdvertisement {
    pub probe_response: bool,
    pub bssid: [u8; 6],
    pub beacon_interval_tu: u16,
    pub capability_info: u16,
    pub ies: Vec<u8>,
    pub band: PhysicalBand,
    pub channel: u8,
    pub rssi_dbm: i8,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PassiveRxError {
    Truncated,
    WrongEvent,
    WrongPacketType,
    RxError,
    HeaderTranslated,
    MissingRxVector,
    UnsupportedFrame,
    InvalidChannel,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PassiveMacMmioOperation {
    Rmw {
        address: u32,
        mask: u32,
        value: u32,
    },
    WtblClear {
        index: u8,
        address: u32,
        value: u32,
        busy_mask: u32,
        timeout_us: u32,
    },
}

/// Exact ordered MMIO closure from pinned `mt7921_mac_init` and
/// `mt792x_mac_init_band`. Physical code must execute every entry with the
/// source primitive's verification semantics; partial support is not
/// sufficient to attest `mac_mmio_initialized`.
pub fn passive_mac_mmio_plan() -> Vec<PassiveMacMmioOperation> {
    let mut plan = vec![
        PassiveMacMmioOperation::Rmw {
            address: 0x820c_d004,
            mask: 0x0000_fff8,
            value: 1536 << 3,
        },
        PassiveMacMmioOperation::Rmw {
            address: 0x820c_d000,
            mask: 1 << 15,
            value: 1 << 15,
        },
        PassiveMacMmioOperation::Rmw {
            address: 0x820c_d000,
            mask: 1 << 19,
            value: 1 << 19,
        },
    ];
    plan.extend((0..20).map(|index| PassiveMacMmioOperation::WtblClear {
        index,
        address: 0x820d_4230,
        value: u32::from(index) | (1 << 12),
        busy_mask: 1 << 31,
        timeout_us: 5000,
    }));
    for band in 0..2 {
        let (tmac, dma, rmac, mib, wtbloff) = if band == 0 {
            (
                0x820e_4000,
                0x820e_7000,
                0x820e_5000,
                0x820e_d000,
                0x820e_9000,
            )
        } else {
            (
                0x820f_4000,
                0x820f_7000,
                0x820f_5000,
                0x820f_d000,
                0x820f_9000,
            )
        };
        plan.extend([
            PassiveMacMmioOperation::Rmw {
                address: tmac + 0x0f4,
                mask: 0x3f,
                value: 0x3f,
            },
            PassiveMacMmioOperation::Rmw {
                address: tmac + 0x0f4,
                mask: (1 << 17) | (1 << 18),
                value: (1 << 17) | (1 << 18),
            },
            PassiveMacMmioOperation::Rmw {
                address: rmac + 0x03c4,
                mask: 1 << 30,
                value: 1 << 30,
            },
            PassiveMacMmioOperation::Rmw {
                address: rmac + 0x0380,
                mask: 1 << 30,
                value: 1 << 30,
            },
            PassiveMacMmioOperation::Rmw {
                address: mib + 0x004,
                mask: 1 << 8,
                value: 1 << 8,
            },
            PassiveMacMmioOperation::Rmw {
                address: mib + 0x004,
                mask: 1 << 9,
                value: 1 << 9,
            },
            PassiveMacMmioOperation::Rmw {
                address: dma,
                mask: 0x0000_fff8,
                value: 1536 << 3,
            },
            PassiveMacMmioOperation::Rmw {
                address: dma,
                mask: 1 << 23,
                value: 0,
            },
            PassiveMacMmioOperation::Rmw {
                address: wtbloff + 0x008,
                mask: (3 << 30) | (3 << 24),
                value: 3 << 24,
            },
        ]);
    }
    plan
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PassiveMacBarError {
    UnsupportedAddress(u32),
    AllOnes { address: u32 },
}

/// Translate only the fixed-map regions touched by the mandatory passive MAC
/// plan. Pinned `__mt7921_reg_addr` resolves these before its L1-remap fallback;
/// callers must not mutate `MT_HIF_REMAP_L1` for any address accepted here.
pub fn passive_mac_bar_offset(address: u32) -> Result<usize, PassiveMacBarError> {
    const FIXED: [(u32, u32, u32); 12] = [
        (0x820d_0000, 0x0003_0000, 0x0001_0000),
        (0x820e_d000, 0x0002_4800, 0x0000_0800),
        (0x820e_4000, 0x0002_1000, 0x0000_0400),
        (0x820e_7000, 0x0002_1e00, 0x0000_0200),
        (0x820e_5000, 0x0002_1400, 0x0000_0800),
        (0x820c_d000, 0x0000_f000, 0x0000_1000),
        (0x820e_9000, 0x0002_3400, 0x0000_0200),
        (0x820f_4000, 0x000a_1000, 0x0000_0400),
        (0x820f_5000, 0x000a_1400, 0x0000_0800),
        (0x820f_7000, 0x000a_1e00, 0x0000_0200),
        (0x820f_9000, 0x000a_3400, 0x0000_0200),
        (0x820f_d000, 0x000a_4800, 0x0000_0800),
    ];
    let wtbl_peer_readback = matches!(address, 0x820d_8700 | 0x820d_8704);
    if !wtbl_peer_readback
        && !passive_mac_mmio_plan()
            .iter()
            .any(|operation| match operation {
                PassiveMacMmioOperation::Rmw {
                    address: expected, ..
                }
                | PassiveMacMmioOperation::WtblClear {
                    address: expected, ..
                } => *expected == address,
            })
    {
        return Err(PassiveMacBarError::UnsupportedAddress(address));
    }
    for (physical, mapped, size) in FIXED {
        if let Some(offset) = address.checked_sub(physical)
            && offset <= size
        {
            return Ok((mapped + offset) as usize);
        }
    }
    Err(PassiveMacBarError::UnsupportedAddress(address))
}

pub fn validate_passive_mac_bar_read(
    address: u32,
    value: u32,
) -> Result<(usize, u32), PassiveMacBarError> {
    let offset = passive_mac_bar_offset(address)?;
    if value == u32::MAX {
        return Err(PassiveMacBarError::AllOnes { address });
    }
    Ok((offset, value))
}

/// Value produced by pinned `mt76_mmio_rmw`: one MMIO read, this calculation,
/// then one `writel`. Linux returns the calculated value and does not require
/// an immediate hardware readback to match it.
pub const fn passive_mac_source_rmw_value(initial: u32, mask: u32, value: u32) -> u32 {
    mt76_mmio_rmw_value(initial, mask, value)
}

pub fn parse_passive_scan_done(bytes: &[u8]) -> Result<PassiveScanDone, PassiveRxError> {
    let response = parse_download_response(bytes, 0).map_err(|_| PassiveRxError::Truncated)?;
    if response.event_id != 0x0d || response.sequence != 0 {
        return Err(PassiveRxError::WrongEvent);
    }
    let body = bytes.get(36..56).ok_or(PassiveRxError::Truncated)?;
    Ok(PassiveScanDone {
        scan_sequence: body[0] & 0x7f,
        completed_channels: body[4],
        beacon_scan_count: u32::from_le_bytes(body[8..12].try_into().expect("fixed field")),
        alpha2: [body[17], body[18]],
    })
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Connac2RxFrame {
    pub bytes: Vec<u8>,
    pub band: PhysicalBand,
    pub channel: u8,
    pub rssi_dbm: i8,
    pub pn: Option<[u8; 6]>,
}

/// Strip exactly one Connac2 data/MCU-normal RX envelope into the complete
/// 802.11 frame plus the receive fields retained by the pinned client seam.
pub fn parse_connac2_rx_frame(bytes: &[u8]) -> Result<Connac2RxFrame, PassiveRxError> {
    let header = bytes.get(..24).ok_or(PassiveRxError::Truncated)?;
    let rxd0 = u32::from_le_bytes(header[0..4].try_into().expect("fixed field"));
    let reported_len = (rxd0 & 0xffff) as usize;
    let bytes = bytes.get(..reported_len).ok_or(PassiveRxError::Truncated)?;
    let rxd1 = u32::from_le_bytes(header[4..8].try_into().expect("fixed field"));
    let rxd2 = u32::from_le_bytes(header[8..12].try_into().expect("fixed field"));
    let rxd3 = u32::from_le_bytes(header[12..16].try_into().expect("fixed field"));
    let packet_type = rxd0 >> 27 & 0x1f;
    let packet_flag = rxd0 >> 16 & 0x0f;
    if packet_type != 2 && !(packet_type == 7 && packet_flag == 1) {
        return Err(PassiveRxError::WrongPacketType);
    }
    // Bit 28 is BAND_IDX on Connac2, not an RX error. Pinned Linux also
    // ignores HDR_TRANS_ERROR (RXD2 bit 25): when HDR_TRANS below is clear,
    // the payload is still the raw 802.11 frame.
    if rxd1 & ((1 << 25) | (1 << 26) | (1 << 27)) != 0 || rxd2 & ((1 << 23) | (1 << 24)) != 0 {
        return Err(PassiveRxError::RxError);
    }
    if rxd2 & (1 << 13) != 0 {
        return Err(PassiveRxError::HeaderTranslated);
    }
    let channel = ((rxd3 >> 8) & 0xff) as u8;
    let band = if (1..=14).contains(&channel) {
        PhysicalBand::Ghz2
    } else if PASSIVE_5GHZ.contains(&u16::from(channel)) {
        PhysicalBand::Ghz5
    } else {
        return Err(PassiveRxError::InvalidChannel);
    };
    let mut offset = 24usize;
    if rxd1 & (1 << 14) != 0 {
        bytes
            .get(offset..offset + 16)
            .ok_or(PassiveRxError::Truncated)?;
        offset = offset.checked_add(16).ok_or(PassiveRxError::Truncated)?;
    }
    let pn = if rxd1 & (1 << 11) != 0 {
        let group1 = bytes
            .get(offset..offset + 16)
            .ok_or(PassiveRxError::Truncated)?;
        offset = offset.checked_add(16).ok_or(PassiveRxError::Truncated)?;
        Some(connac2_group1_pn(group1).expect("GROUP1 is exactly 16 bytes"))
    } else {
        None
    };
    if rxd1 & (1 << 12) != 0 {
        bytes
            .get(offset..offset + 8)
            .ok_or(PassiveRxError::Truncated)?;
        offset = offset.checked_add(8).ok_or(PassiveRxError::Truncated)?;
    }
    if rxd1 & (1 << 13) == 0 {
        return Err(PassiveRxError::MissingRxVector);
    }
    let rxv = bytes
        .get(offset..offset + 8)
        .ok_or(PassiveRxError::Truncated)?;
    let mut rcpi = u32::from_le_bytes(rxv[4..8].try_into().expect("fixed field"));
    offset += 8;
    if rxd1 & (1 << 15) != 0 {
        let group5 = bytes
            .get(offset..offset + 72)
            .ok_or(PassiveRxError::Truncated)?;
        // Pinned Linux skips the first 24 bytes of GROUP_5, takes its
        // overriding RCPI field, then advances across the remaining 48.
        rcpi = u32::from_le_bytes(group5[24..28].try_into().expect("fixed field"));
        offset = offset.checked_add(72).ok_or(PassiveRxError::Truncated)?;
    }
    let rssi_dbm = (0..2)
        .map(|chain| ((rcpi >> (chain * 8)) & 0xff) as i16)
        .map(|value| (value - 220) / 2)
        .max()
        .unwrap_or(-128)
        .clamp(i8::MIN as i16, i8::MAX as i16) as i8;
    offset = offset
        .checked_add(2 * ((rxd2 >> 14) & 0x3) as usize)
        .ok_or(PassiveRxError::Truncated)?;
    let frame = bytes.get(offset..).ok_or(PassiveRxError::Truncated)?;
    if frame.len() < 2 {
        return Err(PassiveRxError::Truncated);
    }
    Ok(Connac2RxFrame {
        bytes: frame.to_vec(),
        band,
        channel,
        rssi_dbm,
        pn,
    })
}

/// Parse the exact Connac2 normal-RX envelope far enough to deliver only raw
/// beacon/probe-response material to pinned Fuchsia. Data/control frames,
/// translated headers, RX errors, absent P-RXV RSSI, and 6 GHz fail closed.
pub fn parse_passive_advertisement(bytes: &[u8]) -> Result<PassiveAdvertisement, PassiveRxError> {
    let Connac2RxFrame {
        bytes: frame,
        band,
        channel,
        rssi_dbm,
        ..
    } = parse_connac2_rx_frame(bytes)?;
    let fixed = frame.get(..36).ok_or(PassiveRxError::Truncated)?;
    let frame_control = u16::from_le_bytes([fixed[0], fixed[1]]);
    let probe_response = match frame_control & 0x00fc {
        0x0080 => false,
        0x0050 => true,
        _ => return Err(PassiveRxError::UnsupportedFrame),
    };
    Ok(PassiveAdvertisement {
        probe_response,
        bssid: fixed[16..22].try_into().expect("fixed field"),
        beacon_interval_tu: u16::from_le_bytes([fixed[32], fixed[33]]),
        capability_info: u16::from_le_bytes([fixed[34], fixed[35]]),
        ies: frame[36..].to_vec(),
        band,
        channel,
        rssi_dbm,
    })
}

pub const FIRMWARE_POLL_INTERVAL_MS: u64 = 10;
pub const DOWNLOAD_READY_TIMEOUT_MS: u64 = 1000;
pub const N9_READY_TIMEOUT_MS: u64 = 1500;
/// A fail-closed per-chunk safety deadline. Pinned PCI Linux uses a 3-second
/// MCU timeout and requests no FW_SCATTER response; this offline model is
/// deliberately stricter by requiring synchronous TX completion per chunk.
pub const SCATTER_COMPLETION_TIMEOUT_MS: u64 = 3000;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FirmwareLoaderState {
    Powering,
    DownloadReady,
    PatchSemaphoreHeld,
    PatchSemaphoreReleased,
    PatchComplete,
    RamDownloading,
    FirmwareStarted,
    N9Ready,
    CapabilityDiscovered,
    EepromDiscovered,
    ClcConfigured,
    ChannelDomainConfigured,
    Ready,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FirmwareImagePart {
    Patch,
    Ram,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FirmwareLoaderOperation {
    Command(DownloadCommand),
    PublishScatter(FirmwareImagePart),
    WaitScatterCompletion(FirmwareImagePart),
    PollDownloadReady,
    PollN9Ready,
    SetClc,
    SetChannelDomain,
    PassiveBoundary,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FirmwareCommandCompletion {
    NoResponse,
    Ack,
    PatchSemaphore(PatchSemaphoreStatus),
    PatchFinish(u8),
    NicCapability(NicCapability),
    EepromBlock(EepromBlock),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PatchSemaphoreStatus {
    NotDownloadedFailed,
    AlreadyDownloaded,
    Acquired,
    Released,
    Other(u8),
}

impl From<u8> for PatchSemaphoreStatus {
    fn from(value: u8) -> Self {
        match value {
            0 => Self::NotDownloadedFailed,
            1 => Self::AlreadyDownloaded,
            2 => Self::Acquired,
            3 => Self::Released,
            value => Self::Other(value),
        }
    }
}

/// Transport boundary for the complete loader transaction. Implementations
/// own command/RX matching and one completion for every scatter chunk.
pub trait FirmwareLoaderTransport {
    type Error;

    /// Allocate the next persistent nonzero four-bit MCU sequence. Linux keeps
    /// this counter on the device and consumes a value for scatter messages.
    fn next_sequence(&mut self) -> u8;
    fn acpi_configuration(&self) -> u8;
    fn command(
        &mut self,
        command: DownloadCommand,
        sequence: u8,
        encoded: &[u8],
    ) -> Result<FirmwareCommandCompletion, Self::Error>;
    fn set_clc(
        &mut self,
        command: &ClcSetCommand,
        sequence: u8,
        encoded: &[u8],
    ) -> Result<Option<ClcSetResponse>, Self::Error>;
    fn set_channel_domain(
        &mut self,
        command: &ChannelDomainCommand,
        sequence: u8,
        encoded: &[u8],
    ) -> Result<(), Self::Error>;
    fn publish_scatter(
        &mut self,
        part: FirmwareImagePart,
        sequence: u8,
        chunk: &[u8],
    ) -> Result<(), Self::Error>;
    fn wait_scatter_completion(
        &mut self,
        part: FirmwareImagePart,
        sequence: u8,
        deadline_ms: u64,
    ) -> Result<(), Self::Error>;
    fn firmware_download_state(&mut self) -> Result<u8, Self::Error>;
    fn firmware_n9_ready(&mut self) -> Result<bool, Self::Error>;
    fn now_ms(&self) -> u64;
    fn sleep_ms(&mut self, duration_ms: u64);
    /// Quiesce DMA/IRQ activity and revoke all loader resources. `state` is
    /// the last successfully entered protocol state, not cleanup permission.
    fn fail_closed_cleanup(&mut self, state: FirmwareLoaderState) -> Result<(), Self::Error>;
}

#[derive(Debug, Eq, PartialEq)]
pub enum FirmwareLoaderFailure<E> {
    Command(DownloadCommandError),
    PatchSecurity(PatchSecurityError),
    Transport {
        operation: FirmwareLoaderOperation,
        source: E,
    },
    UnexpectedCommandCompletion {
        command: DownloadCommand,
        completion: FirmwareCommandCompletion,
    },
    UnexpectedPatchSemaphore(PatchSemaphoreStatus),
    UnexpectedPatchRelease(PatchSemaphoreStatus),
    UnexpectedPatchFinish(u8),
    PatchRelease {
        primary: Option<Box<FirmwareLoaderFailure<E>>>,
        release: Box<FirmwareLoaderFailure<E>>,
    },
    MissingFirmwareOverride,
    N9ReadyTimeout,
    Clc(ClcDiscoveryError),
    ChannelDomain(ChannelDomainError),
}

#[derive(Debug, Eq, PartialEq)]
pub enum FirmwareLoaderError<E> {
    Failed(FirmwareLoaderFailure<E>),
    Cleanup {
        failure: Option<FirmwareLoaderFailure<E>>,
        source: E,
    },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PatchDisposition {
    AlreadyDownloaded,
    Downloaded,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct FirmwareLoaderReport {
    pub download_ready_observed: bool,
    pub patch: PatchDisposition,
    pub patch_sections: usize,
    pub ram_regions: usize,
    pub scatter_chunks: usize,
    pub scatter_bytes: usize,
    pub nic_capability: NicCapability,
    pub candidate_channels: CandidateChannelSummary,
    pub eeprom_hardware: EepromBlock,
    pub clc: ClcDiscovery,
    pub clc_rules_applied: u16,
    pub special_unii_mask: u8,
}

fn loader_command<T: FirmwareLoaderTransport>(
    transport: &mut T,
    command: DownloadCommand,
) -> Result<FirmwareCommandCompletion, FirmwareLoaderFailure<T::Error>> {
    let sequence = next_loader_sequence(transport)?;
    let encoded =
        encode_download_command(command, sequence).map_err(FirmwareLoaderFailure::Command)?;
    transport
        .command(command, sequence, &encoded)
        .map_err(|source| FirmwareLoaderFailure::Transport {
            operation: FirmwareLoaderOperation::Command(command),
            source,
        })
}

fn next_loader_sequence<T: FirmwareLoaderTransport>(
    transport: &mut T,
) -> Result<u8, FirmwareLoaderFailure<T::Error>> {
    let sequence = transport.next_sequence();
    if sequence == 0 || sequence > 15 {
        Err(FirmwareLoaderFailure::Command(
            DownloadCommandError::InvalidSequence,
        ))
    } else {
        Ok(sequence)
    }
}

fn loader_set_clc<T: FirmwareLoaderTransport>(
    transport: &mut T,
    command: &ClcSetCommand,
) -> Result<Option<ClcSetResponse>, FirmwareLoaderFailure<T::Error>> {
    let sequence = next_loader_sequence(transport)?;
    let encoded =
        encode_clc_set_command(command, sequence).map_err(FirmwareLoaderFailure::Command)?;
    transport
        .set_clc(command, sequence, &encoded)
        .map_err(|source| FirmwareLoaderFailure::Transport {
            operation: FirmwareLoaderOperation::SetClc,
            source,
        })
}

fn loader_set_channel_domain<T: FirmwareLoaderTransport>(
    transport: &mut T,
    command: &ChannelDomainCommand,
) -> Result<(), FirmwareLoaderFailure<T::Error>> {
    let sequence = next_loader_sequence(transport)?;
    let encoded = encode_channel_domain_command(command, sequence)
        .map_err(FirmwareLoaderFailure::ChannelDomain)?;
    transport
        .set_channel_domain(command, sequence, &encoded)
        .map_err(|source| FirmwareLoaderFailure::Transport {
            operation: FirmwareLoaderOperation::SetChannelDomain,
            source,
        })
}

fn loader_scatter<T: FirmwareLoaderTransport>(
    transport: &mut T,
    part: FirmwareImagePart,
    payload: &[u8],
    report: &mut FirmwareLoaderReport,
) -> Result<(), FirmwareLoaderFailure<T::Error>> {
    for chunk in payload.chunks(MT7921_FWDL_CHUNK_BYTES) {
        let sequence = next_loader_sequence(transport)?;
        transport
            .publish_scatter(part, sequence, chunk)
            .map_err(|source| FirmwareLoaderFailure::Transport {
                operation: FirmwareLoaderOperation::PublishScatter(part),
                source,
            })?;
        let deadline_ms = transport
            .now_ms()
            .saturating_add(SCATTER_COMPLETION_TIMEOUT_MS);
        transport
            .wait_scatter_completion(part, sequence, deadline_ms)
            .map_err(|source| FirmwareLoaderFailure::Transport {
                operation: FirmwareLoaderOperation::WaitScatterCompletion(part),
                source,
            })?;
        report.scatter_chunks += 1;
        report.scatter_bytes += chunk.len();
    }
    Ok(())
}

fn expect_loader_completion<E>(
    command: DownloadCommand,
    completion: FirmwareCommandCompletion,
    expected: FirmwareCommandCompletion,
) -> Result<(), FirmwareLoaderFailure<E>> {
    if completion == expected {
        Ok(())
    } else {
        Err(FirmwareLoaderFailure::UnexpectedCommandCompletion {
            command,
            completion,
        })
    }
}

fn run_firmware_loader<T: FirmwareLoaderTransport>(
    transport: &mut T,
    patch: Patch<'_>,
    firmware: Firmware<'_>,
    state: &mut FirmwareLoaderState,
    configure_channel_domain: bool,
    stop_after_capability: bool,
) -> Result<FirmwareLoaderReport, FirmwareLoaderFailure<T::Error>> {
    let mut report = FirmwareLoaderReport {
        download_ready_observed: false,
        patch: PatchDisposition::Downloaded,
        patch_sections: 0,
        ram_regions: 0,
        scatter_chunks: 0,
        scatter_bytes: 0,
        nic_capability: NicCapability {
            element_count: 0,
            mac_address: None,
            phy: None,
            has_6ghz: None,
            chip_capability: None,
            unknown_elements: 0,
        },
        candidate_channels: CandidateChannelSummary::default(),
        eeprom_hardware: EepromBlock {
            address: MT7921_EEPROM_HW_TYPE_BLOCK,
            valid: 0,
            data: [0; MT7921_EEPROM_BLOCK_SIZE],
        },
        clc: ClcDiscovery::default(),
        clc_rules_applied: 0,
        special_unii_mask: 0,
    };

    let power = DownloadCommand::NicPowerControl;
    let completion = loader_command(transport, power)?;
    expect_loader_completion(power, completion, FirmwareCommandCompletion::NoResponse)?;
    let download_deadline = transport.now_ms().saturating_add(DOWNLOAD_READY_TIMEOUT_MS);
    loop {
        let firmware_state = transport.firmware_download_state().map_err(|source| {
            FirmwareLoaderFailure::Transport {
                operation: FirmwareLoaderOperation::PollDownloadReady,
                source,
            }
        })?;
        if firmware_state == 1 {
            *state = FirmwareLoaderState::DownloadReady;
            report.download_ready_observed = true;
            break;
        }
        if transport.now_ms() >= download_deadline {
            // Pinned Linux warns and continues into patch semaphore handling.
            break;
        }
        transport.sleep_ms(FIRMWARE_POLL_INTERVAL_MS);
    }

    let get = DownloadCommand::PatchSemaphoreGet;
    match loader_command(transport, get)? {
        FirmwareCommandCompletion::PatchSemaphore(PatchSemaphoreStatus::AlreadyDownloaded) => {
            report.patch = PatchDisposition::AlreadyDownloaded
        }
        FirmwareCommandCompletion::PatchSemaphore(PatchSemaphoreStatus::Acquired) => {
            *state = FirmwareLoaderState::PatchSemaphoreHeld;
            let patch_result = (|| {
                for section in patch.sections() {
                    let mode = patch_download_mode(section.security_info)
                        .map_err(FirmwareLoaderFailure::PatchSecurity)?;
                    let command = DownloadCommand::PatchStart {
                        address: section.address,
                        length: section.payload.len() as u32,
                        mode,
                    };
                    let completion = loader_command(transport, command)?;
                    expect_loader_completion(command, completion, FirmwareCommandCompletion::Ack)?;
                    loader_scatter(
                        transport,
                        FirmwareImagePart::Patch,
                        section.payload,
                        &mut report,
                    )?;
                    report.patch_sections += 1;
                }
                let finish = DownloadCommand::PatchFinish;
                match loader_command(transport, finish)? {
                    FirmwareCommandCompletion::PatchFinish(0) => {}
                    FirmwareCommandCompletion::PatchFinish(status) => {
                        return Err(FirmwareLoaderFailure::UnexpectedPatchFinish(status));
                    }
                    completion => {
                        return Err(FirmwareLoaderFailure::UnexpectedCommandCompletion {
                            command: finish,
                            completion,
                        });
                    }
                }
                Ok(())
            })();
            let primary = patch_result.err();
            let release = loader_command(transport, DownloadCommand::PatchSemaphoreRelease);
            let release_failure = match release {
                Ok(FirmwareCommandCompletion::PatchSemaphore(PatchSemaphoreStatus::Released)) => {
                    None
                }
                Ok(FirmwareCommandCompletion::PatchSemaphore(result)) => {
                    Some(FirmwareLoaderFailure::UnexpectedPatchRelease(result))
                }
                Ok(completion) => Some(FirmwareLoaderFailure::UnexpectedCommandCompletion {
                    command: DownloadCommand::PatchSemaphoreRelease,
                    completion,
                }),
                Err(error) => Some(error),
            };
            if let Some(release) = release_failure {
                return Err(FirmwareLoaderFailure::PatchRelease {
                    primary: primary.map(Box::new),
                    release: Box::new(release),
                });
            }
            *state = FirmwareLoaderState::PatchSemaphoreReleased;
            if let Some(primary) = primary {
                return Err(primary);
            }
        }
        FirmwareCommandCompletion::PatchSemaphore(result) => {
            return Err(FirmwareLoaderFailure::UnexpectedPatchSemaphore(result));
        }
        completion => {
            return Err(FirmwareLoaderFailure::UnexpectedCommandCompletion {
                command: get,
                completion,
            });
        }
    }
    *state = FirmwareLoaderState::PatchComplete;

    let mut override_address = 0;
    *state = FirmwareLoaderState::RamDownloading;
    for region in firmware.regions() {
        if !region.is_downloadable() {
            continue;
        }
        if region.feature_set & (1 << 5) != 0 {
            override_address = region.address;
        }
        let command = DownloadCommand::TargetAddressLength {
            address: region.address,
            length: region.payload.len() as u32,
            mode: firmware_download_mode(region.feature_set, false),
        };
        let completion = loader_command(transport, command)?;
        expect_loader_completion(command, completion, FirmwareCommandCompletion::Ack)?;
        loader_scatter(
            transport,
            FirmwareImagePart::Ram,
            region.payload,
            &mut report,
        )?;
        report.ram_regions += 1;
    }
    if override_address == 0 {
        return Err(FirmwareLoaderFailure::MissingFirmwareOverride);
    }
    let start = DownloadCommand::FirmwareStart {
        address: override_address,
        option: 1,
    };
    let completion = loader_command(transport, start)?;
    expect_loader_completion(start, completion, FirmwareCommandCompletion::Ack)?;
    *state = FirmwareLoaderState::FirmwareStarted;
    let n9_deadline = transport.now_ms().saturating_add(N9_READY_TIMEOUT_MS);
    loop {
        if transport
            .firmware_n9_ready()
            .map_err(|source| FirmwareLoaderFailure::Transport {
                operation: FirmwareLoaderOperation::PollN9Ready,
                source,
            })?
        {
            *state = FirmwareLoaderState::N9Ready;
            break;
        }
        if transport.now_ms() >= n9_deadline {
            return Err(FirmwareLoaderFailure::N9ReadyTimeout);
        }
        transport.sleep_ms(FIRMWARE_POLL_INTERVAL_MS);
    }
    let capability_command = DownloadCommand::GetNicCapability;
    match loader_command(transport, capability_command)? {
        FirmwareCommandCompletion::NicCapability(capability) => {
            report.nic_capability = capability;
            report.candidate_channels = candidate_channel_summary(capability);
            *state = FirmwareLoaderState::CapabilityDiscovered;
        }
        completion => {
            return Err(FirmwareLoaderFailure::UnexpectedCommandCompletion {
                command: capability_command,
                completion,
            });
        }
    }
    if stop_after_capability {
        *state = FirmwareLoaderState::Ready;
        return Ok(report);
    }
    let eeprom_command = DownloadCommand::ReadEepromBlock {
        address: MT7921_EEPROM_HW_TYPE_BLOCK,
    };
    match loader_command(transport, eeprom_command)? {
        FirmwareCommandCompletion::EepromBlock(block) => {
            report.eeprom_hardware = block;
            *state = FirmwareLoaderState::EepromDiscovered;
            report.clc = discover_clc(
                firmware,
                block
                    .hardware_info()
                    .expect("the fixed EEPROM hardware block was validated"),
            )
            .map_err(FirmwareLoaderFailure::Clc)?;
            let chip_capability = report.nic_capability.chip_capability.unwrap_or(0);
            let commands = world_clc_commands(
                firmware,
                block
                    .hardware_info()
                    .expect("the fixed EEPROM hardware block was validated"),
                chip_capability,
                transport.acpi_configuration(),
            )
            .map_err(FirmwareLoaderFailure::Clc)?;
            *state = FirmwareLoaderState::ClcConfigured;
            for command in &commands {
                if let Some(response) = loader_set_clc(transport, command)? {
                    report.special_unii_mask = response.special_unii_mask;
                }
                report.clc_rules_applied = report
                    .clc_rules_applied
                    .checked_add(1)
                    .ok_or(FirmwareLoaderFailure::Clc(ClcDiscoveryError::CountOverflow))?;
            }
            if configure_channel_domain {
                let command = conservative_channel_domain(
                    report.nic_capability,
                    *b"00",
                    true,
                    report.special_unii_mask,
                )
                .map_err(FirmwareLoaderFailure::ChannelDomain)?;
                loader_set_channel_domain(transport, &command)?;
                *state = FirmwareLoaderState::ChannelDomainConfigured;
            }
            *state = FirmwareLoaderState::Ready;
            Ok(report)
        }
        completion => Err(FirmwareLoaderFailure::UnexpectedCommandCompletion {
            command: eeprom_command,
            completion,
        }),
    }
}

/// Execute the bounded Linux MT7921 patch + RAM loading sequence. Cleanup is
/// mandatory after both success and failure; release of an acquired patch
/// semaphore is always attempted before fail-closed cleanup.
pub fn load_mt7921_firmware<T: FirmwareLoaderTransport>(
    transport: &mut T,
    patch: Patch<'_>,
    firmware: Firmware<'_>,
) -> Result<FirmwareLoaderReport, FirmwareLoaderError<T::Error>> {
    let mut state = FirmwareLoaderState::Powering;
    let result = run_firmware_loader(transport, patch, firmware, &mut state, false, false);
    finish_firmware_loader(transport, state, result)
}

/// Execute through the separately gated source-exact SET_CHAN_DOMAIN boundary.
/// No channel tuning, radio enable, or scan command is issued.
pub fn load_mt7921_firmware_through_channel_domain<T: FirmwareLoaderTransport>(
    transport: &mut T,
    patch: Patch<'_>,
    firmware: Firmware<'_>,
) -> Result<FirmwareLoaderReport, FirmwareLoaderError<T::Error>> {
    let mut state = FirmwareLoaderState::Powering;
    let result = run_firmware_loader(transport, patch, firmware, &mut state, true, false);
    finish_firmware_loader(transport, state, result)
}

/// Execute channel-domain setup, then one caller-owned bounded passive hook
/// before the same mandatory cleanup transaction. The hook cannot bypass or
/// replace cleanup and its failure is preserved as a typed transport error.
pub fn load_mt7921_firmware_with_passive_boundary<T, F>(
    transport: &mut T,
    patch: Patch<'_>,
    firmware: Firmware<'_>,
    passive: F,
) -> Result<FirmwareLoaderReport, FirmwareLoaderError<T::Error>>
where
    T: FirmwareLoaderTransport,
    F: FnOnce(&mut T, &FirmwareLoaderReport) -> Result<(), T::Error>,
{
    let mut state = FirmwareLoaderState::Powering;
    let result = run_firmware_loader(transport, patch, firmware, &mut state, true, false).and_then(
        |report| {
            passive(transport, &report).map_err(|source| FirmwareLoaderFailure::Transport {
                operation: FirmwareLoaderOperation::PassiveBoundary,
                source,
            })?;
            Ok(report)
        },
    );
    finish_firmware_loader(transport, state, result)
}

/// Load and start the pinned patch and RAM firmware, prove N9 readiness, and
/// complete one bounded GET_NIC_CAPABILITY response. This deliberately stops
/// before the first EEPROM read, CLC/calibration, channel-domain, or radio
/// command and then runs the same mandatory fail-closed cleanup transaction.
pub fn load_mt7921_firmware_bootstrap<T: FirmwareLoaderTransport>(
    transport: &mut T,
    patch: Patch<'_>,
    firmware: Firmware<'_>,
) -> Result<FirmwareLoaderReport, FirmwareLoaderError<T::Error>> {
    let mut state = FirmwareLoaderState::Powering;
    let result = run_firmware_loader(transport, patch, firmware, &mut state, false, true);
    finish_firmware_loader(transport, state, result)
}

fn finish_firmware_loader<T: FirmwareLoaderTransport>(
    transport: &mut T,
    state: FirmwareLoaderState,
    result: Result<FirmwareLoaderReport, FirmwareLoaderFailure<T::Error>>,
) -> Result<FirmwareLoaderReport, FirmwareLoaderError<T::Error>> {
    match (result, transport.fail_closed_cleanup(state)) {
        (Ok(report), Ok(())) => Ok(report),
        (Err(failure), Ok(())) => Err(FirmwareLoaderError::Failed(failure)),
        (Ok(_), Err(source)) => Err(FirmwareLoaderError::Cleanup {
            failure: None,
            source,
        }),
        (Err(failure), Err(source)) => Err(FirmwareLoaderError::Cleanup {
            failure: Some(failure),
            source,
        }),
    }
}

pub const PINNED_DMA_QUIESCE_MS: u64 = 100;

pub trait PinnedDmaTeardownTransport {
    type Error;
    fn now_ms(&self) -> u64;
    fn mask_device_interrupts(&mut self) -> Result<(), Self::Error>;
    fn disable_dma(&mut self) -> Result<(), Self::Error>;
    fn read_global_config(&mut self) -> Result<u32, Self::Error>;
    fn sleep_ms(&mut self, milliseconds: u64);
    fn reset_vfio_device(&mut self) -> Result<(), Self::Error>;
    fn unmap_all(&mut self) -> Result<(), Self::Error>;
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PinnedDmaTeardownEvent {
    InterruptsMasked,
    DmaDisabled,
    DmaQuiesced,
    DmaBusyTimedOut { raw: u32 },
    DeviceReset,
    MappingsReleased,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum PinnedDmaTeardownError<E> {
    Cleanup(E),
    Reset(E),
    Unmap(E),
    BusyTimedOut(u32),
}

/// Revoke active DMA without ever exposing an unmapped IOVA to the device.
///
/// A function reset is mandatory while every mapping remains pinned, even if
/// TX busy clears normally. If busy never clears, reset is the only transition
/// which permits unmapping. A failed reset returns without calling `unmap_all`,
/// leaving the process and external reboot watchdog as the final containment.
pub fn teardown_pinned_dma<T, F>(
    transport: &mut T,
    mut event: F,
) -> Result<(), PinnedDmaTeardownError<T::Error>>
where
    T: PinnedDmaTeardownTransport,
    F: FnMut(PinnedDmaTeardownEvent),
{
    let mut cleanup_error = None;
    match transport.mask_device_interrupts() {
        Ok(()) => event(PinnedDmaTeardownEvent::InterruptsMasked),
        Err(error) => cleanup_error = Some(error),
    }
    match transport.disable_dma() {
        Ok(()) => event(PinnedDmaTeardownEvent::DmaDisabled),
        Err(error) if cleanup_error.is_none() => cleanup_error = Some(error),
        Err(_) => {}
    }
    let deadline = transport.now_ms().saturating_add(PINNED_DMA_QUIESCE_MS);
    let mut busy_timeout = None;
    loop {
        match transport.read_global_config() {
            Ok(raw) if raw & ((1 << 1) | (1 << 3)) == 0 => {
                event(PinnedDmaTeardownEvent::DmaQuiesced);
                break;
            }
            Ok(raw) if transport.now_ms() >= deadline => {
                busy_timeout = Some(raw);
                event(PinnedDmaTeardownEvent::DmaBusyTimedOut { raw });
                break;
            }
            Ok(_) => transport.sleep_ms(1),
            Err(error) => {
                if cleanup_error.is_none() {
                    cleanup_error = Some(error);
                }
                break;
            }
        }
    }
    transport
        .reset_vfio_device()
        .map_err(PinnedDmaTeardownError::Reset)?;
    event(PinnedDmaTeardownEvent::DeviceReset);
    transport
        .unmap_all()
        .map_err(PinnedDmaTeardownError::Unmap)?;
    event(PinnedDmaTeardownEvent::MappingsReleased);
    if let Some(error) = cleanup_error {
        return Err(PinnedDmaTeardownError::Cleanup(error));
    }
    if let Some(raw) = busy_timeout {
        return Err(PinnedDmaTeardownError::BusyTimedOut(raw));
    }
    Ok(())
}

pub const WFSYS_SW_RST_B: u32 = 1 << 0;
pub const WFSYS_SW_INIT_DONE: u32 = 1 << 4;
pub const WFSYS_ASSERT_MS: u64 = 50;
pub const WFSYS_READY_DEADLINE_MS: u64 = 500;

pub trait WfsysResetTransport {
    type Error;
    fn now_ms(&self) -> u64;
    fn read_reset_control(&mut self) -> Result<u32, Self::Error>;
    fn write_reset_control(&mut self, value: u32) -> Result<(), Self::Error>;
    fn sleep_ms(&mut self, milliseconds: u64);
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum WfsysResetEvent {
    Snapshot { raw: u32 },
    AssertBefore { raw: u32 },
    Asserted { raw: u32, at_ms: u64 },
    ReleaseBefore { raw: u32 },
    Released { raw: u32, at_ms: u64 },
    StatusRead { raw: u32, at_ms: u64 },
    Ready { raw: u32, at_ms: u64 },
    TimedOut { raw: u32, at_ms: u64 },
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum WfsysResetError<E> {
    Transport(E),
    ClockOverflow,
    Timeout,
}

/// Port pinned Linux `mt792x_wfsys_reset` without its kernel runtime.
///
/// The concrete MT7921 target is physical address 0x18000140 and therefore
/// requires the same bounded dynamic-L1 mechanism before a physical adapter is
/// admitted. This portable state machine only owns the exact bit sequence and
/// deadlines; it cannot select or access any register itself.
pub fn reset_wfsys<T, F>(transport: &mut T, mut event: F) -> Result<(), WfsysResetError<T::Error>>
where
    T: WfsysResetTransport,
    F: FnMut(WfsysResetEvent),
{
    let start = transport.now_ms();
    let initial = transport
        .read_reset_control()
        .map_err(WfsysResetError::Transport)?;
    event(WfsysResetEvent::Snapshot { raw: initial });
    let asserted = initial & !WFSYS_SW_RST_B;
    event(WfsysResetEvent::AssertBefore { raw: asserted });
    transport
        .write_reset_control(asserted)
        .map_err(WfsysResetError::Transport)?;
    event(WfsysResetEvent::Asserted {
        raw: asserted,
        at_ms: 0,
    });
    transport.sleep_ms(WFSYS_ASSERT_MS);
    let released = asserted | WFSYS_SW_RST_B;
    event(WfsysResetEvent::ReleaseBefore { raw: released });
    transport
        .write_reset_control(released)
        .map_err(WfsysResetError::Transport)?;
    event(WfsysResetEvent::Released {
        raw: released,
        at_ms: transport.now_ms().saturating_sub(start),
    });
    let deadline = transport
        .now_ms()
        .checked_add(WFSYS_READY_DEADLINE_MS)
        .ok_or(WfsysResetError::ClockOverflow)?;
    loop {
        let raw = transport
            .read_reset_control()
            .map_err(WfsysResetError::Transport)?;
        let now = transport.now_ms();
        event(WfsysResetEvent::StatusRead {
            raw,
            at_ms: now.saturating_sub(start),
        });
        if raw & WFSYS_SW_INIT_DONE != 0 {
            event(WfsysResetEvent::Ready {
                raw,
                at_ms: now.saturating_sub(start),
            });
            return Ok(());
        }
        if now >= deadline {
            event(WfsysResetEvent::TimedOut {
                raw,
                at_ms: now.saturating_sub(start),
            });
            return Err(WfsysResetError::Timeout);
        }
        transport.sleep_ms(DRIVER_OWN_POLL_MS.min(deadline - now));
    }
}

pub trait IrqResetTransport: WfsysResetTransport {
    fn install_irq(&mut self, capability: PciIrqCapability) -> Result<(), Self::Error>;
    fn mask_host_irq(&mut self) -> Result<(), Self::Error>;
    fn enable_pcie_mac_irq(&mut self) -> Result<(), Self::Error>;
    fn disable_pcie_mac_irq(&mut self) -> Result<(), Self::Error>;
    fn disable_irq(&mut self) -> Result<(), Self::Error>;
    fn containment_reset(&mut self) -> Result<(), Self::Error>;
    fn verify_contained(&mut self) -> Result<(), Self::Error>;
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum IrqResetPrimaryError<E> {
    InvalidCapability,
    Install(E),
    Wfsys(WfsysResetError<E>),
    MaskHost(E),
    EnablePcieMac(E),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum IrqResetCleanupStep {
    DisablePcieMac,
    MaskHost,
    DisableIrq,
    ContainmentReset,
    VerifyContained,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct IrqResetError<E> {
    pub primary: Option<IrqResetPrimaryError<E>>,
    pub cleanup: Vec<(IrqResetCleanupStep, E)>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum IrqResetEvent {
    IrqInstallBefore { capability: PciIrqCapability },
    IrqInstalled { capability: PciIrqCapability },
    Wfsys(WfsysResetEvent),
    HostIrqMaskBefore,
    HostIrqMasked,
    PcieMacIrqEnableBefore,
    PcieMacIrqEnabled,
    SetupComplete,
    CleanupBefore { step: IrqResetCleanupStep },
    CleanupComplete { step: IrqResetCleanupStep },
}

/// Prepare the Linux-derived reset/IRQ boundary, then contain it before any
/// DMA mapping or firmware work. It preserves pinned Linux's reset, host-mask,
/// MAC-gate, then IRQ-install order; all cleanup steps are attempted even after
/// an ambiguous install error.
pub fn exercise_irq_reset_boundary<T, F>(
    transport: &mut T,
    capability: PciIrqCapability,
    mut event: F,
) -> Result<(), IrqResetError<T::Error>>
where
    T: IrqResetTransport,
    F: FnMut(IrqResetEvent),
{
    if capability.count == 0 || !capability.eventfd {
        return Err(IrqResetError {
            primary: Some(IrqResetPrimaryError::InvalidCapability),
            cleanup: Vec::new(),
        });
    }
    let primary = match reset_wfsys(transport, |item| event(IrqResetEvent::Wfsys(item))) {
        Err(error) => Some(IrqResetPrimaryError::Wfsys(error)),
        Ok(()) => {
            event(IrqResetEvent::HostIrqMaskBefore);
            match transport.mask_host_irq() {
                Err(error) => Some(IrqResetPrimaryError::MaskHost(error)),
                Ok(()) => {
                    event(IrqResetEvent::HostIrqMasked);
                    event(IrqResetEvent::PcieMacIrqEnableBefore);
                    match transport.enable_pcie_mac_irq() {
                        Err(error) => Some(IrqResetPrimaryError::EnablePcieMac(error)),
                        Ok(()) => {
                            event(IrqResetEvent::PcieMacIrqEnabled);
                            event(IrqResetEvent::IrqInstallBefore { capability });
                            match transport.install_irq(capability) {
                                Err(error) => Some(IrqResetPrimaryError::Install(error)),
                                Ok(()) => {
                                    event(IrqResetEvent::IrqInstalled { capability });
                                    event(IrqResetEvent::SetupComplete);
                                    None
                                }
                            }
                        }
                    }
                }
            }
        }
    };

    let mut cleanup = Vec::new();
    macro_rules! cleanup_step {
        ($step:expr, $operation:expr) => {{
            let step = $step;
            event(IrqResetEvent::CleanupBefore { step });
            match $operation {
                Ok(()) => event(IrqResetEvent::CleanupComplete { step }),
                Err(error) => cleanup.push((step, error)),
            }
        }};
    }
    cleanup_step!(IrqResetCleanupStep::MaskHost, transport.mask_host_irq());
    cleanup_step!(
        IrqResetCleanupStep::DisablePcieMac,
        transport.disable_pcie_mac_irq()
    );
    cleanup_step!(IrqResetCleanupStep::DisableIrq, transport.disable_irq());
    cleanup_step!(
        IrqResetCleanupStep::ContainmentReset,
        transport.containment_reset()
    );
    cleanup_step!(
        IrqResetCleanupStep::VerifyContained,
        transport.verify_contained()
    );
    if primary.is_none() && cleanup.is_empty() {
        Ok(())
    } else {
        Err(IrqResetError { primary, cleanup })
    }
}

impl ReadOnlyStatus {
    pub const fn decode(conn_misc: u32, low_power: u32, wfdma_config: u32) -> Self {
        Self {
            firmware_powered: conn_misc & 1 != 0,
            firmware_n9_ready: conn_misc & 3 == 3,
            firmware_owns_device: low_power & 4 != 0,
            tx_dma_enabled: wfdma_config & 1 != 0,
            tx_dma_busy: wfdma_config & 2 != 0,
            rx_dma_enabled: wfdma_config & 4 != 0,
            rx_dma_busy: wfdma_config & 8 != 0,
        }
    }
}

pub const MT7921_MGMT_TXWI_BYTES: usize = 64;

pub const MT7921_SKU_RATE_COUNT: usize = 161;
pub const MT7921_PSE_BASE: u32 = 0x820c_8000;

pub fn encode_pse_reg_read_command(sequence: u8) -> Result<Vec<u8>, RateTxPowerError> {
    if sequence == 0 || sequence > 15 {
        return Err(RateTxPowerError::InvalidSequence);
    }
    let total = CONNAC2_MCU_TXD_BYTES + 8;
    let mut bytes = vec![0u8; total];
    bytes[0..4].copy_from_slice(&((total as u32) | (2 << 23) | (0x20 << 25)).to_le_bytes());
    bytes[4..8].copy_from_slice(&((1u32 << 31) | (1 << 16)).to_le_bytes());
    bytes[32..34].copy_from_slice(&((total - 32) as u16).to_le_bytes());
    bytes[34..36].copy_from_slice(&0x8000u16.to_le_bytes());
    bytes[36..40].copy_from_slice(&[0xc0, 0xa0, 0, sequence]);
    bytes[CONNAC2_MCU_TXD_BYTES..CONNAC2_MCU_TXD_BYTES + 4]
        .copy_from_slice(&MT7921_PSE_BASE.to_le_bytes());
    Ok(bytes)
}

pub fn parse_pse_reg_read_response(
    event_id: u8,
    option: u8,
    bytes: &[u8],
) -> Result<u32, RateTxPowerError> {
    // Pinned mt76 names the legacy CE REG_READ response
    // MCU_EVENT_REG_ACCESS (0x05); MCU_EVENT_ACCESS_REG (0x02) is distinct.
    if event_id != 0x05 || option & (1 << 2) != 0 {
        return Err(RateTxPowerError::InvalidPseResponse);
    }
    let length = u16::from_le_bytes(
        bytes
            .get(24..26)
            .ok_or(RateTxPowerError::InvalidPseResponse)?
            .try_into()
            .expect("fixed field"),
    );
    if length != 20 || bytes.len() < 24 + usize::from(length) {
        return Err(RateTxPowerError::InvalidPseResponse);
    }
    let event = bytes
        .get(36..44)
        .ok_or(RateTxPowerError::InvalidPseResponse)?;
    if u32::from_le_bytes(event[..4].try_into().expect("fixed field")) != MT7921_PSE_BASE {
        return Err(RateTxPowerError::InvalidPseResponse);
    }
    Ok(u32::from_le_bytes(
        event[4..8].try_into().expect("fixed field"),
    ))
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ConservativePowerLimits {
    pub alpha2: [u8; 2],
    pub max_reg_power_dbm: u8,
    /// Minimum applicable SAR bound across every emitted static channel/rate.
    pub sar_limit_half_dbm: Option<i8>,
    /// Project-owned cap applied in addition to opaque, separately installed CLC policy.
    pub external_safety_cap_half_dbm: Option<i8>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RateTxPowerError {
    NonWorldDomain,
    MissingBandCapabilities,
    MissingLimit,
    InvalidRegulatoryLimit,
    InvalidSequence,
    Unsupported6Ghz,
    InvalidPseResponse,
}

pub trait RateTxPowerTransport {
    type Error;
    /// Return after DMA consumption. This CE command has no response payload.
    fn send_and_wait_consumed(&mut self, encoded: &[u8]) -> Result<(), Self::Error>;
    /// Mandatory pinned CE REG_READ query after every batch to prevent PSE underflow.
    fn query_pse_base(&mut self) -> Result<u32, Self::Error>;
}

#[derive(Debug, Eq, PartialEq)]
struct RateTxPowerSubmission {
    target_half_dbm: i8,
}

#[derive(Debug, Eq, PartialEq)]
pub struct RateTxPowerAuthorization {
    owner_id: NonZeroU64,
    generation: u64,
    alpha2: [u8; 2],
    target_half_dbm: i8,
}

#[derive(Debug, Eq, PartialEq)]
pub struct RateTxPowerAuthorizer {
    owner_id: Option<NonZeroU64>,
    generation: u64,
    alpha2: [u8; 2],
    authorization: Option<RateTxPowerAuthorization>,
}

impl RateTxPowerAuthorizer {
    pub const fn new() -> Self {
        Self {
            owner_id: None,
            generation: 0,
            alpha2: *b"00",
            authorization: None,
        }
    }

    pub fn submit<T: RateTxPowerTransport>(
        &mut self,
        transport: &mut T,
        capability: NicCapability,
        limits: ConservativePowerLimits,
        first_sequence: u8,
    ) -> Result<RateTxPowerAuthorization, RateTxPowerInstallError<T::Error>> {
        if limits.alpha2 != self.alpha2 || self.alpha2 != *b"00" {
            return Err(RateTxPowerInstallError::Encode(
                RateTxPowerError::NonWorldDomain,
            ));
        }
        let submission =
            submit_conservative_rate_tx_power(transport, capability, limits, first_sequence)?;
        let owner_id = *self
            .owner_id
            .get_or_insert_with(next_rate_power_authorizer_id);
        let authorization = RateTxPowerAuthorization {
            owner_id,
            generation: self.generation,
            alpha2: self.alpha2,
            target_half_dbm: submission.target_half_dbm,
        };
        self.authorization = Some(RateTxPowerAuthorization {
            owner_id: authorization.owner_id,
            generation: authorization.generation,
            alpha2: authorization.alpha2,
            target_half_dbm: authorization.target_half_dbm,
        });
        Ok(authorization)
    }

    pub fn set_regulatory_domain(&mut self, alpha2: [u8; 2]) {
        self.generation = self.generation.wrapping_add(1);
        self.alpha2 = alpha2;
        self.authorization = None;
    }

    pub fn reset(&mut self) {
        self.generation = self.generation.wrapping_add(1);
        self.authorization = None;
    }

    pub fn permits(&self, authorization: &RateTxPowerAuthorization) -> bool {
        self.authorization.as_ref() == Some(authorization)
            && self.owner_id == Some(authorization.owner_id)
            && authorization.generation == self.generation
            && authorization.alpha2 == self.alpha2
    }
}

fn next_rate_power_authorizer_id() -> NonZeroU64 {
    static NEXT_ID: AtomicU64 = AtomicU64::new(1);
    let id = NEXT_ID
        .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |id| id.checked_add(1))
        .expect("rate-power authorizer identity space exhausted");
    NonZeroU64::new(id).expect("rate-power authorizer identities start at one")
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum RateTxPowerInstallError<E> {
    Encode(RateTxPowerError),
    Transport { command: u8, error: E },
}

fn submit_conservative_rate_tx_power<T: RateTxPowerTransport>(
    transport: &mut T,
    capability: NicCapability,
    limits: ConservativePowerLimits,
    first_sequence: u8,
) -> Result<RateTxPowerSubmission, RateTxPowerInstallError<T::Error>> {
    let commands = encode_conservative_rate_tx_power_commands(capability, limits, first_sequence)
        .map_err(RateTxPowerInstallError::Encode)?;
    for (index, command) in commands.iter().enumerate() {
        transport.send_and_wait_consumed(command).map_err(|error| {
            RateTxPowerInstallError::Transport {
                command: index as u8,
                error,
            }
        })?;
        transport
            .query_pse_base()
            .map_err(|error| RateTxPowerInstallError::Transport {
                command: index as u8,
                error,
            })?;
    }
    Ok(RateTxPowerSubmission {
        target_half_dbm: (limits.max_reg_power_dbm as i8 * 2)
            .min(limits.sar_limit_half_dbm.expect("encoder required SAR"))
            .min(
                limits
                    .external_safety_cap_half_dbm
                    .expect("encoder required safety cap"),
            ),
    })
}

/// Encode pinned Connac2 `MCU_CE_CMD(SET_RATE_TX_POWER)` batches. This narrow
/// world-domain subset uses one most-restrictive limit for every rate, matching
/// Linux when no platform per-rate DT table expands the initialized target.
/// Both a platform/SAR bound and an explicit project safety cap are mandatory.
pub fn encode_conservative_rate_tx_power_commands(
    capability: NicCapability,
    limits: ConservativePowerLimits,
    first_sequence: u8,
) -> Result<Vec<Vec<u8>>, RateTxPowerError> {
    const CHANNELS_2GHZ: &[u8] = &[1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14];
    const CHANNELS_5GHZ: &[u8] = &[
        36, 38, 40, 42, 44, 46, 48, 50, 52, 54, 56, 58, 60, 62, 64, 100, 102, 104, 106, 108, 110,
        112, 114, 116, 118, 120, 122, 124, 126, 128, 132, 134, 136, 138, 140, 142, 144, 149, 151,
        153, 155, 157, 159, 161, 165, 169, 173, 177,
    ];
    if limits.alpha2 != *b"00" {
        return Err(RateTxPowerError::NonWorldDomain);
    }
    if limits.max_reg_power_dbm > 20 {
        return Err(RateTxPowerError::InvalidRegulatoryLimit);
    }
    let sar = limits
        .sar_limit_half_dbm
        .ok_or(RateTxPowerError::MissingLimit)?;
    let safety_cap = limits
        .external_safety_cap_half_dbm
        .ok_or(RateTxPowerError::MissingLimit)?;
    let target = (limits.max_reg_power_dbm as i8 * 2)
        .min(sar)
        .min(safety_cap);
    let phy = capability
        .phy
        .ok_or(RateTxPowerError::MissingBandCapabilities)?;
    if capability.has_6ghz != Some(false) {
        return Err(RateTxPowerError::Unsupported6Ghz);
    }
    let mut bands = Vec::new();
    // The pinned capability has no explicit has_2ghz bit; a present PHY always
    // contributes the baseline 2-GHz table, while has_5ghz gates that table.
    bands.push((1u8, CHANNELS_2GHZ));
    if phy.has_5ghz {
        bands.push((2u8, CHANNELS_5GHZ));
    }
    let command_count: usize = bands
        .iter()
        .map(|(_, channels)| channels.len().div_ceil(8))
        .sum();
    if first_sequence == 0 || usize::from(first_sequence) + command_count - 1 > 15 {
        return Err(RateTxPowerError::InvalidSequence);
    }
    let final_channel = bands
        .last()
        .and_then(|(_, channels)| channels.last())
        .copied();
    let final_band = bands.last().map(|(band, _)| *band);
    let mut commands = Vec::with_capacity(command_count);
    for (band, channels) in bands {
        for batch in channels.chunks(8) {
            let request_length = 44 + batch.len() * (1 + MT7921_SKU_RATE_COUNT);
            let total = CONNAC2_MCU_TXD_BYTES + request_length;
            let mut bytes = vec![0u8; total];
            bytes[0..4].copy_from_slice(&((total as u32) | (2 << 23) | (0x20 << 25)).to_le_bytes());
            bytes[4..8].copy_from_slice(&((1u32 << 31) | (1 << 16)).to_le_bytes());
            bytes[32..34].copy_from_slice(&((total - 32) as u16).to_le_bytes());
            bytes[34..36].copy_from_slice(&0x8000u16.to_le_bytes());
            bytes[36..40].copy_from_slice(&[0x5d, 0xa0, 1, first_sequence + commands.len() as u8]);
            let request = &mut bytes[CONNAC2_MCU_TXD_BYTES..];
            request[4] = batch.len() as u8;
            request[5] = band;
            request[6] =
                u8::from(Some(band) == final_band && batch.last().copied() == final_channel);
            request[8..10].copy_from_slice(&limits.alpha2);
            for (index, channel) in batch.iter().copied().enumerate() {
                let offset = 44 + index * (1 + MT7921_SKU_RATE_COUNT);
                request[offset] = channel;
                request[offset + 1..offset + 1 + MT7921_SKU_RATE_COUNT].fill(target as u8);
                if band == 2 {
                    request[offset + 1..offset + 5].fill(127);
                }
            }
            commands.push(bytes);
        }
    }
    Ok(commands)
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Mt7921MgmtTx {
    pub txwi: [u8; MT7921_MGMT_TXWI_BYTES],
    pub descriptor: DmaDescriptor,
    pub token: u16,
    pub pid: u8,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Mt7921MgmtMatrixCase {
    Reserved128,
    Reserved129,
    Reserved176,
    InvalidGroup20,
    Reserved128Repeat,
}

impl Mt7921MgmtMatrixCase {
    const ALL: [Self; 5] = [
        Self::Reserved128,
        Self::Reserved129,
        Self::Reserved176,
        Self::InvalidGroup20,
        Self::Reserved128Repeat,
    ];

    fn identity(self) -> (u16, u8, u16) {
        match self {
            Self::Reserved128 => (0, 3, 128),
            Self::Reserved129 => (1, 4, 129),
            Self::Reserved176 => (2, 5, 176),
            Self::InvalidGroup20 => (3, 6, 176),
            Self::Reserved128Repeat => (4, 7, 128),
        }
    }
}

#[derive(Debug, Eq, PartialEq)]
pub struct Mt7921MgmtMatrixFrame {
    pub case: Mt7921MgmtMatrixCase,
    pub token: u16,
    pub pid: u8,
    pub sequence_control: u16,
    bytes: Vec<u8>,
}

impl Mt7921MgmtMatrixFrame {
    pub fn bytes(&self) -> &[u8] {
        &self.bytes
    }
}

impl Drop for Mt7921MgmtMatrixFrame {
    fn drop(&mut self) {
        self.bytes.fill(0);
        core::sync::atomic::compiler_fence(core::sync::atomic::Ordering::SeqCst);
    }
}

/// Build synthetic directed Authentication MPDUs for the bounded management-TX
/// matrix. None can authenticate: four use a reserved algorithm and the fifth
/// has an invalid all-zero group-20 scalar and element.
pub fn mt7921_privacy_safe_mgmt_matrix(
    client: [u8; 6],
    bssid: [u8; 6],
) -> [Mt7921MgmtMatrixFrame; 5] {
    Mt7921MgmtMatrixCase::ALL.map(|case| {
        let (token, pid, frame_len) = case.identity();
        let sequence_control = token << 4;
        let mut bytes = vec![0u8; usize::from(frame_len)];
        bytes[0..2].copy_from_slice(&0x00b0u16.to_le_bytes());
        bytes[4..10].copy_from_slice(&bssid);
        bytes[10..16].copy_from_slice(&client);
        bytes[16..22].copy_from_slice(&bssid);
        bytes[22..24].copy_from_slice(&sequence_control.to_le_bytes());
        if case == Mt7921MgmtMatrixCase::InvalidGroup20 {
            bytes[24..26].copy_from_slice(&3u16.to_le_bytes());
            bytes[26..28].copy_from_slice(&1u16.to_le_bytes());
            bytes[28..30].copy_from_slice(&126u16.to_le_bytes());
            bytes[30..32].copy_from_slice(&20u16.to_le_bytes());
            // bytes 32..176 remain the deliberately invalid zero scalar/element.
        } else {
            bytes[24..26].copy_from_slice(&u16::MAX.to_le_bytes());
            bytes[26..28].copy_from_slice(&0u16.to_le_bytes());
            bytes[28..30].copy_from_slice(&u16::MAX.to_le_bytes());
            for (offset, byte) in bytes[30..].iter_mut().enumerate() {
                *byte = (offset as u8).wrapping_mul(61).wrapping_add(17);
            }
        }
        Mt7921MgmtMatrixFrame {
            case,
            token,
            pid,
            sequence_control,
            bytes,
        }
    })
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Mt7921MgmtMatrixCompletion {
    TxFree(Vec<u8>),
    TxStatus(Vec<u8>),
}

pub trait Mt7921MgmtMatrixTransport {
    type Error: core::fmt::Debug;

    fn read_dmashdl_control(&mut self) -> Result<u32, Self::Error>;
    fn publish_ring0(
        &mut self,
        case: Mt7921MgmtMatrixCase,
        frame: &[u8],
        encoded: &Mt7921MgmtTx,
    ) -> Result<(), Self::Error>;
    fn next_completion(&mut self) -> Result<Option<Mt7921MgmtMatrixCompletion>, Self::Error>;
    /// Must stop/reset ring 0 and wipe its TXWI and frame DMA arenas. The
    /// harness wipes its owned frame before invoking this boundary.
    fn reclaim_ring0(
        &mut self,
        case: Mt7921MgmtMatrixCase,
        wiped_frame: &[u8],
    ) -> Result<(), Self::Error>;
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Mt7921MgmtMatrixObservation {
    pub case: Mt7921MgmtMatrixCase,
    pub pre_dmashdl_control: u32,
    pub post_dmashdl_control: u32,
    pub raw_tx_free: Vec<u8>,
    pub raw_tx_status: Vec<u8>,
    pub tx_free_status: u8,
    pub attempts: u16,
    pub tx_status_ack_error: u8,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Mt7921MgmtMatrixError {
    Transport,
    DmashdlUnavailable,
    DmashdlBypassDisabled,
    MissingCompletion,
    DuplicateCompletion,
    UncorrelatedCompletion,
    InvalidCompletion,
    Reclaim,
}

fn raw_tx_free_status(bytes: &[u8]) -> Option<u8> {
    let reported_len = usize::from(u16::from_le_bytes(bytes.get(0..2)?.try_into().ok()?));
    let info_offset = match reported_len {
        12 => 8,
        16 => 12,
        _ => return None,
    };
    let info = u32::from_le_bytes(bytes.get(info_offset..info_offset + 4)?.try_into().ok()?);
    Some(((info >> 13) & 0x3) as u8)
}

fn raw_txs_ack_error(bytes: &[u8]) -> Option<u8> {
    let txs0 = u32::from_le_bytes(bytes.get(8..12)?.try_into().ok()?);
    Some(((txs0 >> 16) & 0x7) as u8)
}

/// Run A-E serially. Only TX completion packets are accepted; no received
/// Authentication frame can enter this harness or an authentication state
/// machine.
pub fn run_mt7921_privacy_safe_mgmt_matrix<T: Mt7921MgmtMatrixTransport>(
    transport: &mut T,
    client: [u8; 6],
    bssid: [u8; 6],
    txwi_iova: u64,
    frame_iova: u64,
) -> Result<Vec<Mt7921MgmtMatrixObservation>, Mt7921MgmtMatrixError> {
    let mut observations = Vec::with_capacity(5);
    for mut fixture in mt7921_privacy_safe_mgmt_matrix(client, bssid) {
        let pre = transport
            .read_dmashdl_control()
            .map_err(|_| Mt7921MgmtMatrixError::Transport)?;
        if pre == u32::MAX {
            return Err(Mt7921MgmtMatrixError::DmashdlUnavailable);
        }
        if pre & (1 << 28) == 0 {
            return Err(Mt7921MgmtMatrixError::DmashdlBypassDisabled);
        }
        let encoded = encode_mt7921_5ghz_auth_tx(
            &fixture.bytes,
            txwi_iova,
            frame_iova,
            fixture.token,
            fixture.pid,
            19,
        )
        .map_err(|_| Mt7921MgmtMatrixError::InvalidCompletion)?;

        let result = (|| {
            transport
                .publish_ring0(fixture.case, &fixture.bytes, &encoded)
                .map_err(|_| Mt7921MgmtMatrixError::Transport)?;
            let mut raw_free = None;
            let mut raw_status = None;
            while raw_free.is_none() || raw_status.is_none() {
                match transport
                    .next_completion()
                    .map_err(|_| Mt7921MgmtMatrixError::Transport)?
                    .ok_or(Mt7921MgmtMatrixError::MissingCompletion)?
                {
                    Mt7921MgmtMatrixCompletion::TxFree(raw) => {
                        if raw_free.is_some() {
                            return Err(Mt7921MgmtMatrixError::DuplicateCompletion);
                        }
                        let parsed = parse_mt7921_tx_free(&raw)
                            .map_err(|_| Mt7921MgmtMatrixError::InvalidCompletion)?;
                        if parsed.token != fixture.token
                            || parsed.wcid.is_some_and(|wcid| wcid != 19)
                        {
                            return Err(Mt7921MgmtMatrixError::UncorrelatedCompletion);
                        }
                        raw_free = Some((raw, parsed));
                    }
                    Mt7921MgmtMatrixCompletion::TxStatus(raw) => {
                        if raw_status.is_some() {
                            return Err(Mt7921MgmtMatrixError::DuplicateCompletion);
                        }
                        let parsed = parse_mt7921_tx_status(&raw)
                            .map_err(|_| Mt7921MgmtMatrixError::InvalidCompletion)?;
                        if parsed.pid != fixture.pid || parsed.wcid != 19 {
                            return Err(Mt7921MgmtMatrixError::UncorrelatedCompletion);
                        }
                        raw_status = Some(raw);
                    }
                }
            }
            let post = transport
                .read_dmashdl_control()
                .map_err(|_| Mt7921MgmtMatrixError::Transport)?;
            if post == u32::MAX {
                return Err(Mt7921MgmtMatrixError::DmashdlUnavailable);
            }
            if post & (1 << 28) == 0 {
                return Err(Mt7921MgmtMatrixError::DmashdlBypassDisabled);
            }
            let (raw_tx_free, parsed_free) = raw_free.expect("loop completed");
            let raw_tx_status = raw_status.expect("loop completed");
            Ok(Mt7921MgmtMatrixObservation {
                case: fixture.case,
                pre_dmashdl_control: pre,
                post_dmashdl_control: post,
                tx_free_status: raw_tx_free_status(&raw_tx_free)
                    .ok_or(Mt7921MgmtMatrixError::InvalidCompletion)?,
                attempts: parsed_free.attempts,
                tx_status_ack_error: raw_txs_ack_error(&raw_tx_status)
                    .ok_or(Mt7921MgmtMatrixError::InvalidCompletion)?,
                raw_tx_free,
                raw_tx_status,
            })
        })();
        fixture.bytes.fill(0);
        core::sync::atomic::compiler_fence(core::sync::atomic::Ordering::SeqCst);
        if transport
            .reclaim_ring0(fixture.case, &fixture.bytes)
            .is_err()
        {
            return Err(Mt7921MgmtMatrixError::Reclaim);
        }
        observations.push(result?);
    }
    Ok(observations)
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Mt7921MgmtTxError {
    InvalidFrame,
    InvalidIova,
    InvalidToken,
    InvalidPid,
    InvalidWcid,
    Descriptor(DescriptorError),
}

/// Encode the pinned Connac2 PCI TXWI + hardware TXP used by
/// `mt7921e_tx_prepare_skb` for one 5-GHz authentication frame. The raw frame
/// remains in a separate DMA mapping referenced by TXP; the WFDMA descriptor
/// publishes only the 64-byte TXWI/TXP buffer on band-0 ring 0.
pub fn encode_mt7921_5ghz_auth_tx(
    frame: &[u8],
    txwi_iova: u64,
    frame_iova: u64,
    token: u16,
    pid: u8,
    wcid: u16,
) -> Result<Mt7921MgmtTx, Mt7921MgmtTxError> {
    // mt76_connac_write_hw_txp masks each TXP buffer length with
    // MT_TXD_LEN_MASK (GENMASK(11, 0)); bit 15 is the independent LAST flag.
    if frame.len() < 30 || frame.len() > 0x0fff {
        return Err(Mt7921MgmtTxError::InvalidFrame);
    }
    let frame_control = u16::from_le_bytes([frame[0], frame[1]]);
    if frame_control != 0x00b0 {
        return Err(Mt7921MgmtTxError::InvalidFrame);
    }
    let fits_low32 = |iova: u64, len: usize| {
        len != 0
            && iova
                .checked_add(len as u64 - 1)
                .is_some_and(|end| end <= u64::from(u32::MAX))
    };
    if !fits_low32(txwi_iova, MT7921_MGMT_TXWI_BYTES) || !fits_low32(frame_iova, frame.len()) {
        return Err(Mt7921MgmtTxError::InvalidIova);
    }
    if token >= 8192 {
        return Err(Mt7921MgmtTxError::InvalidToken);
    }
    if !(3..127).contains(&pid) {
        return Err(Mt7921MgmtTxError::InvalidPid);
    }
    if wcid >= 20 {
        return Err(Mt7921MgmtTxError::InvalidWcid);
    }

    let mut txwi = [0u8; MT7921_MGMT_TXWI_BYTES];
    let mut word = |index: usize, value: u32| {
        txwi[index * 4..index * 4 + 4].copy_from_slice(&value.to_le_bytes())
    };
    // mt76_connac2_mac_write_txwi: CT packet, alternate TX queue, caller WCID / OMAC 0.
    word(0, (0x10 << 25) | ((frame.len() as u32 + 32) & 0xffff));
    // Long format, 802.11 header, 24-byte management header / 2.
    word(1, (1 << 31) | (2 << 16) | (12 << 11) | u32::from(wcid));
    // Authentication subtype, fixed legacy rate, and HTC-valid as in Linux.
    word(2, (1 << 31) | (1 << 13) | 0x0b);
    // 15 remaining attempts and BA disabled for fixed-rate management TX.
    word(3, (1 << 28) | (15 << 11));
    word(4, 0);
    word(5, (1 << 10) | u32::from(pid));
    // 5-GHz lowest basic rate: OFDM 6 Mbps (mode 1, hardware index 11).
    word(6, ((0x40u32 | 11) << 16) | (1 << 2));
    word(7, 0x0b << 16);
    drop(word);

    txwi[32..34].copy_from_slice(&(token | 0x8000).to_le_bytes());
    txwi[40..44].copy_from_slice(&(frame_iova as u32).to_le_bytes());
    txwi[44..46].copy_from_slice(&((frame.len() as u16) | 0x8000).to_le_bytes());
    let descriptor = mt7921_dma_tx(
        DmaSegment {
            iova: txwi_iova,
            len: MT7921_MGMT_TXWI_BYTES as u16,
        },
        None,
        0,
    )
    .map_err(Mt7921MgmtTxError::Descriptor)?;
    Ok(Mt7921MgmtTx {
        txwi,
        descriptor,
        token,
        pid,
    })
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Mt7921TxFree {
    pub wcid: Option<u16>,
    pub token: u16,
    pub dropped: bool,
    pub attempts: u16,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Mt7921TxStatus {
    pub wcid: u16,
    pub pid: u8,
    pub acked: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Mt7921TxCompletionError {
    Truncated,
    WrongPacketType,
    MultipleOrPaired,
    InvalidFormat,
}

pub fn mt7921_packet_type(bytes: &[u8]) -> Option<u8> {
    let header = u32::from_le_bytes(bytes.get(..4)?.try_into().ok()?);
    Some(((header >> 27) & 0x1f) as u8)
}

pub fn parse_mt7921_tx_free(bytes: &[u8]) -> Result<Mt7921TxFree, Mt7921TxCompletionError> {
    let header = u32::from_le_bytes(
        bytes
            .get(0..4)
            .ok_or(Mt7921TxCompletionError::Truncated)?
            .try_into()
            .expect("fixed field"),
    );
    if mt7921_packet_type(bytes) != Some(6) {
        return Err(Mt7921TxCompletionError::WrongPacketType);
    }
    let reported_len = (header & 0xffff) as usize;
    if reported_len != 12 && reported_len != 16 {
        return Err(Mt7921TxCompletionError::InvalidFormat);
    }
    let bytes = bytes
        .get(..reported_len)
        .ok_or(Mt7921TxCompletionError::Truncated)?;
    if header >> 16 & 0x03ff != 1 {
        return Err(Mt7921TxCompletionError::MultipleOrPaired);
    }
    let first = u32::from_le_bytes(
        bytes
            .get(8..12)
            .ok_or(Mt7921TxCompletionError::Truncated)?
            .try_into()
            .expect("fixed field"),
    );
    let (wcid, info) = if first & (1 << 31) != 0 {
        if reported_len != 16 {
            return Err(Mt7921TxCompletionError::InvalidFormat);
        }
        let info = u32::from_le_bytes(bytes[12..16].try_into().expect("fixed field"));
        if info & (1 << 31) != 0 {
            return Err(Mt7921TxCompletionError::InvalidFormat);
        }
        (Some(((first >> 14) & 0x03ff) as u16), info)
    } else {
        if reported_len != 12 {
            return Err(Mt7921TxCompletionError::InvalidFormat);
        }
        (None, first)
    };
    Ok(Mt7921TxFree {
        wcid,
        token: ((info >> 16) & 0x7fff) as u16,
        dropped: (info >> 13) & 0x3 != 0,
        attempts: (info & 0x1fff) as u16,
    })
}

pub fn parse_mt7921_tx_status(bytes: &[u8]) -> Result<Mt7921TxStatus, Mt7921TxCompletionError> {
    let header = u32::from_le_bytes(
        bytes
            .get(0..4)
            .ok_or(Mt7921TxCompletionError::Truncated)?
            .try_into()
            .expect("fixed field"),
    );
    if mt7921_packet_type(bytes) != Some(0) {
        return Err(Mt7921TxCompletionError::WrongPacketType);
    }
    let reported_len = (header & 0xffff) as usize;
    if reported_len != 40 {
        return Err(Mt7921TxCompletionError::InvalidFormat);
    }
    let bytes = bytes
        .get(..reported_len)
        .ok_or(Mt7921TxCompletionError::Truncated)?;
    let txs = bytes.get(8..40).ok_or(Mt7921TxCompletionError::Truncated)?;
    let dword = |index: usize| {
        u32::from_le_bytes(
            txs[index * 4..index * 4 + 4]
                .try_into()
                .expect("fixed field"),
        )
    };
    if dword(0) >> 23 & 0x3 > 1 {
        return Err(Mt7921TxCompletionError::InvalidFormat);
    }
    let wcid = ((dword(2) >> 16) & 0x03ff) as u16;
    if wcid >= 20 {
        return Err(Mt7921TxCompletionError::InvalidFormat);
    }
    Ok(Mt7921TxStatus {
        wcid,
        pid: (dword(3) >> 24) as u8,
        acked: dword(0) & (0x7 << 16) == 0,
    })
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Mt7921AuthRx {
    pub receiver: [u8; 6],
    pub transmitter: [u8; 6],
    pub bssid: [u8; 6],
    pub algorithm: u16,
    pub sequence: u16,
    pub status: u16,
    pub fields: Vec<u8>,
}

/// Strip the pinned Connac2 normal-RX metadata and parse one raw 802.11
/// authentication frame. Crypto and SAE interpretation remain Fuchsia-owned.
pub fn parse_mt7921_auth_rx(bytes: &[u8]) -> Result<Mt7921AuthRx, PassiveRxError> {
    let header = bytes.get(..24).ok_or(PassiveRxError::Truncated)?;
    let rxd0 = u32::from_le_bytes(header[0..4].try_into().expect("fixed field"));
    let reported_len = (rxd0 & 0xffff) as usize;
    let bytes = bytes.get(..reported_len).ok_or(PassiveRxError::Truncated)?;
    if reported_len < 24 {
        return Err(PassiveRxError::Truncated);
    }
    let rxd1 = u32::from_le_bytes(header[4..8].try_into().expect("fixed field"));
    let rxd2 = u32::from_le_bytes(header[8..12].try_into().expect("fixed field"));
    let packet_type = rxd0 >> 27 & 0x1f;
    let packet_flag = rxd0 >> 16 & 0x0f;
    if packet_type != 2 && !(packet_type == 7 && packet_flag == 1) {
        return Err(PassiveRxError::WrongPacketType);
    }
    // Match parse_connac2_rx_frame: HDR_TRANS_ERROR without HDR_TRANS still
    // carries a usable raw 802.11 authentication frame.
    if rxd1 & ((1 << 25) | (1 << 26) | (1 << 27)) != 0 || rxd2 & ((1 << 23) | (1 << 24)) != 0 {
        return Err(PassiveRxError::RxError);
    }
    if rxd2 & (1 << 13) != 0 {
        return Err(PassiveRxError::HeaderTranslated);
    }
    let mut offset = 24usize;
    if rxd1 & (1 << 14) != 0 {
        offset += 16;
    }
    if rxd1 & (1 << 11) != 0 {
        offset += 16;
    }
    if rxd1 & (1 << 12) != 0 {
        offset += 8;
    }
    if rxd1 & (1 << 13) == 0 {
        return Err(PassiveRxError::MissingRxVector);
    }
    offset += 8;
    if rxd1 & (1 << 15) != 0 {
        offset += 72;
    }
    offset += 2 * ((rxd2 >> 14) & 0x3) as usize;
    let frame = bytes.get(offset..).ok_or(PassiveRxError::Truncated)?;
    if frame.len() < 30 || u16::from_le_bytes([frame[0], frame[1]]) != 0x00b0 {
        return Err(PassiveRxError::UnsupportedFrame);
    }
    Ok(Mt7921AuthRx {
        receiver: frame[4..10].try_into().expect("fixed field"),
        transmitter: frame[10..16].try_into().expect("fixed field"),
        bssid: frame[16..22].try_into().expect("fixed field"),
        algorithm: u16::from_le_bytes([frame[24], frame[25]]),
        sequence: u16::from_le_bytes([frame[26], frame[27]]),
        status: u16::from_le_bytes([frame[28], frame[29]]),
        fields: frame[30..].to_vec(),
    })
}

#[allow(dead_code)]
pub fn encode_remove_wcid_command(
    sequence: u8,
    bss_index: u8,
    wcid: u8,
    aid: u16,
    peer: [u8; 6],
    negotiated_qos: bool,
) -> Result<Vec<u8>, String> {
    if !(1..=15).contains(&sequence) {
        return Err("WCID removal omitted valid sequence".into());
    }
    let mut body = vec![0; 40];
    body[0..8].copy_from_slice(&[bss_index, wcid, 2, 0, 1, 0, 0, 0]);
    body[8..12].copy_from_slice(&[0, 0, 20, 0]);
    body[12..16].copy_from_slice(&0x0001_0002u32.to_le_bytes());
    body[16] = 0;
    body[17] = u8::from(negotiated_qos);
    body[18..20].copy_from_slice(&aid.to_le_bytes());
    body[20..26].copy_from_slice(&peer);
    body[26..28].copy_from_slice(&1u16.to_le_bytes());
    body[28..32].copy_from_slice(&[13, 0, 12, 0]);
    body[32..40].copy_from_slice(&[wcid, 1, 0, 0, 0, 0, 0, 0]);

    let total = 48 + body.len();
    let mut bytes = vec![0; total];
    bytes[0..4].copy_from_slice(&((total as u32) | (2 << 23) | (0x20 << 25)).to_le_bytes());
    bytes[4..8].copy_from_slice(&((1u32 << 31) | (1 << 16)).to_le_bytes());
    bytes[32..34].copy_from_slice(&((total - 32) as u16).to_le_bytes());
    bytes[34..36].copy_from_slice(&3u16.to_le_bytes());
    bytes[37] = 0xa0;
    bytes[39] = sequence;
    bytes[43] = 0x07;
    bytes[48..].copy_from_slice(&body);
    Ok(bytes)
}

fn encode_legacy_wme_wcid_command(
    sequence: u8,
    bss_index: u8,
    wcid: u8,
    aid: u16,
    peer: [u8; 6],
    rcpi: u8,
    basic_rates: u16,
    legacy_rates: u16,
    associated: bool,
) -> Result<Vec<u8>, String> {
    if !(1..=15).contains(&sequence) {
        return Err("WCID add omitted valid sequence".into());
    }
    let mut bytes = vec![
        176, 0, 0, 65, 0, 0, 1, 128, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
        0, 0, 0, 144, 0, 3, 0, 0, 160, 0, sequence, 0, 0, 0, 7, 0, 0, 0, 0, bss_index, wcid, 5, 0,
        1, 0, 0, 0, 0, 0, 20, 0, 2, 0, 1, 0, 2, 1, 0, 0, 0, 0, 0, 0, 0, 0, 3, 0, 21, 0, 12, 0, 1,
        0, 8, 0, 0, rcpi, 0, 0, 1, 0, 16, 0, 64, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 7, 0, 12, 0, 0,
        0, 0, 0, 0, 0, 0, 0, 13, 0, 60, 0, wcid, 1, 4, 0, 0, 0, 0, 0, 0, 0, 20, 0, 0, 0, 0, 0, 0,
        0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 1, 0, 12, 0, 0, 1, 1, 1, 0, 0, 0, 0, 6, 0, 8, 0, 1, 0, 1,
        0, 13, 0, 8, 0, 1, 0, 1, 0,
    ];
    bytes[66..68].copy_from_slice(&aid.to_le_bytes());
    bytes[68..74].copy_from_slice(&peer);
    // mt7921_mac_sta_add publishes STATE_NONE with EXTRA_INFO_NEW before
    // authentication. mt7921_mac_sta_event updates that same WCID to
    // STATE_ASSOC after the association response.
    bytes[65] = u8::from(associated);
    bytes[74..76].copy_from_slice(&(if associated { 1u16 } else { 3u16 }).to_le_bytes());
    bytes[80..82].copy_from_slice(&basic_rates.to_le_bytes());
    bytes[92..94].copy_from_slice(&legacy_rates.to_le_bytes());
    bytes[112] = if associated { 2 } else { 0 };
    bytes[132..138].copy_from_slice(&peer);
    bytes[141] = u8::from(associated);
    bytes[144..146].copy_from_slice(&aid.to_le_bytes());
    Ok(bytes)
}

pub fn encode_preauth_peer_wcid_command(
    sequence: u8,
    bss_index: u8,
    wcid: u8,
    peer: [u8; 6],
    rcpi: u8,
) -> Result<Vec<u8>, String> {
    encode_legacy_wme_wcid_command(sequence, bss_index, wcid, 0, peer, rcpi, 1, 0x40, false)
}

pub fn encode_legacy_wme_add_wcid_command(
    sequence: u8,
    bss_index: u8,
    wcid: u8,
    aid: u16,
    peer: [u8; 6],
    rcpi: u8,
    basic_rates: u16,
    legacy_rates: u16,
) -> Result<Vec<u8>, String> {
    if !(1..=2007).contains(&aid) {
        return Err("associated WCID AID escaped infrastructure range".into());
    }
    if basic_rates == 0 || legacy_rates == 0 {
        return Err("associated WCID omitted negotiated legacy rates".into());
    }
    encode_legacy_wme_wcid_command(
        sequence,
        bss_index,
        wcid,
        aid,
        peer,
        rcpi,
        basic_rates,
        legacy_rates,
        true,
    )
}

/// Linux v7.1 mac80211 band-rate indexes translated into the exact Connac2
/// STA_REC_PHY.basic_rate and STA_REC_RA.legacy fields.
pub fn linux_legacy_rate_context_reference(
    band: u8,
    encoded_rates: &[u8],
) -> Result<(u16, u16), String> {
    let rate_values: &[u8] = match band {
        0 => &[2, 4, 11, 22, 12, 18, 24, 36, 48, 72, 96, 108],
        1 => &[12, 18, 24, 36, 48, 72, 96, 108],
        _ => return Err("legacy rate context used an unsupported band".into()),
    };
    let mut supported = 0u16;
    let mut basic = 0u16;
    for encoded in encoded_rates {
        let position = rate_values
            .iter()
            .position(|rate| *rate == encoded & 0x7f)
            .ok_or("association advertised a rate outside the Linux band table")?;
        supported |= 1 << position;
        if encoded & 0x80 != 0 {
            basic |= 1 << position;
        }
    }
    if basic == 0 || supported == 0 {
        return Err("association omitted negotiated basic/supported rates".into());
    }
    let legacy = if band == 0 {
        supported & 0x0f | (supported >> 4) << 6
    } else {
        supported << 6
    };
    Ok((basic, legacy))
}

/// Linux v7.1 `mt76_connac_mcu_uni_add_bss` station BASIC+QBSS request.
/// The BSS is programmed immediately before the associated WCID, matching
/// `mt7921_mac_sta_event(MT76_STA_EVENT_ASSOC)`.
pub fn encode_client_bss_command(
    sequence: u8,
    bss_index: u8,
    bssid: [u8; 6],
    channel: u16,
    beacon_interval: u16,
    qos: bool,
    enable: bool,
) -> Result<Vec<u8>, String> {
    if !(1..=15).contains(&sequence)
        || bssid == [0; 6]
        || beacon_interval == 0
        || !(1..=177).contains(&channel)
    {
        return Err("BSS update escaped station BASIC bounds".into());
    }
    let mut payload = vec![0; 48];
    payload[0] = bss_index;
    payload[4..6].copy_from_slice(&0u16.to_le_bytes());
    payload[6..8].copy_from_slice(&36u16.to_le_bytes());
    payload[8] = u8::from(enable);
    payload[12..16].copy_from_slice(&0x0001_0001u32.to_le_bytes());
    payload[16] = u8::from(!enable);
    payload[18..24].copy_from_slice(&bssid);
    payload[24..26].copy_from_slice(&19u16.to_le_bytes());
    payload[26..28].copy_from_slice(&beacon_interval.to_le_bytes());
    payload[28] = 1;
    payload[29] = if channel <= 14 { 0x4e } else { 0xb1 };
    payload[30..32].copy_from_slice(&19u16.to_le_bytes());
    payload[32..34].copy_from_slice(&(if channel <= 14 { 2u16 } else { 1u16 }).to_le_bytes());
    payload[40..42].copy_from_slice(&15u16.to_le_bytes());
    payload[42..44].copy_from_slice(&8u16.to_le_bytes());
    payload[44] = u8::from(qos);
    Ok(encode_uni_mcu(2, &payload, sequence))
}

pub struct SensitiveUniCommand(Vec<u8>);

impl SensitiveUniCommand {
    pub fn as_bytes(&self) -> &[u8] {
        &self.0
    }
}

impl Drop for SensitiveUniCommand {
    fn drop(&mut self) {
        for byte in &mut self.0 {
            unsafe { core::ptr::write_volatile(byte, 0) };
        }
        core::sync::atomic::compiler_fence(Ordering::SeqCst);
    }
}

pub fn encode_key_v2_command(
    sequence: u8,
    bss_index: u8,
    wcid: u8,
    muar_index: u8,
    key_id: u8,
    key: &[u8],
    retained_gtk: Option<(u8, &[u8])>,
) -> Result<SensitiveUniCommand, String> {
    if !(1..=15).contains(&sequence) || key.len() != 16 {
        return Err("key-v2 requires a valid sequence and 16-byte key".into());
    }
    if let Some((_, gtk)) = retained_gtk
        && gtk.len() != 16
    {
        return Err("IGTK update requires a retained 16-byte GTK".into());
    }
    let mut bytes = vec![0; 136];
    let txd0 = 136u32 | (2 << 23) | (0x20 << 25);
    bytes[0..4].copy_from_slice(&txd0.to_le_bytes());
    bytes[4..8].copy_from_slice(&((1u32 << 31) | (1 << 16)).to_le_bytes());
    bytes[32..34].copy_from_slice(&104u16.to_le_bytes());
    bytes[34..36].copy_from_slice(&3u16.to_le_bytes());
    bytes[37] = 0xa0;
    bytes[39] = sequence;
    bytes[43] = 7;
    bytes[48..56].copy_from_slice(&[bss_index, wcid, 1, 0, 1, muar_index, 0, 0]);
    bytes[56..58].copy_from_slice(&17u16.to_le_bytes());
    bytes[60] = 0;
    if let Some((gtk_id, gtk)) = retained_gtk {
        bytes[58..60].copy_from_slice(&80u16.to_le_bytes());
        bytes[61] = 2;
        bytes[64..68].copy_from_slice(&[5, 36, gtk_id, 16]);
        bytes[68..84].copy_from_slice(gtk);
        bytes[100..104].copy_from_slice(&[10, 36, 0, 16]);
        bytes[104..120].copy_from_slice(key);
    } else {
        bytes[58..60].copy_from_slice(&44u16.to_le_bytes());
        bytes[61] = 1;
        bytes[64..68].copy_from_slice(&[5, 36, key_id, 16]);
        bytes[68..84].copy_from_slice(key);
    }
    Ok(SensitiveUniCommand(bytes))
}

pub fn encode_disable_keys_command(
    sequence: u8,
    bss_index: u8,
    wcid: u8,
    muar_index: u8,
) -> Result<SensitiveUniCommand, String> {
    let mut command =
        encode_key_v2_command(sequence, bss_index, wcid, muar_index, 0, &[0; 16], None)?;
    let bytes = &mut command.0;
    bytes[58..60].copy_from_slice(&8u16.to_le_bytes());
    bytes[60] = 1;
    bytes[61..].fill(0);
    Ok(command)
}

pub fn encode_ptk_command(
    sequence: u8,
    bss_index: u8,
    peer_wcid: u8,
    key: &[u8],
) -> Result<SensitiveUniCommand, String> {
    encode_key_v2_command(sequence, bss_index, peer_wcid, 0, 0, key, None)
}

pub fn encode_gtk_command(
    sequence: u8,
    bss_index: u8,
    key_id: u8,
    key: &[u8],
) -> Result<SensitiveUniCommand, String> {
    if !(1..=3).contains(&key_id) {
        return Err("GTK key ID escaped group-key slots".into());
    }
    encode_key_v2_command(sequence, bss_index, 19, 0x0e, key_id, key, None)
}

pub fn encode_igtk_command(
    sequence: u8,
    bss_index: u8,
    igtk_id: u8,
    igtk: &[u8],
    gtk_id: u8,
    retained_gtk: &[u8],
) -> Result<SensitiveUniCommand, String> {
    if !(4..=5).contains(&igtk_id) || !(1..=3).contains(&gtk_id) {
        return Err("IGTK/GTK key ID escaped protected-management slots".into());
    }
    encode_key_v2_command(
        sequence,
        bss_index,
        19,
        0x0e,
        igtk_id,
        igtk,
        Some((gtk_id, retained_gtk)),
    )
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct LegacyWmeAssociation {
    pub bss_index: u8,
    pub peer_wcid: u8,
    pub aid: u16,
    pub peer: [u8; 6],
    pub rcpi: u8,
    /// mac80211 band-rate bitmap copied to STA_REC_PHY.basic_rate.
    pub basic_rates: u16,
    /// Connac RA_LEGACY_CCK/OFDM bitmap copied to STA_REC_RA.legacy.
    pub legacy_rates: u16,
    pub negotiated_qos: bool,
    pub mfp_required: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ClientEdcaAc {
    pub cw_min: u16,
    pub cw_max: u16,
    pub txop: u16,
    pub aifs: u16,
    pub acm: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ClientEdcaParameters {
    /// Linux/mac80211 order: VO, VI, BE, BK.
    pub ac: [ClientEdcaAc; 4],
}

/// Pinned Linux 7.1 `mt7921_mcu_set_tx` CE SET_EDCA_PARMS request.
pub fn encode_client_edca_command(
    sequence: u8,
    bss_index: u8,
    params: ClientEdcaParameters,
) -> Result<Vec<u8>, String> {
    if sequence == 0 || sequence > 15 || bss_index != 0 {
        return Err("client EDCA identity escaped the single station VIF".into());
    }
    if params.ac.iter().any(|ac| {
        ac.cw_min == 0
            || ac.cw_max < ac.cw_min
            || ac.aifs == 0
            || ac.aifs > 15
            || ac.cw_max > 0x7fff
    }) {
        return Err("client EDCA parameters escaped firmware bounds".into());
    }
    let mut payload = [0u8; 44];
    // Firmware slot order is the source-owned to_aci[] permutation.
    for (ac, slot) in [1usize, 0, 2, 3].into_iter().enumerate() {
        let value = params.ac[ac];
        let offset = slot * 10;
        payload[offset..offset + 2].copy_from_slice(&value.cw_min.to_le_bytes());
        payload[offset + 2..offset + 4].copy_from_slice(&value.cw_max.to_le_bytes());
        payload[offset + 4..offset + 6].copy_from_slice(&value.txop.to_le_bytes());
        payload[offset + 6..offset + 8].copy_from_slice(&value.aifs.to_le_bytes());
        payload[offset + 9] = u8::from(value.acm);
    }
    payload[40] = bss_index;
    payload[41] = 1;
    payload[42] = 0;
    Ok(encode_legacy_mcu(0x1d, 0, &payload, sequence))
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct JoinedClientBss {
    pub bssid: [u8; 6],
    pub channel: u16,
    pub channel_generation: u64,
    pub beacon_interval: u16,
}

/// The physical channel identity programmed by the mt7921 channel-switch
/// command. This mirrors Linux v7.1.5 `mt7921_mcu_set_chan_info`'s complete
/// chandef identity rather than duplicating a protocol-layer approximation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ClientPhysicalChannel {
    pub band: u8,
    pub primary: u16,
    pub center: u16,
    pub bandwidth: u8,
    pub center2: u16,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ClientChannelLease {
    pub channel: ClientPhysicalChannel,
    pub generation: u64,
}

/// One completed selector scan result retained for the externally selected BSS.
/// It is consumed exactly once when rate/power readiness authorizes SAE and is
/// thereafter bound to the physical channel generation used by join.
#[derive(Debug, Eq, PartialEq)]
pub struct ClientScanEvidence {
    pub scan_id: u64,
    pub observation_generation: u64,
    pub observation_timestamp_nanos: i64,
    pub bssid: [u8; 6],
    pub channel: ClientPhysicalChannel,
}

#[derive(Default)]
pub struct ClientTargetBssLease {
    retained: Option<ClientScanEvidence>,
    rate_power_ready: bool,
    authorized: Option<(ClientScanEvidence, ClientChannelLease)>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PreAssociationSaeAuth {
    pub transaction: u16,
    pub status: u16,
}

/// Normalize the raw two-byte AID field carried by a successful legacy
/// infrastructure association response. IEEE 802.11 reserves the top two bits
/// as ones on the wire; firmware receives only the bounded association ID.
pub fn normalize_infrastructure_aid(raw: u16) -> Result<u16, &'static str> {
    if raw & 0xc000 != 0xc000 {
        return Err("association response AID omitted reserved-bit form");
    }
    let aid = raw & 0x3fff;
    if !(1..=2007).contains(&aid) {
        return Err("association response AID escaped infrastructure range");
    }
    Ok(aid)
}

/// Safely classify only the fixed 802.11 Authentication envelope. SAE body
/// interpretation remains owned by Fuchsia MLME/SME.
pub fn classify_preassociation_sae_auth(
    frame: &[u8],
    client: [u8; 6],
    peer: [u8; 6],
) -> Result<Option<PreAssociationSaeAuth>, &'static str> {
    let control = frame
        .get(..2)
        .map(|bytes| u16::from_le_bytes([bytes[0], bytes[1]]))
        .ok_or("truncated frame control")?;
    if control & 0x00fc != 0x00b0 {
        return Ok(None);
    }
    if !(30..=2304).contains(&frame.len()) {
        return Err("authentication frame length is invalid");
    }
    if frame.get(4..10) != Some(&client)
        || frame.get(10..16) != Some(&peer)
        || frame.get(16..22) != Some(&peer)
    {
        return Err("authentication address tuple does not match selected BSS");
    }
    if frame.get(24..26) != Some(&3u16.to_le_bytes()) {
        return Err("pre-association authentication algorithm is not SAE");
    }
    let transaction = u16::from_le_bytes([frame[26], frame[27]]);
    if !(1..=2).contains(&transaction) {
        return Err("SAE authentication transaction is invalid");
    }
    let status = u16::from_le_bytes([frame[28], frame[29]]);
    let body = &frame[30..];
    if status == 77 && (body.is_empty() || body.len() % 2 != 0) {
        return Err("SAE rejected-groups body is malformed");
    }
    Ok(Some(PreAssociationSaeAuth {
        transaction,
        status,
    }))
}

impl ClientTargetBssLease {
    pub fn retain(evidence: ClientScanEvidence) -> Result<Self, String> {
        if evidence.scan_id == 0 || evidence.observation_generation == 0 {
            return Err("client selection evidence has no scan identity".into());
        }
        Ok(Self {
            retained: Some(evidence),
            rate_power_ready: false,
            authorized: None,
        })
    }

    pub fn mark_rate_power_ready(
        &mut self,
        bssid: [u8; 6],
        channel: ClientPhysicalChannel,
    ) -> Result<(), String> {
        let evidence = self
            .retained
            .as_ref()
            .ok_or("client selection evidence is absent or consumed")?;
        if evidence.bssid != bssid || evidence.channel != channel {
            return Err("rate-power readiness does not match selected BSS evidence".into());
        }
        self.rate_power_ready = true;
        Ok(())
    }

    pub fn authorize_sae(
        &mut self,
        bssid: [u8; 6],
        channel: ClientChannelLease,
    ) -> Result<ClientChannelLease, String> {
        let evidence = self
            .retained
            .as_ref()
            .ok_or("client selection evidence is absent or consumed")?;
        if !self.rate_power_ready || evidence.bssid != bssid || evidence.channel != channel.channel
        {
            return Err("SAE authorization does not match selected BSS readiness".into());
        }
        let evidence = self.retained.take().expect("retained evidence was checked");
        self.rate_power_ready = false;
        self.authorized = Some((evidence, channel));
        Ok(channel)
    }

    pub fn permits_join(&self, bssid: [u8; 6], channel: ClientChannelLease) -> bool {
        self.authorized
            .as_ref()
            .is_some_and(|(evidence, authorized)| evidence.bssid == bssid && *authorized == channel)
    }

    pub fn channel_changed(&mut self, channel: ClientPhysicalChannel) {
        let matches_retained = self
            .retained
            .as_ref()
            .is_some_and(|evidence| evidence.channel == channel);
        let matches_authorized = self
            .authorized
            .as_ref()
            .is_some_and(|(_, authorized)| authorized.channel == channel);
        if !matches_retained && !matches_authorized {
            self.invalidate();
        }
    }

    pub fn invalidate(&mut self) {
        self.retained = None;
        self.rate_power_ready = false;
        self.authorized = None;
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ClientPhysicalChannelEnsure {
    Current(ClientChannelLease),
    TransitionRequired {
        current: Option<ClientChannelLease>,
        requested: ClientPhysicalChannel,
    },
}

#[derive(Default)]
pub struct ClientChannelContext {
    current: Option<ClientChannelLease>,
    authorized_generation: Option<u64>,
    next_generation: u64,
}

impl ClientChannelContext {
    pub fn ensure_channel(&self, requested: ClientPhysicalChannel) -> ClientPhysicalChannelEnsure {
        match self.current {
            Some(current) if current.channel == requested => {
                ClientPhysicalChannelEnsure::Current(current)
            }
            current => ClientPhysicalChannelEnsure::TransitionRequired { current, requested },
        }
    }

    pub fn establish_channel(
        &mut self,
        channel: ClientPhysicalChannel,
    ) -> Result<ClientChannelLease, String> {
        if let ClientPhysicalChannelEnsure::Current(current) = self.ensure_channel(channel) {
            return Ok(current);
        }
        self.next_generation = self
            .next_generation
            .checked_add(1)
            .ok_or("client channel generation exhausted")?;
        let lease = ClientChannelLease {
            channel,
            generation: self.next_generation,
        };
        self.current = Some(lease);
        self.authorized_generation = None;
        Ok(lease)
    }

    pub fn authorize_channel(
        &mut self,
        channel: ClientPhysicalChannel,
    ) -> Result<ClientChannelLease, String> {
        let ClientPhysicalChannelEnsure::Current(current) = self.ensure_channel(channel) else {
            return Err("client channel authorization does not match physical context".into());
        };
        self.authorized_generation = Some(current.generation);
        Ok(current)
    }

    pub fn authorized_channel(&self) -> Result<ClientChannelLease, String> {
        let current = self.current.ok_or("client physical channel is absent")?;
        if self.authorized_generation != Some(current.generation) {
            return Err("client physical channel generation is not authorized".into());
        }
        Ok(current)
    }

    pub fn revoke_authorization(&mut self) {
        self.authorized_generation = None;
    }
}

pub struct RetainedGtk {
    id: u8,
    bytes: [u8; 16],
}

impl Drop for RetainedGtk {
    fn drop(&mut self) {
        for byte in &mut self.bytes {
            unsafe { core::ptr::write_volatile(byte, 0) };
        }
        core::sync::atomic::compiler_fence(Ordering::SeqCst);
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ClientDataGeneration {
    Association(u64),
    Authorized(u64),
}

#[derive(Clone, Copy)]
pub struct ClientRxCandidate {
    pub generation: ClientDataGeneration,
    pub eapol: bool,
    pub wcid: u16,
    pub tid: u8,
    pub group: bool,
    pub key_id: u8,
    pub security_mode: u8,
    pub cm: bool,
    pub clm: bool,
    pub icv_error: bool,
    pub mic_error: bool,
    pub fcs_error: bool,
    pub pn: [u8; 6],
}

pub fn encode_client_data_txwi(
    payload_len: usize,
    payload_iova: u64,
    token: u16,
    pid: u8,
    eapol: bool,
    protected: bool,
    qos: bool,
    tid: u8,
) -> Result<[u8; 64], String> {
    if payload_len == 0
        || payload_len > 0x0fff
        || payload_iova
            .checked_add(payload_len as u64 - 1)
            .is_none_or(|end| end > u64::from(u32::MAX))
        || token >= 8192
        || !(3..127).contains(&pid)
        || tid > 7
    {
        return Err("client data TX escaped TXWI/TXP bounds".into());
    }
    let mut bytes = [0u8; 64];
    let words = if eapol {
        let subtype = u32::from(qos) * 8;
        let descriptor_tid = if qos { tid } else { 0 };
        [
            0x0600_0000 | (payload_len as u32 + 32),
            0x8002_6007 | (u32::from(descriptor_tid) << 20) | (u32::from(qos) << 11),
            0x8000_2020 | subtype,
            0x1000_7800 | u32::from(protected) * 2,
            0,
            0x400 | u32::from(pid),
            0x004b_0004,
            0x0020_0000 | (subtype << 16),
        ]
    } else {
        if !protected {
            return Err("normal client data requires PTK protection".into());
        }
        [
            0x0200_0000 | (payload_len as u32 + 32),
            0x8000_8007,
            0x0000_0028,
            0x0000_7802,
            0,
            0x400 | u32::from(pid),
            0,
            0x0028_0000,
        ]
    };
    for (index, word) in words.into_iter().enumerate() {
        bytes[index * 4..index * 4 + 4].copy_from_slice(&word.to_le_bytes());
    }
    bytes[32..34].copy_from_slice(&(token | 0x8000).to_le_bytes());
    bytes[40..44].copy_from_slice(&(payload_iova as u32).to_le_bytes());
    bytes[44..46].copy_from_slice(&((payload_len as u16) | 0x8000).to_le_bytes());
    Ok(bytes)
}

/// Independent port of Linux v7.1's mac80211 control-port preparation and
/// `mt76_connac2_mac_write_txwi` for the one pre-key QoS EAPOL shape used by
/// the physical client.  This deliberately does not call the production
/// encoder: it is a diagnostic reference for the exact skb/tx_info path.
pub fn linux_qos_eapol_control_port_reference(
    mpdu: &[u8],
    payload_iova: u64,
    token: u16,
    pid: u8,
) -> Result<[u8; 64], String> {
    if mpdu.len() > 0x0fff
        || mpdu.len() < 38
        || payload_iova
            .checked_add(mpdu.len() as u64 - 1)
            .is_none_or(|end| end > u64::from(u32::MAX))
        || token >= 8192
        || !(3..127).contains(&pid)
    {
        return Err("Linux control-port reference escaped TXWI/TXP bounds".into());
    }

    let fc = u16::from_le_bytes(mpdu[0..2].try_into().unwrap());
    let qos = u16::from_le_bytes(mpdu[24..26].try_into().unwrap());
    // Normal STA To-DS QoS data, unprotected, unicast RA, TID 7, LLC EAPOL.
    // mac80211 has already written the per-STA/per-TID seq_ctrl in bytes
    // 22..24.  It does not set ASSIGN_SEQ for QoS data, and mt76 only emits
    // TXD3 SN_VALID for INJECTED frames.
    if fc != 0x0188
        || mpdu[4] & 1 != 0
        || qos & 15 != 7
        || mpdu.get(26..34) != Some(&[0xaa, 0xaa, 3, 0, 0, 0, 0x88, 0x8e])
    {
        return Err("Linux control-port reference requires a QoS EAPOL MPDU".into());
    }

    let mut bytes = [0u8; 64];
    let mut put = |index: usize, value: u32| {
        bytes[index * 4..index * 4 + 4].copy_from_slice(&value.to_le_bytes())
    };
    // skb queue mapping VO -> Connac LMAC qidx 3; normal qid is below PSD,
    // therefore this is CT rather than ALTX.  PORT_CTRL_PROTO implies
    // USE_MINRATE.  DONT_ENCRYPT leaves hw_key NULL before PTK installation.
    put(0, (3 << 25) | (mpdu.len() as u32 + 32));
    put(1, (1 << 31) | (7 << 20) | (2 << 16) | (13 << 11) | 7);
    put(2, (1 << 31) | (1 << 13) | (2 << 4) | 8);
    // BA disabled by the fixed-rate path, 15 remaining attempts. SN_VALID,
    // SEQ, protection and NO_ACK are intentionally clear.
    put(3, (1 << 28) | (15 << 11));
    put(4, 0);
    put(5, (1 << 10) | u32::from(pid));
    // Lowest 5-GHz basic rate: OFDM 6 Mbps. Fixed bandwidth; no LDPC/STBC.
    put(6, ((0x40u32 | 11) << 16) | (1 << 2));
    put(7, (2 << 20) | (8 << 16));
    drop(put);

    bytes[32..34].copy_from_slice(&(token | 0x8000).to_le_bytes());
    bytes[40..44].copy_from_slice(&(payload_iova as u32).to_le_bytes());
    bytes[44..46].copy_from_slice(&((mpdu.len() as u16) | 0x8000).to_le_bytes());
    Ok(bytes)
}

/// Independent Linux v7.1 `ieee80211_send_nullfunc` plus Connac2 TXWI
/// transcript for an awake (PM=0) QoS-null probe on the associated VO queue.
pub fn linux_qos_null_probe_reference(
    mpdu: &[u8],
    payload_iova: u64,
    token: u16,
    pid: u8,
) -> Result<[u8; 64], String> {
    if mpdu.len() != 26
        || u16::from_le_bytes(mpdu[0..2].try_into().unwrap()) != 0x01c8
        || mpdu[4] & 1 != 0
        || mpdu[24] & 15 != 7
    {
        return Err("Linux QoS-null reference requires an awake unicast TID7 probe".into());
    }
    let mut bytes = [0u8; 64];
    let words = [
        0x0600_003a,
        0x8072_6807,
        0x8000_202c,
        0x1000_7800,
        0,
        0x400 | u32::from(pid),
        0x004b_0004,
        0x002c_0000,
    ];
    for (index, word) in words.into_iter().enumerate() {
        bytes[index * 4..index * 4 + 4].copy_from_slice(&word.to_le_bytes());
    }
    if payload_iova > u64::from(u32::MAX) || token >= 8192 || !(3..127).contains(&pid) {
        return Err("Linux QoS-null reference escaped TXWI/TXP bounds".into());
    }
    bytes[32..34].copy_from_slice(&(token | 0x8000).to_le_bytes());
    bytes[40..44].copy_from_slice(&(payload_iova as u32).to_le_bytes());
    bytes[44..46].copy_from_slice(&0x801au16.to_le_bytes());
    Ok(bytes)
}

pub fn encode_client_management_tx(
    frame: &[u8],
    txwi_iova: u64,
    frame_iova: u64,
    token: u16,
    pid: u8,
) -> Result<Mt7921MgmtTx, String> {
    let control = frame
        .get(..2)
        .map(|bytes| u16::from_le_bytes([bytes[0], bytes[1]]))
        .ok_or("management frame omitted control")?;
    if control & 0x000c != 0 || frame.len() < 30 {
        return Err("client management TX requires one complete management MPDU".into());
    }
    // The existing golden encoder owns the complete Linux TXWI/TXP envelope.
    // Management subtypes differ only in TXD2's frame-subtype nibble.
    let mut auth_shape = frame.to_vec();
    auth_shape[0..2].copy_from_slice(&0x00b0u16.to_le_bytes());
    let mut encoded =
        encode_mt7921_5ghz_auth_tx(&auth_shape, txwi_iova, frame_iova, token, pid, 19)
            .map_err(|error| format!("encode client management MPDU: {error:?}"))?;
    let mut txd2 = u32::from_le_bytes(encoded.txwi[8..12].try_into().unwrap());
    txd2 = (txd2 & !0xf) | u32::from((control >> 4) & 0xf);
    encoded.txwi[8..12].copy_from_slice(&txd2.to_le_bytes());
    // The auth-shaped padding is never published; TXP length is the original
    // MPDU length and the DMA payload arena contains only `frame`.
    encoded.txwi[40..44].copy_from_slice(&(frame_iova as u32).to_le_bytes());
    encoded.txwi[44..46].copy_from_slice(&((frame.len() as u16) | 0x8000).to_le_bytes());
    Ok(encoded)
}

#[derive(Default)]
pub struct ClientFirmwareEffectsState {
    pub joined: Option<JoinedClientBss>,
    pub bss_programmed: bool,
    bss_binding: Option<(u8, bool)>,
    pub preauth_peer: Option<LegacyWmeAssociation>,
    pub association: Option<LegacyWmeAssociation>,
    pub edca_programmed: Option<ClientEdcaParameters>,
    pub sequence: u8,
    pub ptk_installed: bool,
    pub ptk_dirty: bool,
    pub gtk: Option<RetainedGtk>,
    pub igtk_installed: bool,
    pub broadcast_keys_dirty: bool,
    pub controlled_port_open: bool,
    pub firmware_uncertain: bool,
    pub next_generation: u64,
    pub association_generation: Option<u64>,
    pub authorized_generation: Option<u64>,
    pub outstanding_tx: Vec<(u16, ClientDataGeneration)>,
    pub ptk_rx_pn: Option<[u64; 16]>,
    pub gtk_rx_pn: Option<(u8, [u64; 16])>,
}

impl ClientFirmwareEffectsState {
    pub fn program_edca(
        &mut self,
        params: ClientEdcaParameters,
        mut submit: impl FnMut(&[u8]) -> Result<(), String>,
    ) -> Result<(), String> {
        let association = self.association.ok_or("EDCA requires an ACKed association")?;
        if !association.negotiated_qos || self.edca_programmed.is_some() || self.firmware_uncertain {
            return Err("EDCA state is not clean negotiated QoS".into());
        }
        let command = encode_client_edca_command(
            self.next_sequence(),
            association.bss_index,
            params,
        )?;
        self.firmware_uncertain = true;
        submit(&command)?;
        self.edca_programmed = Some(params);
        self.firmware_uncertain = false;
        Ok(())
    }

    pub fn qos_tx_ready(&self) -> bool {
        self.association.is_some_and(|association| !association.negotiated_qos)
            || self.edca_programmed.is_some()
    }
    pub fn bind_join(
        &mut self,
        bssid: [u8; 6],
        channel: ClientChannelLease,
        beacon_interval: u16,
    ) -> Result<(), String> {
        if bssid == [0; 6]
            || beacon_interval == 0
            || self.preauth_peer.is_some()
            || self.association.is_some()
            || self.bss_programmed
            || self.firmware_uncertain
        {
            return Err("join target/channel is not current and clean".into());
        }
        let joined = JoinedClientBss {
            bssid,
            channel: channel.channel.primary,
            channel_generation: channel.generation,
            beacon_interval,
        };
        if self.joined.is_some_and(|current| current != joined) {
            return Err("join target changed without teardown".into());
        }
        self.joined = Some(joined);
        Ok(())
    }

    pub fn accepts_joined_management(&self, frame: &[u8], client: [u8; 6]) -> bool {
        let Some(joined) = self.joined else {
            return false;
        };
        let Some(control) = frame
            .get(..2)
            .map(|value| u16::from_le_bytes([value[0], value[1]]))
        else {
            return false;
        };
        let subtype = ((control >> 4) & 15) as u8;
        let receiver = frame.get(4..10);
        let selected_bss =
            frame.get(10..16) == Some(&joined.bssid) && frame.get(16..22) == Some(&joined.bssid);
        let client_directed = receiver == Some(&client);
        let bss_advertisement =
            matches!(subtype, 5 | 8) && receiver.is_some_and(|receiver| receiver[0] & 1 != 0);
        control & 0x000c == 0 && selected_bss && (client_directed || bss_advertisement)
    }

    fn mint_generation(&mut self) -> u64 {
        self.next_generation = self.next_generation.wrapping_add(1).max(1);
        self.next_generation
    }

    fn next_sequence(&mut self) -> u8 {
        self.sequence = self.sequence % 15 + 1;
        self.sequence
    }

    pub fn prepare_preauth_peer(
        &mut self,
        peer: LegacyWmeAssociation,
        channel: ClientChannelLease,
        mut submit: impl FnMut(u8, &[u8]) -> Result<(), String>,
    ) -> Result<(), String> {
        let joined = self
            .joined
            .filter(|joined| {
                joined.bssid == peer.peer
                    && joined.channel == channel.channel.primary
                    && joined.channel_generation == channel.generation
            })
            .ok_or("preauth peer requires the current joined channel generation")?;
        if peer.aid != 0
            || peer.negotiated_qos
            || peer.mfp_required
            || self.association.is_some()
            || self.bss_programmed
            || self.firmware_uncertain
        {
            return Err("preauth peer state is not clean".into());
        }
        if let Some(current) = self.preauth_peer {
            return if current == peer {
                Ok(())
            } else {
                Err("preauth peer changed without teardown".into())
            };
        }
        let command = encode_preauth_peer_wcid_command(
            self.next_sequence(),
            peer.bss_index,
            peer.peer_wcid,
            peer.peer,
            peer.rcpi,
        )?;
        self.firmware_uncertain = true;
        if let Err(error) = submit(3, &command) {
            let rollback = encode_remove_wcid_command(
                self.next_sequence(),
                peer.bss_index,
                peer.peer_wcid,
                0,
                joined.bssid,
                false,
            )
            .and_then(|command| submit(3, &command));
            self.firmware_uncertain = rollback.is_err();
            return Err(format!(
                "preauth WCID add failed: {error}; rollback_wcid={rollback:?}"
            ));
        }
        self.preauth_peer = Some(peer);
        self.firmware_uncertain = false;
        Ok(())
    }

    pub fn associate(
        &mut self,
        association: LegacyWmeAssociation,
        channel: ClientChannelLease,
        mut submit: impl FnMut(u8, &[u8]) -> Result<(), String>,
    ) -> Result<(), String> {
        let joined = self
            .joined
            .filter(|joined| {
                joined.bssid == association.peer
                    && joined.channel == channel.channel.primary
                    && joined.channel_generation == channel.generation
            })
            .ok_or("association requires the current joined channel generation")?;
        let preauth = self
            .preauth_peer
            .filter(|preauth| {
                preauth.bss_index == association.bss_index
                    && preauth.peer_wcid == association.peer_wcid
                    && preauth.peer == association.peer
                    && preauth.rcpi == association.rcpi
                    && preauth.aid == 0
            })
            .ok_or("association requires an ACKed preauth peer WCID")?;
        if self.association.is_some() || self.bss_programmed || self.firmware_uncertain {
            return Err("client firmware association state is not clean".into());
        }
        let bss = encode_client_bss_command(
            self.next_sequence(),
            association.bss_index,
            joined.bssid,
            joined.channel,
            joined.beacon_interval,
            association.negotiated_qos,
            true,
        )?;
        // Submission failure can be post-publication. Retain enough binding
        // state to remove the BSS later unless the immediate rollback is ACKed.
        self.bss_programmed = true;
        self.bss_binding = Some((association.bss_index, association.negotiated_qos));
        self.firmware_uncertain = true;
        if let Err(error) = submit(2, &bss) {
            let rollback_bss = encode_client_bss_command(
                self.next_sequence(),
                association.bss_index,
                joined.bssid,
                joined.channel,
                joined.beacon_interval,
                association.negotiated_qos,
                false,
            )
            .and_then(|command| submit(2, &command));
            if rollback_bss.is_ok() {
                self.bss_programmed = false;
                self.bss_binding = None;
                self.firmware_uncertain = false;
            }
            return Err(format!(
                "BSS add failed: {error}; rollback_bss={rollback_bss:?}"
            ));
        }
        self.firmware_uncertain = false;
        let command = encode_legacy_wme_add_wcid_command(
            self.next_sequence(),
            association.bss_index,
            association.peer_wcid,
            association.aid,
            association.peer,
            association.rcpi,
            association.basic_rates,
            association.legacy_rates,
        )?;
        if let Err(error) = submit(3, &command) {
            self.controlled_port_open = false;
            let rollback_wcid = encode_remove_wcid_command(
                self.next_sequence(),
                association.bss_index,
                association.peer_wcid,
                association.aid,
                association.peer,
                association.negotiated_qos,
            )
            .and_then(|command| submit(3, &command));
            let rollback_bss = encode_client_bss_command(
                self.next_sequence(),
                association.bss_index,
                joined.bssid,
                joined.channel,
                joined.beacon_interval,
                association.negotiated_qos,
                false,
            )
            .and_then(|command| submit(2, &command));
            self.bss_programmed = rollback_bss.is_err();
            if rollback_bss.is_ok() {
                self.bss_binding = None;
            }
            if rollback_wcid.is_ok() {
                self.preauth_peer = None;
            }
            self.firmware_uncertain = rollback_wcid.is_err() || rollback_bss.is_err();
            return Err(format!(
                "WCID add failed: {error}; rollback_wcid={rollback_wcid:?}; rollback_bss={rollback_bss:?}"
            ));
        }
        debug_assert_eq!(preauth.peer, association.peer);
        self.association = Some(association);
        self.association_generation = Some(self.mint_generation());
        Ok(())
    }

    pub fn install_ptk(
        &mut self,
        key: &[u8],
        rsc: u64,
        mut submit: impl FnMut(u8, &[u8]) -> Result<(), String>,
    ) -> Result<(), String> {
        let association = self.association.ok_or("PTK install requires WCID ACK")?;
        if rsc >> 48 != 0 {
            return Err("PTK RSC exceeds 48 bits".into());
        }
        let command = encode_ptk_command(
            self.next_sequence(),
            association.bss_index,
            association.peer_wcid,
            key,
        )?;
        // A timeout can hide a successful firmware install. Record the
        // target before publication so teardown cannot skip its disable.
        self.ptk_dirty = true;
        if let Err(error) = submit(3, command.as_bytes()) {
            self.controlled_port_open = false;
            self.firmware_uncertain = true;
            let rollback = self.teardown(&mut submit);
            return Err(format!(
                "PTK install failed: {error}; rollback={rollback:?}"
            ));
        }
        self.ptk_installed = true;
        self.ptk_rx_pn = Some([rsc; 16]);
        Ok(())
    }

    pub fn install_gtk(
        &mut self,
        key_id: u8,
        key: &[u8],
        rsc: u64,
        mut submit: impl FnMut(u8, &[u8]) -> Result<(), String>,
    ) -> Result<(), String> {
        let association = self.association.ok_or("GTK install requires WCID ACK")?;
        if rsc >> 48 != 0 {
            return Err("GTK RSC exceeds 48 bits".into());
        }
        let command = encode_gtk_command(self.next_sequence(), association.bss_index, key_id, key)?;
        self.broadcast_keys_dirty = true;
        if let Err(error) = submit(3, command.as_bytes()) {
            self.controlled_port_open = false;
            self.firmware_uncertain = true;
            let rollback = self.teardown(&mut submit);
            return Err(format!(
                "GTK install failed: {error}; rollback={rollback:?}"
            ));
        }
        self.gtk = Some(RetainedGtk {
            id: key_id,
            bytes: key.try_into().expect("encoder required 16 bytes"),
        });
        self.gtk_rx_pn = Some((key_id, [rsc; 16]));
        Ok(())
    }

    pub fn install_igtk(
        &mut self,
        key_id: u8,
        key: &[u8],
        mut submit: impl FnMut(u8, &[u8]) -> Result<(), String>,
    ) -> Result<(), String> {
        let association = self.association.ok_or("IGTK install requires WCID ACK")?;
        let sequence = self.next_sequence();
        let gtk = self
            .gtk
            .as_ref()
            .ok_or("IGTK install requires retained GTK ACK")?;
        let command = encode_igtk_command(
            sequence,
            association.bss_index,
            key_id,
            key,
            gtk.id,
            &gtk.bytes,
        )?;
        self.broadcast_keys_dirty = true;
        if let Err(error) = submit(3, command.as_bytes()) {
            self.controlled_port_open = false;
            self.firmware_uncertain = true;
            let rollback = self.teardown(&mut submit);
            return Err(format!(
                "IGTK install failed: {error}; rollback={rollback:?}"
            ));
        }
        self.igtk_installed = true;
        Ok(())
    }

    pub fn set_controlled_port(&mut self, open: bool) -> Result<(), String> {
        if !open {
            self.controlled_port_open = false;
            self.authorized_generation = None;
            return Ok(());
        }
        if self.controlled_port_open {
            return Ok(());
        }
        let association = self
            .association
            .ok_or("controlled port requires WCID ACK")?;
        if self.firmware_uncertain
            || !self.ptk_installed
            || self.gtk.is_none()
            || (association.mfp_required && !self.igtk_installed)
        {
            return Err("controlled port requires all mandatory key ACKs".into());
        }
        self.controlled_port_open = true;
        self.authorized_generation = Some(self.mint_generation());
        Ok(())
    }

    pub fn tx_generation(&self, eapol: bool) -> Result<ClientDataGeneration, String> {
        if eapol {
            self.association_generation
                .map(ClientDataGeneration::Association)
                .ok_or("EAPOL TX requires association generation".into())
        } else {
            self.authorized_generation
                .map(ClientDataGeneration::Authorized)
                .ok_or("data TX requires authorized generation".into())
        }
    }

    pub fn publish_tx(
        &mut self,
        token: u16,
        generation: ClientDataGeneration,
    ) -> Result<(), String> {
        if self.tx_generation(matches!(generation, ClientDataGeneration::Association(_)))?
            != generation
            || self.outstanding_tx.iter().any(|(used, _)| *used == token)
        {
            return Err("stale or duplicate client TX publication".into());
        }
        self.outstanding_tx.push((token, generation));
        Ok(())
    }

    pub fn complete_tx(&mut self, token: u16) -> Result<(), String> {
        let index = self
            .outstanding_tx
            .iter()
            .position(|(used, _)| *used == token)
            .ok_or("unknown client TX completion")?;
        self.outstanding_tx.swap_remove(index);
        Ok(())
    }

    pub fn deliver_rx(&mut self, rx: ClientRxCandidate) -> Result<(), String> {
        let pre_key_eapol = rx.eapol && rx.wcid == 1023 && rx.security_mode == 0;
        if self.tx_generation(rx.eapol)? != rx.generation
            || (rx.wcid != 7 && !pre_key_eapol)
            || rx.tid >= 16
        {
            return Err("stale or foreign client RX".into());
        }
        if rx.security_mode == 0 && rx.eapol {
            return Ok(());
        }
        if rx.security_mode != 4 || rx.cm || rx.clm || rx.icv_error || rx.mic_error || rx.fcs_error
        {
            return Err("client RX failed CCMP status".into());
        }
        let pn = u64::from_be_bytes([
            0, 0, rx.pn[0], rx.pn[1], rx.pn[2], rx.pn[3], rx.pn[4], rx.pn[5],
        ]);
        let retained = if rx.group {
            let (id, counters) = self.gtk_rx_pn.as_mut().ok_or("group RX lacks GTK ACK")?;
            if *id != rx.key_id {
                return Err("group RX key id is stale".into());
            }
            &mut counters[usize::from(rx.tid)]
        } else {
            if rx.key_id != 0 {
                return Err("pairwise RX key id is invalid".into());
            }
            &mut self.ptk_rx_pn.as_mut().ok_or("unicast RX lacks PTK ACK")?[usize::from(rx.tid)]
        };
        if pn <= *retained {
            return Err("client RX replayed PN".into());
        }
        *retained = pn;
        Ok(())
    }

    pub fn deliver_protected_management_rx(
        &mut self,
        rx: ClientRxCandidate,
    ) -> Result<(), String> {
        let association = self
            .association
            .ok_or("protected management RX lacks association")?;
        let generation = self
            .association_generation
            .map(ClientDataGeneration::Association)
            .ok_or("protected management RX lacks association generation")?;
        if rx.generation != generation
            || !association.mfp_required
            || !self.ptk_installed
            || self.firmware_uncertain
            || rx.eapol
            || rx.group
            || rx.wcid != u16::from(association.peer_wcid)
            || rx.tid >= 16
            || rx.key_id != 0
        {
            return Err("protected management RX lacks current pairwise PMF state".into());
        }
        if rx.security_mode != 4 || rx.cm || rx.clm || rx.icv_error || rx.mic_error || rx.fcs_error
        {
            return Err("protected management RX failed CCMP status".into());
        }
        let pn = u64::from_be_bytes([
            0, 0, rx.pn[0], rx.pn[1], rx.pn[2], rx.pn[3], rx.pn[4], rx.pn[5],
        ]);
        let retained = &mut self
            .ptk_rx_pn
            .as_mut()
            .ok_or("protected management RX lacks PTK replay state")?[usize::from(rx.tid)];
        if pn <= *retained {
            return Err("protected management RX replayed PN".into());
        }
        *retained = pn;
        Ok(())
    }

    pub fn teardown(
        &mut self,
        mut submit: impl FnMut(u8, &[u8]) -> Result<(), String>,
    ) -> Result<(), String> {
        self.controlled_port_open = false;
        self.edca_programmed = None;
        self.authorized_generation = None;
        if !self.outstanding_tx.is_empty() {
            self.firmware_uncertain = true;
            return Err("client TX must be contained before key teardown".into());
        }
        let Some(association) = self.association else {
            self.gtk = None;
            self.ptk_installed = false;
            self.ptk_dirty = false;
            self.igtk_installed = false;
            self.broadcast_keys_dirty = false;
            self.association_generation = None;
            self.ptk_rx_pn = None;
            self.gtk_rx_pn = None;
            if let Some(preauth) = self.preauth_peer {
                encode_remove_wcid_command(
                    self.next_sequence(),
                    preauth.bss_index,
                    preauth.peer_wcid,
                    0,
                    preauth.peer,
                    false,
                )
                .and_then(|command| submit(3, &command))
                .map_err(|error| {
                    self.firmware_uncertain = true;
                    format!("client firmware preauth WCID teardown failed: {error}")
                })?;
                self.preauth_peer = None;
            }
            if self.bss_programmed {
                let joined = self.joined.ok_or("programmed BSS lost its join binding")?;
                let (bss_index, negotiated_qos) = self
                    .bss_binding
                    .ok_or("programmed BSS lost its firmware binding")?;
                encode_client_bss_command(
                    self.next_sequence(),
                    bss_index,
                    joined.bssid,
                    joined.channel,
                    joined.beacon_interval,
                    negotiated_qos,
                    false,
                )
                .and_then(|command| submit(2, &command))
                .map_err(|error| {
                    self.firmware_uncertain = true;
                    format!("client firmware BSS teardown failed: {error}")
                })?;
                self.bss_programmed = false;
                self.bss_binding = None;
                self.joined = None;
                self.firmware_uncertain = false;
            }
            self.joined = None;
            self.firmware_uncertain = false;
            return Ok(());
        };
        if self.broadcast_keys_dirty {
            encode_disable_keys_command(self.next_sequence(), association.bss_index, 19, 0x0e)
                .and_then(|command| submit(3, command.as_bytes()))
                .map_err(|error| {
                    self.firmware_uncertain = true;
                    format!("client firmware broadcast-key teardown failed: {error}")
                })?;
            self.gtk = None;
            self.gtk_rx_pn = None;
            self.igtk_installed = false;
            self.broadcast_keys_dirty = false;
        }
        if self.ptk_dirty {
            encode_disable_keys_command(
                self.next_sequence(),
                association.bss_index,
                association.peer_wcid,
                0,
            )
            .and_then(|command| submit(3, command.as_bytes()))
            .map_err(|error| {
                self.firmware_uncertain = true;
                format!("client firmware pairwise-key teardown failed: {error}")
            })?;
            self.ptk_installed = false;
            self.ptk_rx_pn = None;
            self.ptk_dirty = false;
        }
        encode_remove_wcid_command(
            self.next_sequence(),
            association.bss_index,
            association.peer_wcid,
            association.aid,
            association.peer,
            association.negotiated_qos,
        )
        .and_then(|command| submit(3, &command))
        .map_err(|error| {
            self.firmware_uncertain = true;
            format!("client firmware WCID teardown failed: {error}")
        })?;
        let joined = self.joined.expect("association retained joined BSS");
        encode_client_bss_command(
            self.next_sequence(),
            association.bss_index,
            joined.bssid,
            joined.channel,
            joined.beacon_interval,
            association.negotiated_qos,
            false,
        )
        .and_then(|command| submit(2, &command))
        .map_err(|error| {
            self.firmware_uncertain = true;
            format!("client firmware BSS teardown failed: {error}")
        })?;
        self.bss_programmed = false;
        self.bss_binding = None;
        self.preauth_peer = None;
        self.association = None;
        self.joined = None;
        self.association_generation = None;
        self.firmware_uncertain = false;
        Ok(())
    }
}

mod active_authority {
    #[derive(Clone, Copy, Debug, Eq, PartialEq)]
    enum GenerationAxis {
        Device,
        Reset,
        Firmware,
        Ownership,
        Domain,
        Channel,
        Power,
        Scan,
        Target,
        Attempt,
    }

    impl GenerationAxis {
        const COUNT: usize = 10;

        const fn index(self) -> usize {
            self as usize
        }
    }

    #[derive(Clone, Copy, Debug, Eq, PartialEq)]
    enum AxisState {
        Current,
        Pending,
        Unknown,
    }

    #[derive(Clone, Copy, Debug, Eq, PartialEq)]
    enum AuthorityError {
        GenerationExhausted(GenerationAxis),
        InvalidTransition,
        StaleObservation,
        TerminalAlreadySelected,
    }

    #[derive(Clone, Copy, Debug, Eq, PartialEq)]
    enum Invalidation {
        Device,
        Reset,
        Firmware,
        Ownership,
        Domain,
        Channel,
        Power,
        Scan,
        Target,
    }

    #[derive(Clone, Copy)]
    struct InvalidationRule {
        cause: Invalidation,
        axes: &'static [GenerationAxis],
        clears: EvidenceMask,
    }

    #[derive(Clone, Copy)]
    struct EvidenceMask(u8);

    impl EvidenceMask {
        const DISCOVERY: Self = Self(1 << 0);
        const FINAL_OBSERVATION: Self = Self(1 << 1);
        const BEACON: Self = Self(1 << 2);
        const POWER: Self = Self(1 << 3);
        const LEASE: Self = Self(1 << 4);
        const ATTEMPT: Self = Self(1 << 5);
        const ALL: Self = Self(u8::MAX);

        const fn union(self, other: Self) -> Self {
            Self(self.0 | other.0)
        }

        const fn contains(self, other: Self) -> bool {
            self.0 & other.0 != 0
        }
    }

    const INVALIDATION_RULES: &[InvalidationRule] = &[
        InvalidationRule {
            cause: Invalidation::Device,
            axes: &[
                GenerationAxis::Device,
                GenerationAxis::Reset,
                GenerationAxis::Firmware,
                GenerationAxis::Ownership,
                GenerationAxis::Domain,
                GenerationAxis::Channel,
                GenerationAxis::Power,
                GenerationAxis::Scan,
                GenerationAxis::Target,
            ],
            clears: EvidenceMask::ALL,
        },
        InvalidationRule {
            cause: Invalidation::Reset,
            axes: &[
                GenerationAxis::Reset,
                GenerationAxis::Firmware,
                GenerationAxis::Ownership,
                GenerationAxis::Power,
                GenerationAxis::Scan,
                GenerationAxis::Target,
            ],
            clears: EvidenceMask::ALL,
        },
        InvalidationRule {
            cause: Invalidation::Firmware,
            axes: &[GenerationAxis::Firmware],
            clears: EvidenceMask::ALL,
        },
        InvalidationRule {
            cause: Invalidation::Ownership,
            axes: &[GenerationAxis::Ownership],
            clears: EvidenceMask::ALL,
        },
        InvalidationRule {
            cause: Invalidation::Domain,
            axes: &[
                GenerationAxis::Domain,
                GenerationAxis::Power,
                GenerationAxis::Scan,
                GenerationAxis::Target,
            ],
            clears: EvidenceMask::ALL,
        },
        InvalidationRule {
            cause: Invalidation::Channel,
            axes: &[
                GenerationAxis::Channel,
                GenerationAxis::Power,
                GenerationAxis::Scan,
            ],
            // TargetPending deliberately survives the retune that starts its
            // final verification lineage; observations do not.
            clears: EvidenceMask::FINAL_OBSERVATION
                .union(EvidenceMask::BEACON)
                .union(EvidenceMask::POWER)
                .union(EvidenceMask::LEASE)
                .union(EvidenceMask::ATTEMPT),
        },
        InvalidationRule {
            cause: Invalidation::Power,
            axes: &[GenerationAxis::Power],
            clears: EvidenceMask::POWER
                .union(EvidenceMask::LEASE)
                .union(EvidenceMask::ATTEMPT),
        },
        InvalidationRule {
            cause: Invalidation::Scan,
            axes: &[GenerationAxis::Scan],
            clears: EvidenceMask::FINAL_OBSERVATION
                .union(EvidenceMask::BEACON)
                .union(EvidenceMask::LEASE)
                .union(EvidenceMask::ATTEMPT),
        },
        InvalidationRule {
            cause: Invalidation::Target,
            axes: &[GenerationAxis::Target],
            clears: EvidenceMask::FINAL_OBSERVATION
                .union(EvidenceMask::BEACON)
                .union(EvidenceMask::POWER)
                .union(EvidenceMask::LEASE)
                .union(EvidenceMask::ATTEMPT),
        },
    ];

    #[derive(Clone, Copy, Debug, Eq, PartialEq)]
    struct ObservationId(u64);

    #[derive(Clone, Copy, Debug, Eq, PartialEq)]
    struct TargetFingerprint(u64);

    #[derive(Clone, Copy, Debug, Eq, PartialEq)]
    struct Observation {
        id: ObservationId,
        fingerprint: TargetFingerprint,
        device: u64,
        reset: u64,
        firmware: u64,
        ownership: u64,
        domain: u64,
        channel: u64,
        scan: u64,
        target: Option<u64>,
        slot_epoch: u64,
        sealed: bool,
    }

    #[derive(Clone, Copy, Debug, Eq, PartialEq)]
    enum TargetState {
        None,
        Pending {
            generation: u64,
            discovery: ObservationId,
            fingerprint: TargetFingerprint,
        },
        Current {
            generation: u64,
            discovery: ObservationId,
            final_observation: ObservationId,
        },
        Unknown {
            generation: u64,
        },
    }

    #[derive(Clone, Copy, Debug, Eq, PartialEq)]
    enum AttemptState {
        None,
        Live {
            generation: u64,
        },
        Staged {
            generation: u64,
        },
        Committing {
            generation: u64,
            abort_requested: bool,
        },
        InFlight {
            generation: u64,
            abort_requested: bool,
        },
        Spent {
            generation: u64,
        },
        Revoked {
            generation: u64,
            may_have_transmitted: bool,
        },
    }

    #[derive(Clone, Copy, Debug, Eq, PartialEq)]
    enum TerminalResult {
        CancelledBeforePublish,
        Completed,
        MayHaveTransmitted,
        Contained,
    }

    #[derive(Clone, Copy, Debug, Eq, PartialEq)]
    enum CancellationDisposition {
        NoAttempt,
        CancelledBeforePublish,
        AbortInFlight,
        AlreadyTerminal,
    }

    #[derive(Clone, Copy, Debug, Eq, PartialEq)]
    enum ReleaseClassification {
        Released,
        HardwareSafeReleaseError,
        ParkUnsafe,
    }

    #[derive(Clone, Copy, Debug, Eq, PartialEq)]
    enum InvalidationStep {
        Revoked,
        Advanced(GenerationAxis),
        Pending(GenerationAxis),
        Faulted,
    }

    #[derive(Debug)]
    struct AuthorityModel {
        generations: [u64; GenerationAxis::COUNT],
        axis_states: [AxisState; GenerationAxis::COUNT],
        discovery: Option<Observation>,
        final_observation: Option<Observation>,
        beacon_evidence: bool,
        power_evidence: bool,
        lease: bool,
        target: TargetState,
        attempt: AttemptState,
        terminal: Option<TerminalResult>,
        next_observation: u64,
        faulted: bool,
        attempt_authorized: bool,
        invalidation_trace: [Option<InvalidationStep>; 24],
        invalidation_trace_len: usize,
    }

    impl AuthorityModel {
        #[cfg(test)]
        fn new_for_test() -> Self {
            Self {
                generations: [0; GenerationAxis::COUNT],
                axis_states: [AxisState::Current; GenerationAxis::COUNT],
                discovery: None,
                final_observation: None,
                beacon_evidence: false,
                power_evidence: false,
                lease: false,
                target: TargetState::None,
                attempt: AttemptState::None,
                terminal: None,
                next_observation: 1,
                faulted: false,
                attempt_authorized: false,
                invalidation_trace: [None; 24],
                invalidation_trace_len: 0,
            }
        }

        fn generation(&self, axis: GenerationAxis) -> u64 {
            self.generations[axis.index()]
        }

        fn advance(&mut self, axis: GenerationAxis) -> Result<u64, AuthorityError> {
            let Some(generation) = self.generations[axis.index()].checked_add(1) else {
                self.fault();
                return Err(AuthorityError::GenerationExhausted(axis));
            };
            self.generations[axis.index()] = generation;
            Ok(generation)
        }

        fn fault(&mut self) {
            self.clear(EvidenceMask::ALL);
            self.axis_states.fill(AxisState::Unknown);
            self.faulted = true;
            self.trace(InvalidationStep::Faulted);
        }

        fn trace(&mut self, step: InvalidationStep) {
            self.invalidation_trace[self.invalidation_trace_len] = Some(step);
            self.invalidation_trace_len += 1;
        }

        fn rule(cause: Invalidation) -> &'static InvalidationRule {
            INVALIDATION_RULES
                .iter()
                .find(|rule| rule.cause == cause)
                .expect("every invalidation has an explicit rule")
        }

        fn begin_invalidation(&mut self, cause: Invalidation) -> Result<(), AuthorityError> {
            if self.faulted {
                return Err(AuthorityError::InvalidTransition);
            }
            let rule = *Self::rule(cause);
            self.invalidation_trace.fill(None);
            self.invalidation_trace_len = 0;
            if let Some(axis) = rule
                .axes
                .iter()
                .copied()
                .find(|axis| self.generation(*axis) == u64::MAX)
            {
                self.fault();
                return Err(AuthorityError::GenerationExhausted(axis));
            }
            // Revocation is deliberately encoded before generation/state
            // mutation. Preflight above makes the remaining updates infallible.
            self.clear(rule.clears);
            self.trace(InvalidationStep::Revoked);
            for axis in rule.axes {
                self.advance(*axis)?;
                self.trace(InvalidationStep::Advanced(*axis));
                self.axis_states[axis.index()] = AxisState::Pending;
                self.trace(InvalidationStep::Pending(*axis));
            }
            Ok(())
        }

        fn confirm(&mut self, axis: GenerationAxis) -> Result<(), AuthorityError> {
            if self.faulted
                || axis == GenerationAxis::Target
                || self.axis_states[axis.index()] != AxisState::Pending
            {
                return Err(AuthorityError::InvalidTransition);
            }
            self.axis_states[axis.index()] = AxisState::Current;
            Ok(())
        }

        fn fail(&mut self, axis: GenerationAxis) -> Result<(), AuthorityError> {
            if self.faulted || self.axis_states[axis.index()] != AxisState::Pending {
                return Err(AuthorityError::InvalidTransition);
            }
            self.fault();
            Ok(())
        }

        fn clear(&mut self, mask: EvidenceMask) {
            if mask.contains(EvidenceMask::DISCOVERY) {
                self.discovery = None;
            }
            if mask.contains(EvidenceMask::FINAL_OBSERVATION) {
                self.final_observation = None;
            }
            if mask.contains(EvidenceMask::BEACON) {
                self.beacon_evidence = false;
            }
            if mask.contains(EvidenceMask::POWER) {
                self.power_evidence = false;
            }
            if mask.contains(EvidenceMask::LEASE) {
                self.lease = false;
                self.attempt_authorized = false;
            }
            if mask.contains(EvidenceMask::ATTEMPT) {
                self.revoke_attempt();
            }
            if mask.0 == EvidenceMask::ALL.0 {
                self.target = TargetState::None;
            }
        }

        fn current_for_observation(&self) -> bool {
            [
                GenerationAxis::Device,
                GenerationAxis::Reset,
                GenerationAxis::Firmware,
                GenerationAxis::Ownership,
                GenerationAxis::Domain,
                GenerationAxis::Channel,
                GenerationAxis::Scan,
            ]
            .into_iter()
            .all(|axis| self.axis_states[axis.index()] == AxisState::Current)
        }

        fn observation_is_current(&self, observation: Observation) -> bool {
            observation.device == self.generation(GenerationAxis::Device)
                && observation.reset == self.generation(GenerationAxis::Reset)
                && observation.firmware == self.generation(GenerationAxis::Firmware)
                && observation.ownership == self.generation(GenerationAxis::Ownership)
                && observation.domain == self.generation(GenerationAxis::Domain)
                && observation.channel == self.generation(GenerationAxis::Channel)
                && observation.scan == self.generation(GenerationAxis::Scan)
                && observation.slot_epoch == observation.scan
                && self.current_for_observation()
        }

        fn make_observation(
            &mut self,
            fingerprint: TargetFingerprint,
            target: Option<u64>,
            slot_epoch: u64,
        ) -> Result<Observation, AuthorityError> {
            if !self.current_for_observation()
                || slot_epoch != self.generation(GenerationAxis::Scan)
            {
                return Err(AuthorityError::StaleObservation);
            }
            let id = ObservationId(self.next_observation);
            let Some(next_observation) = self.next_observation.checked_add(1) else {
                self.fault();
                return Err(AuthorityError::GenerationExhausted(GenerationAxis::Scan));
            };
            self.next_observation = next_observation;
            Ok(Observation {
                id,
                fingerprint,
                device: self.generation(GenerationAxis::Device),
                reset: self.generation(GenerationAxis::Reset),
                firmware: self.generation(GenerationAxis::Firmware),
                ownership: self.generation(GenerationAxis::Ownership),
                domain: self.generation(GenerationAxis::Domain),
                channel: self.generation(GenerationAxis::Channel),
                scan: self.generation(GenerationAxis::Scan),
                target,
                slot_epoch,
                sealed: false,
            })
        }

        fn record_discovery(
            &mut self,
            fingerprint: TargetFingerprint,
        ) -> Result<ObservationId, AuthorityError> {
            let observation =
                self.make_observation(fingerprint, None, self.generation(GenerationAxis::Scan))?;
            self.discovery = Some(observation);
            Ok(observation.id)
        }

        fn begin_final_target(&mut self, discovery: ObservationId) -> Result<u64, AuthorityError> {
            let observation = self
                .discovery
                .filter(|observation| observation.id == discovery)
                .ok_or(AuthorityError::StaleObservation)?;
            self.begin_invalidation(Invalidation::Target)?;
            let generation = self.generation(GenerationAxis::Target);
            self.target = TargetState::Pending {
                generation,
                discovery,
                fingerprint: observation.fingerprint,
            };
            Ok(generation)
        }

        fn record_final_observation(
            &mut self,
            fingerprint: TargetFingerprint,
            slot_epoch: u64,
        ) -> Result<ObservationId, AuthorityError> {
            let (generation, discovery) = match self.target {
                TargetState::Pending {
                    generation,
                    discovery,
                    ..
                } => (generation, discovery),
                _ => return Err(AuthorityError::InvalidTransition),
            };
            let discovery_scan = self
                .discovery
                .filter(|observation| observation.id == discovery)
                .map(|observation| observation.scan)
                .ok_or(AuthorityError::StaleObservation)?;
            if !self.power_evidence || self.generation(GenerationAxis::Scan) <= discovery_scan {
                return Err(AuthorityError::InvalidTransition);
            }
            let observation = self.make_observation(fingerprint, Some(generation), slot_epoch)?;
            self.final_observation = Some(observation);
            Ok(observation.id)
        }

        fn seal_final_observation(
            &mut self,
            observation: ObservationId,
            matching_terminal: bool,
            ring_drained: bool,
            irq_drained: bool,
        ) -> Result<(), AuthorityError> {
            let current_scan = self.generation(GenerationAxis::Scan);
            let current_target = self.generation(GenerationAxis::Target);
            let candidate = self
                .final_observation
                .filter(|candidate| candidate.id == observation)
                .ok_or(AuthorityError::StaleObservation)?;
            if !matching_terminal
                || !ring_drained
                || !irq_drained
                || candidate.scan != current_scan
                || candidate.slot_epoch != current_scan
                || candidate.target != Some(current_target)
                || !self.observation_is_current(candidate)
            {
                return Err(AuthorityError::StaleObservation);
            }
            self.final_observation
                .as_mut()
                .filter(|candidate| candidate.id == observation)
                .expect("candidate was checked above")
                .sealed = true;
            Ok(())
        }

        fn matching_join(
            &mut self,
            final_observation: ObservationId,
        ) -> Result<(), AuthorityError> {
            let (generation, discovery, fingerprint) = match self.target {
                TargetState::Pending {
                    generation,
                    discovery,
                    fingerprint,
                } => (generation, discovery, fingerprint),
                _ => return Err(AuthorityError::InvalidTransition),
            };
            let observation = self.final_observation.filter(|observation| {
                observation.id == final_observation
                    && observation.sealed
                    && observation.fingerprint == fingerprint
                    && observation.target == Some(generation)
                    && self.observation_is_current(*observation)
            });
            let Some(observation) = observation else {
                self.reject_join()?;
                return Err(AuthorityError::StaleObservation);
            };
            self.beacon_evidence = true;
            self.target = TargetState::Current {
                generation,
                discovery,
                final_observation: observation.id,
            };
            self.axis_states[GenerationAxis::Target.index()] = AxisState::Current;
            Ok(())
        }

        fn reject_join(&mut self) -> Result<(), AuthorityError> {
            self.begin_invalidation(Invalidation::Target)?;
            let generation = self.generation(GenerationAxis::Target);
            self.target = TargetState::Unknown { generation };
            self.axis_states[GenerationAxis::Target.index()] = AxisState::Unknown;
            Ok(())
        }

        fn install_power_evidence(&mut self) -> Result<(), AuthorityError> {
            if self.axis_states[GenerationAxis::Power.index()] != AxisState::Current
                || !matches!(
                    self.target,
                    TargetState::Pending { .. } | TargetState::Current { .. }
                )
            {
                return Err(AuthorityError::InvalidTransition);
            }
            self.power_evidence = true;
            Ok(())
        }

        fn reserve_attempt(&mut self) -> Result<u64, AuthorityError> {
            if self.faulted
                || !self.attempt_authorized
                || !self.beacon_evidence
                || !self.power_evidence
                || !matches!(self.target, TargetState::Current { .. })
                || !matches!(
                    self.attempt,
                    AttemptState::None | AttemptState::Spent { .. } | AttemptState::Revoked { .. }
                )
            {
                return Err(AuthorityError::InvalidTransition);
            }
            let generation = self.advance(GenerationAxis::Attempt)?;
            self.axis_states[GenerationAxis::Attempt.index()] = AxisState::Current;
            self.attempt_authorized = false;
            self.lease = true;
            self.terminal = None;
            self.attempt = AttemptState::Live { generation };
            Ok(generation)
        }

        fn authorize_attempt(&mut self) -> Result<(), AuthorityError> {
            let lineage_closed = matches!(
                self.attempt,
                AttemptState::None | AttemptState::Spent { .. }
            ) || matches!(self.attempt, AttemptState::Revoked { .. })
                && self.terminal.is_some();
            if self.faulted
                || self.attempt_authorized
                || !lineage_closed
                || !self.beacon_evidence
                || !self.power_evidence
                || !matches!(self.target, TargetState::Current { .. })
            {
                return Err(AuthorityError::InvalidTransition);
            }
            self.attempt_authorized = true;
            Ok(())
        }

        fn stage(&mut self) -> Result<(), AuthorityError> {
            self.attempt = match self.attempt {
                AttemptState::Live { generation } => AttemptState::Staged { generation },
                _ => return Err(AuthorityError::InvalidTransition),
            };
            Ok(())
        }

        fn commit(&mut self) -> Result<(), AuthorityError> {
            self.attempt = match self.attempt {
                AttemptState::Staged { generation } => AttemptState::Committing {
                    generation,
                    abort_requested: false,
                },
                _ => return Err(AuthorityError::InvalidTransition),
            };
            self.lease = false;
            Ok(())
        }

        fn submitted(&mut self) -> Result<(), AuthorityError> {
            self.attempt = match self.attempt {
                AttemptState::Committing {
                    generation,
                    abort_requested,
                } => AttemptState::InFlight {
                    generation,
                    abort_requested,
                },
                _ => return Err(AuthorityError::InvalidTransition),
            };
            self.lease = false;
            Ok(())
        }

        fn cancel(&mut self) -> Result<CancellationDisposition, AuthorityError> {
            match self.attempt {
                AttemptState::None => Ok(CancellationDisposition::NoAttempt),
                AttemptState::Live { generation } | AttemptState::Staged { generation } => {
                    self.attempt = AttemptState::Revoked {
                        generation,
                        may_have_transmitted: false,
                    };
                    self.lease = false;
                    self.select_terminal(TerminalResult::CancelledBeforePublish)?;
                    Ok(CancellationDisposition::CancelledBeforePublish)
                }
                AttemptState::Committing { generation, .. } => {
                    // The publication linearization decision won. The caller
                    // cannot be told that no effect occurred.
                    self.attempt = AttemptState::Committing {
                        generation,
                        abort_requested: true,
                    };
                    Ok(CancellationDisposition::AbortInFlight)
                }
                AttemptState::InFlight { generation, .. } => {
                    self.attempt = AttemptState::InFlight {
                        generation,
                        abort_requested: true,
                    };
                    Ok(CancellationDisposition::AbortInFlight)
                }
                AttemptState::Spent { .. } | AttemptState::Revoked { .. } => {
                    Ok(CancellationDisposition::AlreadyTerminal)
                }
            }
        }

        fn finish_attempt(&mut self, result: TerminalResult) -> Result<(), AuthorityError> {
            let generation = match (self.attempt, result) {
                (
                    AttemptState::Committing { generation, .. },
                    TerminalResult::MayHaveTransmitted | TerminalResult::Contained,
                )
                | (AttemptState::InFlight { generation, .. }, TerminalResult::Completed)
                | (
                    AttemptState::InFlight { generation, .. },
                    TerminalResult::MayHaveTransmitted | TerminalResult::Contained,
                ) => generation,
                _ => return Err(AuthorityError::InvalidTransition),
            };
            self.select_terminal(result)?;
            self.attempt = AttemptState::Spent { generation };
            self.lease = false;
            Ok(())
        }

        fn finish_revoked_attempt(&mut self) -> Result<TerminalResult, AuthorityError> {
            let (generation, may_have_transmitted) = match self.attempt {
                AttemptState::Revoked {
                    generation,
                    may_have_transmitted,
                } => (generation, may_have_transmitted),
                _ => return Err(AuthorityError::InvalidTransition),
            };
            let result = if may_have_transmitted {
                TerminalResult::MayHaveTransmitted
            } else {
                TerminalResult::Contained
            };
            self.select_terminal(result)?;
            self.attempt = AttemptState::Spent { generation };
            Ok(result)
        }

        fn select_terminal(&mut self, result: TerminalResult) -> Result<(), AuthorityError> {
            if self.terminal.is_some() {
                return Err(AuthorityError::TerminalAlreadySelected);
            }
            self.terminal = Some(result);
            Ok(())
        }

        fn revoke_attempt(&mut self) {
            let revoked = match self.attempt {
                AttemptState::Live { generation } | AttemptState::Staged { generation } => {
                    Some((generation, false))
                }
                AttemptState::Committing { generation, .. }
                | AttemptState::InFlight { generation, .. } => Some((generation, true)),
                AttemptState::None | AttemptState::Spent { .. } | AttemptState::Revoked { .. } => {
                    None
                }
            };
            if let Some((generation, may_have_transmitted)) = revoked {
                self.attempt = AttemptState::Revoked {
                    generation,
                    may_have_transmitted,
                };
            }
            self.lease = false;
        }

        const fn classify_release(
            hardware_safe: bool,
            observable_release_failed: bool,
        ) -> ReleaseClassification {
            if !hardware_safe {
                ReleaseClassification::ParkUnsafe
            } else if observable_release_failed {
                ReleaseClassification::HardwareSafeReleaseError
            } else {
                ReleaseClassification::Released
            }
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        fn seed_ready_model() -> AuthorityModel {
            let mut model = AuthorityModel::new_for_test();
            let discovery = model.record_discovery(TargetFingerprint(7)).unwrap();
            let target = model.begin_final_target(discovery).unwrap();
            assert_eq!(target, 1);
            model.install_power_evidence().unwrap();
            model.begin_invalidation(Invalidation::Scan).unwrap();
            model.confirm(GenerationAxis::Scan).unwrap();
            let final_observation = model
                .record_final_observation(TargetFingerprint(7), 1)
                .unwrap();
            model
                .seal_final_observation(final_observation, true, true, true)
                .unwrap();
            model.matching_join(final_observation).unwrap();
            model
        }

        fn reserve_authorized_attempt(model: &mut AuthorityModel) -> u64 {
            model.authorize_attempt().unwrap();
            model.reserve_attempt().unwrap()
        }

        #[test]
        fn invalidation_table_is_complete_and_revoke_first() {
            let all_causes = [
                Invalidation::Device,
                Invalidation::Reset,
                Invalidation::Firmware,
                Invalidation::Ownership,
                Invalidation::Domain,
                Invalidation::Channel,
                Invalidation::Power,
                Invalidation::Scan,
                Invalidation::Target,
            ];
            assert_eq!(INVALIDATION_RULES.len(), all_causes.len());
            for cause in all_causes {
                let mut model = seed_ready_model();
                let _ = reserve_authorized_attempt(&mut model);
                let before = model.generations;
                let rule = *AuthorityModel::rule(cause);
                model.begin_invalidation(cause).unwrap();
                for axis in rule.axes {
                    assert_eq!(model.generation(*axis), before[axis.index()] + 1);
                    assert_eq!(model.axis_states[axis.index()], AxisState::Pending);
                }
                if rule.clears.contains(EvidenceMask::LEASE) {
                    assert!(!model.lease);
                }
                if rule.clears.contains(EvidenceMask::ATTEMPT) {
                    assert!(matches!(model.attempt, AttemptState::Revoked { .. }));
                }
                assert_eq!(model.invalidation_trace[0], Some(InvalidationStep::Revoked));
                for (index, axis) in rule.axes.iter().copied().enumerate() {
                    assert_eq!(
                        model.invalidation_trace[1 + index * 2],
                        Some(InvalidationStep::Advanced(axis))
                    );
                    assert_eq!(
                        model.invalidation_trace[2 + index * 2],
                        Some(InvalidationStep::Pending(axis))
                    );
                }
            }
        }

        #[test]
        fn failed_transition_never_restores_authority() {
            let mut model = seed_ready_model();
            model.begin_invalidation(Invalidation::Firmware).unwrap();
            model.fail(GenerationAxis::Firmware).unwrap();
            assert_eq!(
                model.axis_states[GenerationAxis::Firmware.index()],
                AxisState::Unknown
            );
            assert!(!model.beacon_evidence);
            assert!(!model.power_evidence);
            assert!(!model.lease);
            assert!(model.faulted);
            assert_eq!(
                model.confirm(GenerationAxis::Firmware),
                Err(AuthorityError::InvalidTransition)
            );
            assert_eq!(
                model.begin_invalidation(Invalidation::Firmware),
                Err(AuthorityError::InvalidTransition)
            );
        }

        #[test]
        fn generation_exhaustion_fails_before_wrap() {
            for axis in [
                GenerationAxis::Device,
                GenerationAxis::Reset,
                GenerationAxis::Firmware,
                GenerationAxis::Ownership,
                GenerationAxis::Domain,
                GenerationAxis::Channel,
                GenerationAxis::Power,
                GenerationAxis::Scan,
                GenerationAxis::Target,
                GenerationAxis::Attempt,
            ] {
                let mut model = AuthorityModel::new_for_test();
                model.generations[axis.index()] = u64::MAX;
                assert_eq!(
                    model.advance(axis),
                    Err(AuthorityError::GenerationExhausted(axis))
                );
                assert_eq!(model.generation(axis), u64::MAX);
                assert!(model.faulted);
                assert!(
                    model
                        .axis_states
                        .iter()
                        .all(|state| *state == AxisState::Unknown)
                );
            }
        }

        #[test]
        fn multi_axis_invalidation_is_atomic_at_generation_exhaustion() {
            let mut model = seed_ready_model();
            model.generations[GenerationAxis::Ownership.index()] = u64::MAX;
            let generations = model.generations;
            assert_eq!(
                model.begin_invalidation(Invalidation::Reset),
                Err(AuthorityError::GenerationExhausted(
                    GenerationAxis::Ownership
                ))
            );
            assert_eq!(model.generations, generations);
            assert!(model.faulted);
            assert!(
                model
                    .axis_states
                    .iter()
                    .all(|state| *state == AxisState::Unknown)
            );
            assert_eq!(model.target, TargetState::None);
            assert!(!model.beacon_evidence);
            assert!(!model.power_evidence);
            assert!(!model.lease);
        }

        #[test]
        fn observation_and_attempt_exhaustion_poison_authority() {
            let mut observation = seed_ready_model();
            observation.next_observation = u64::MAX;
            assert_eq!(
                observation.record_discovery(TargetFingerprint(17)),
                Err(AuthorityError::GenerationExhausted(GenerationAxis::Scan))
            );
            assert!(observation.faulted);
            assert!(!observation.beacon_evidence);

            let mut attempt = seed_ready_model();
            attempt.generations[GenerationAxis::Attempt.index()] = u64::MAX;
            attempt.authorize_attempt().unwrap();
            assert_eq!(
                attempt.reserve_attempt(),
                Err(AuthorityError::GenerationExhausted(GenerationAxis::Attempt))
            );
            assert!(attempt.faulted);
            assert!(!attempt.beacon_evidence);
            assert!(!attempt.power_evidence);
        }

        #[test]
        fn invalidation_revokes_without_replacing_attempt_generation() {
            let mut model = seed_ready_model();
            let generation = reserve_authorized_attempt(&mut model);
            model.begin_invalidation(Invalidation::Channel).unwrap();
            assert_eq!(model.generation(GenerationAxis::Attempt), generation);
            assert_eq!(
                model.attempt,
                AttemptState::Revoked {
                    generation,
                    may_have_transmitted: false,
                }
            );
        }

        #[test]
        fn post_commit_invalidation_remains_may_have_transmitted() {
            let mut model = seed_ready_model();
            let generation = reserve_authorized_attempt(&mut model);
            model.stage().unwrap();
            model.commit().unwrap();
            model.submitted().unwrap();
            model.begin_invalidation(Invalidation::Reset).unwrap();
            assert_eq!(
                model.attempt,
                AttemptState::Revoked {
                    generation,
                    may_have_transmitted: true,
                }
            );
            assert_eq!(model.generation(GenerationAxis::Attempt), generation);
            assert_eq!(model.terminal, None);
            assert_eq!(
                model.finish_revoked_attempt().unwrap(),
                TerminalResult::MayHaveTransmitted
            );
            assert_eq!(model.terminal, Some(TerminalResult::MayHaveTransmitted));
            assert_eq!(
                model.finish_revoked_attempt(),
                Err(AuthorityError::InvalidTransition)
            );
        }

        #[test]
        fn final_observation_requires_current_slot_and_terminal_proof() {
            let mut model = AuthorityModel::new_for_test();
            let discovery = model.record_discovery(TargetFingerprint(9)).unwrap();
            model.begin_final_target(discovery).unwrap();
            model.install_power_evidence().unwrap();
            model.begin_invalidation(Invalidation::Scan).unwrap();
            model.confirm(GenerationAxis::Scan).unwrap();
            assert_eq!(
                model.record_final_observation(TargetFingerprint(9), 0),
                Err(AuthorityError::StaleObservation)
            );
            let observation = model
                .record_final_observation(TargetFingerprint(9), 1)
                .unwrap();
            assert_eq!(
                model.seal_final_observation(observation, true, false, true),
                Err(AuthorityError::StaleObservation)
            );
            assert!(!model.final_observation.unwrap().sealed);
            model
                .seal_final_observation(observation, true, true, true)
                .unwrap();
        }

        #[test]
        fn target_generation_spans_final_scan_and_matching_join() {
            let mut model = AuthorityModel::new_for_test();
            let discovery = model.record_discovery(TargetFingerprint(11)).unwrap();
            let generation = model.begin_final_target(discovery).unwrap();
            assert_eq!(
                model.axis_states[GenerationAxis::Target.index()],
                AxisState::Pending
            );
            assert_eq!(
                model.confirm(GenerationAxis::Target),
                Err(AuthorityError::InvalidTransition)
            );
            model.install_power_evidence().unwrap();
            model.begin_invalidation(Invalidation::Scan).unwrap();
            model.confirm(GenerationAxis::Scan).unwrap();
            let observation = model
                .record_final_observation(TargetFingerprint(11), 1)
                .unwrap();
            model
                .seal_final_observation(observation, true, true, true)
                .unwrap();
            model.matching_join(observation).unwrap();
            assert_eq!(model.generation(GenerationAxis::Target), generation);
            assert_eq!(
                model.axis_states[GenerationAxis::Target.index()],
                AxisState::Current
            );
            assert!(matches!(
                model.target,
                TargetState::Current { generation: current, .. } if current == generation
            ));
        }

        #[test]
        fn conflicting_final_observation_cannot_promote() {
            let mut model = AuthorityModel::new_for_test();
            let discovery = model.record_discovery(TargetFingerprint(13)).unwrap();
            model.begin_final_target(discovery).unwrap();
            model.install_power_evidence().unwrap();
            model.begin_invalidation(Invalidation::Scan).unwrap();
            model.confirm(GenerationAxis::Scan).unwrap();
            let observation = model
                .record_final_observation(TargetFingerprint(14), 1)
                .unwrap();
            model
                .seal_final_observation(observation, true, true, true)
                .unwrap();
            assert_eq!(
                model.matching_join(observation),
                Err(AuthorityError::StaleObservation)
            );
            assert!(matches!(
                model.target,
                TargetState::Unknown { generation: 2 }
            ));
            assert!(!model.beacon_evidence);
            assert!(!model.power_evidence);
            assert!(model.final_observation.is_none());
        }

        #[test]
        fn attempt_generation_is_reserved_once_and_consumed_unchanged() {
            let mut model = seed_ready_model();
            let generation = reserve_authorized_attempt(&mut model);
            model.stage().unwrap();
            model.commit().unwrap();
            model.submitted().unwrap();
            assert!(matches!(
                model.attempt,
                AttemptState::InFlight { generation: current, .. } if current == generation
            ));
            assert_eq!(model.generation(GenerationAxis::Attempt), generation);
            model.finish_attempt(TerminalResult::Completed).unwrap();
            assert_eq!(model.generation(GenerationAxis::Attempt), generation);
            assert_eq!(
                model.reserve_attempt(),
                Err(AuthorityError::InvalidTransition)
            );
            model.authorize_attempt().unwrap();
            let replacement = model.reserve_attempt().unwrap();
            assert_eq!(replacement, generation + 1);
        }

        #[test]
        fn replacement_cannot_be_preauthorized_before_terminal_close() {
            let mut model = seed_ready_model();
            model.authorize_attempt().unwrap();
            assert_eq!(
                model.authorize_attempt(),
                Err(AuthorityError::InvalidTransition)
            );
            model.reserve_attempt().unwrap();
            assert_eq!(
                model.authorize_attempt(),
                Err(AuthorityError::InvalidTransition)
            );
            model.stage().unwrap();
            model.commit().unwrap();
            model.submitted().unwrap();
            assert_eq!(
                model.authorize_attempt(),
                Err(AuthorityError::InvalidTransition)
            );
            model.finish_attempt(TerminalResult::Completed).unwrap();
            model.authorize_attempt().unwrap();
        }

        #[test]
        fn cancellation_and_publish_have_unambiguous_ordering() {
            let mut before = seed_ready_model();
            reserve_authorized_attempt(&mut before);
            before.stage().unwrap();
            assert_eq!(
                before.cancel().unwrap(),
                CancellationDisposition::CancelledBeforePublish
            );
            assert_eq!(
                before.terminal,
                Some(TerminalResult::CancelledBeforePublish)
            );
            assert_eq!(before.commit(), Err(AuthorityError::InvalidTransition));

            let mut after = seed_ready_model();
            reserve_authorized_attempt(&mut after);
            after.stage().unwrap();
            after.commit().unwrap();
            assert!(!after.lease);
            assert_eq!(
                after.cancel().unwrap(),
                CancellationDisposition::AbortInFlight
            );
            assert!(matches!(
                after.attempt,
                AttemptState::Committing {
                    abort_requested: true,
                    ..
                }
            ));
            after.submitted().unwrap();
            assert!(matches!(
                after.attempt,
                AttemptState::InFlight {
                    abort_requested: true,
                    ..
                }
            ));
            assert_eq!(after.terminal, None);
            after
                .finish_attempt(TerminalResult::MayHaveTransmitted)
                .unwrap();
            assert_eq!(after.terminal, Some(TerminalResult::MayHaveTransmitted));
        }

        #[test]
        fn terminal_result_is_selected_exactly_once() {
            let mut model = seed_ready_model();
            reserve_authorized_attempt(&mut model);
            model.stage().unwrap();
            model.commit().unwrap();
            model.submitted().unwrap();
            model.finish_attempt(TerminalResult::Completed).unwrap();
            assert_eq!(
                model.select_terminal(TerminalResult::Contained),
                Err(AuthorityError::TerminalAlreadySelected)
            );
            assert_eq!(model.terminal, Some(TerminalResult::Completed));
        }

        #[test]
        fn release_classification_parks_only_when_hardware_is_unproven() {
            assert_eq!(
                AuthorityModel::classify_release(false, false),
                ReleaseClassification::ParkUnsafe
            );
            assert_eq!(
                AuthorityModel::classify_release(false, true),
                ReleaseClassification::ParkUnsafe
            );
            assert_eq!(
                AuthorityModel::classify_release(true, true),
                ReleaseClassification::HardwareSafeReleaseError
            );
            assert_eq!(
                AuthorityModel::classify_release(true, false),
                ReleaseClassification::Released
            );
        }
    }
}

#[cfg(test)]
mod tests {
    extern crate std;

    use super::*;
    use std::vec;
    use std::vec::Vec;

    #[test]
    fn client_channel_context_owns_identity_generation_and_authorization() {
        let channel36 = ClientPhysicalChannel {
            band: 1,
            primary: 36,
            center: 36,
            bandwidth: 0,
            center2: 0,
        };
        let channel40 = ClientPhysicalChannel {
            band: 1,
            primary: 40,
            center: 40,
            bandwidth: 0,
            center2: 0,
        };
        let mut context = ClientChannelContext::default();
        assert!(matches!(
            context.ensure_channel(channel36),
            ClientPhysicalChannelEnsure::TransitionRequired { current: None, .. }
        ));
        let first = context.establish_channel(channel36).unwrap();
        assert_eq!(
            context.ensure_channel(channel36),
            ClientPhysicalChannelEnsure::Current(first)
        );
        assert_eq!(context.establish_channel(channel36).unwrap(), first);
        assert_eq!(context.authorize_channel(channel36).unwrap(), first);

        let second = context.establish_channel(channel40).unwrap();
        assert_eq!(second.generation, first.generation + 1);
        assert!(context.authorized_channel().is_err());
        assert!(matches!(
            context.ensure_channel(channel36),
            ClientPhysicalChannelEnsure::TransitionRequired { current: Some(current), .. }
                if current == second
        ));
    }

    #[test]
    fn client_target_bss_evidence_is_one_shot_and_generation_bound() {
        let channel = ClientPhysicalChannel {
            band: 1,
            primary: 36,
            center: 42,
            bandwidth: 2,
            center2: 0,
        };
        let bssid = [1, 2, 3, 4, 5, 6];
        let evidence = ClientScanEvidence {
            scan_id: 4,
            observation_generation: 1,
            observation_timestamp_nanos: 10,
            bssid,
            channel,
        };
        let channel_lease = ClientChannelLease {
            channel,
            generation: 2,
        };
        let mut lease = ClientTargetBssLease::retain(evidence).unwrap();
        assert!(lease.authorize_sae(bssid, channel_lease).is_err());
        lease.mark_rate_power_ready(bssid, channel).unwrap();
        assert_eq!(lease.authorize_sae(bssid, channel_lease), Ok(channel_lease));
        assert!(lease.authorize_sae(bssid, channel_lease).is_err());
        assert!(lease.permits_join(bssid, channel_lease));
        assert!(!lease.permits_join(
            bssid,
            ClientChannelLease {
                generation: 3,
                ..channel_lease
            }
        ));
        lease.invalidate();
        assert!(!lease.permits_join(bssid, channel_lease));
    }

    #[test]
    fn infrastructure_aid_normalization_matches_linux_assoc_boundary() {
        assert_eq!(normalize_infrastructure_aid(0xc004), Ok(4));
        assert_eq!(normalize_infrastructure_aid(0xc7d7), Ok(2007));
        for malformed in [0, 4, 0xc000, 0xc7d8, 0xffff] {
            assert!(normalize_infrastructure_aid(malformed).is_err());
        }
        assert!(
            encode_legacy_wme_add_wcid_command(
                1,
                0,
                7,
                0,
                [1, 2, 3, 4, 5, 6],
                100,
                1,
                0x40,
            )
            .is_err()
        );
    }

    #[test]
    fn preassociation_sae_classifier_is_narrow_and_bounded() {
        let client = [1, 2, 3, 4, 5, 6];
        let peer = [6, 5, 4, 3, 2, 1];
        let mut status77 = vec![0; 32];
        status77[0..2].copy_from_slice(&0x00b0u16.to_le_bytes());
        status77[4..10].copy_from_slice(&client);
        status77[10..16].copy_from_slice(&peer);
        status77[16..22].copy_from_slice(&peer);
        status77[24..26].copy_from_slice(&3u16.to_le_bytes());
        status77[26..28].copy_from_slice(&1u16.to_le_bytes());
        status77[28..30].copy_from_slice(&77u16.to_le_bytes());
        status77[30..32].copy_from_slice(&20u16.to_le_bytes());
        assert_eq!(
            classify_preassociation_sae_auth(&status77, client, peer),
            Ok(Some(PreAssociationSaeAuth {
                transaction: 1,
                status: 77,
            }))
        );

        let mut wrong = status77.clone();
        wrong[10] ^= 1;
        assert!(classify_preassociation_sae_auth(&wrong, client, peer).is_err());
        let mut malformed = status77.clone();
        malformed.pop();
        assert!(classify_preassociation_sae_auth(&malformed, client, peer).is_err());
        let mut non_auth = status77;
        non_auth[0..2].copy_from_slice(&0x0080u16.to_le_bytes());
        assert_eq!(
            classify_preassociation_sae_auth(&non_auth, client, peer),
            Ok(None)
        );
    }

    #[test]
    fn client_interface_commands_bind_one_local_vif_identity_before_authentication() {
        let client = [0x8a, 0xfd, 0x2a, 0x8b, 0x70, 0x5a];
        let [dev, bss] = encode_client_interface_commands(client, true, 14, 15).unwrap();
        assert_eq!(u16::from_le_bytes(dev[34..36].try_into().unwrap()), 1);
        assert_eq!(dev[39], 14);
        assert_eq!(&dev[52..56], &[0, 0, 12, 0]);
        assert_eq!(dev[56], 1);
        assert_eq!(&dev[58..64], &client);
        assert_eq!(u16::from_le_bytes(bss[34..36].try_into().unwrap()), 2);
        assert_eq!(bss[39], 15);
        assert_eq!(&bss[52..56], &[0, 0, 32, 0]);
        assert_eq!(bss[56], 1);
        assert_eq!(
            u32::from_le_bytes(bss[60..64].try_into().unwrap()),
            0x0001_0001
        );
        assert_eq!(u16::from_le_bytes(bss[72..74].try_into().unwrap()), 19);
        assert_eq!(u16::from_le_bytes(bss[78..80].try_into().unwrap()), 19);

        let [disable_bss, disable_dev] =
            encode_client_interface_commands(client, false, 2, 1).unwrap();
        assert_eq!(
            u16::from_le_bytes(disable_bss[34..36].try_into().unwrap()),
            2
        );
        assert_eq!(disable_bss[56], 0);
        assert_eq!(
            u16::from_le_bytes(disable_dev[34..36].try_into().unwrap()),
            1
        );
        assert_eq!(disable_dev[56], 0);

        for invalid in [[0x50, 1, 2, 3, 4, 5], [0x8b, 1, 2, 3, 4, 5], [0; 6]] {
            assert!(encode_client_interface_commands(invalid, true, 1, 2).is_err());
        }
        assert!(encode_client_interface_commands(client, true, 1, 1).is_err());
    }

    #[test]
    fn client_join_association_and_teardown_match_linux_v71_bss_wcid_order() {
        let peer = [0x10, 0x20, 0x30, 0x40, 0x50, 0x60];
        let association = LegacyWmeAssociation {
            bss_index: 0,
            peer_wcid: 7,
            aid: 42,
            peer,
            rcpi: 100,
            basic_rates: 1,
            legacy_rates: 0x40,
            negotiated_qos: true,
            mfp_required: false,
        };
        let mut state = ClientFirmwareEffectsState::default();
        let mut channels = ClientChannelContext::default();
        let physical = ClientPhysicalChannel {
            band: 1,
            primary: 36,
            center: 36,
            bandwidth: 0,
            center2: 0,
        };
        let lease = channels.establish_channel(physical).unwrap();
        assert!(channels.authorized_channel().is_err());
        assert_eq!(channels.authorize_channel(physical).unwrap(), lease);
        state.bind_join(peer, lease, 100).unwrap();
        let client = [6, 5, 4, 3, 2, 1];
        let mut association_response = [0; 22];
        association_response[..2].copy_from_slice(&0x0010u16.to_le_bytes());
        association_response[4..10].copy_from_slice(&client);
        association_response[10..16].copy_from_slice(&peer);
        association_response[16..22].copy_from_slice(&peer);
        assert!(state.accepts_joined_management(&association_response, client));
        association_response[4..10].copy_from_slice(&[1, 1, 1, 1, 1, 1]);
        assert!(!state.accepts_joined_management(&association_response, client));
        let preauth = LegacyWmeAssociation {
            aid: 0,
            negotiated_qos: false,
            ..association
        };
        let mut transcript = Vec::new();
        state
            .prepare_preauth_peer(preauth, lease, |_, command| {
                transcript.push(command.to_vec());
                Ok(())
            })
            .unwrap();
        assert!(
            state
                .associate(
                    association,
                    ClientChannelLease {
                        generation: lease.generation + 1,
                        ..lease
                    },
                    |_, _| Ok(())
                )
                .is_err()
        );

        state
            .associate(association, lease, |_, command| {
                transcript.push(command.to_vec());
                Ok(())
            })
            .unwrap();
        assert!(!state.qos_tx_ready());
        let edca = ClientEdcaParameters {
            ac: [
                ClientEdcaAc { cw_min: 3, cw_max: 7, txop: 47, aifs: 2, acm: false },
                ClientEdcaAc { cw_min: 7, cw_max: 15, txop: 94, aifs: 2, acm: false },
                ClientEdcaAc { cw_min: 15, cw_max: 1023, txop: 0, aifs: 3, acm: false },
                ClientEdcaAc { cw_min: 15, cw_max: 1023, txop: 0, aifs: 7, acm: false },
            ],
        };
        let mut edca_command = Vec::new();
        state
            .program_edca(edca, |command| {
                edca_command = command.to_vec();
                Ok(())
            })
            .unwrap();
        assert!(state.qos_tx_ready());
        assert_eq!(edca_command.len(), 108);
        assert_eq!(&edca_command[36..39], &[0x1d, 0xa0, 1]);
        assert_eq!(&edca_command[64..74], &[7, 0, 15, 0, 94, 0, 2, 0, 0, 0]);
        assert_eq!(&edca_command[74..84], &[3, 0, 7, 0, 47, 0, 2, 0, 0, 0]);
        state
            .teardown(|_, command| {
                transcript.push(command.to_vec());
                Ok(())
            })
            .unwrap();

        assert_eq!(transcript.len(), 5);
        assert_eq!(
            transcript
                .iter()
                .map(|command| u16::from_le_bytes([command[34], command[35]]))
                .collect::<Vec<_>>(),
            [3, 2, 3, 3, 2]
        );
        let preauth_add = &transcript[0];
        assert_eq!(preauth_add[112], 0);
        assert_eq!(&preauth_add[68..74], &peer);
        assert_eq!(
            u16::from_le_bytes(preauth_add[66..68].try_into().unwrap()),
            0
        );
        let bss_add = &transcript[1];
        assert_eq!(&bss_add[66..72], &peer);
        assert_eq!(bss_add[56], 1);
        assert_eq!(bss_add[92], 1);
        assert_eq!(transcript[2][112], 2);
        assert_eq!(transcript[4][56], 0);
        assert!(state.joined.is_none());
        assert!(!state.bss_programmed);
    }

    #[test]
    fn ambiguous_bss_add_rolls_back_or_retains_dirty_teardown_binding() {
        let peer = [0x10, 0x20, 0x30, 0x40, 0x50, 0x60];
        let association = LegacyWmeAssociation {
            bss_index: 0,
            peer_wcid: 7,
            aid: 42,
            peer,
            rcpi: 100,
            basic_rates: 1,
            legacy_rates: 0x40,
            negotiated_qos: true,
            mfp_required: false,
        };
        let lease = ClientChannelLease {
            channel: ClientPhysicalChannel {
                band: 1,
                primary: 36,
                center: 36,
                bandwidth: 0,
                center2: 0,
            },
            generation: 1,
        };

        let mut rolled_back = ClientFirmwareEffectsState::default();
        rolled_back.bind_join(peer, lease, 100).unwrap();
        rolled_back
            .prepare_preauth_peer(
                LegacyWmeAssociation {
                    aid: 0,
                    negotiated_qos: false,
                    ..association
                },
                lease,
                |_, _| Ok(()),
            )
            .unwrap();
        let mut transcript = Vec::new();
        assert!(
            rolled_back
                .associate(association, lease, |cid, command| {
                    transcript.push((cid, command[56]));
                    if transcript.len() == 1 {
                        Err("ambiguous BSS add".into())
                    } else {
                        Ok(())
                    }
                })
                .is_err()
        );
        assert_eq!(transcript, [(2, 1), (2, 0)]);
        assert!(!rolled_back.bss_programmed);
        assert!(!rolled_back.firmware_uncertain);

        let mut dirty = ClientFirmwareEffectsState::default();
        dirty.bind_join(peer, lease, 100).unwrap();
        dirty
            .prepare_preauth_peer(
                LegacyWmeAssociation {
                    aid: 0,
                    negotiated_qos: false,
                    ..association
                },
                lease,
                |_, _| Ok(()),
            )
            .unwrap();
        assert!(
            dirty
                .associate(association, lease, |_, _| Err("no ACK".into()))
                .is_err()
        );
        assert!(dirty.bss_programmed);
        assert!(dirty.firmware_uncertain);
        let mut teardown = Vec::new();
        dirty
            .teardown(|cid, command| {
                teardown.push((cid, command[56]));
                Ok(())
            })
            .unwrap();
        assert_eq!(teardown, [(3, 0), (2, 0)]);
        assert!(!dirty.bss_programmed);
        assert!(!dirty.firmware_uncertain);
        assert!(dirty.joined.is_none());
    }

    #[test]
    fn source_exact_conservative_rate_power_batches_fail_closed() {
        let reg_read = encode_pse_reg_read_command(9).unwrap();
        assert_eq!(&reg_read[36..40], &[0xc0, 0xa0, 0, 9]);
        assert_eq!(
            &reg_read[CONNAC2_MCU_TXD_BYTES..CONNAC2_MCU_TXD_BYTES + 8],
            &[0x00, 0x80, 0x0c, 0x82, 0, 0, 0, 0]
        );
        let mut response = [0u8; 44];
        response[24..26].copy_from_slice(&20u16.to_le_bytes());
        response[36..40].copy_from_slice(&MT7921_PSE_BASE.to_le_bytes());
        response[40..44].copy_from_slice(&0x1234_5678u32.to_le_bytes());
        assert_eq!(
            parse_pse_reg_read_response(0x05, 0, &response),
            Ok(0x1234_5678)
        );
        assert_eq!(
            parse_pse_reg_read_response(0x02, 0, &response),
            Err(RateTxPowerError::InvalidPseResponse)
        );
        assert_eq!(
            parse_pse_reg_read_response(0xed, 0, &response),
            Err(RateTxPowerError::InvalidPseResponse)
        );
        assert_eq!(
            parse_pse_reg_read_response(0x05, 1 << 2, &response),
            Err(RateTxPowerError::InvalidPseResponse)
        );
        let capability = NicCapability {
            element_count: 0,
            mac_address: None,
            phy: Some(NicPhyCapability {
                ht: true,
                vht: true,
                has_5ghz: true,
                max_bandwidth: 2,
                spatial_streams: 2,
                hardware_path: 15,
                he: true,
            }),
            has_6ghz: Some(false),
            chip_capability: None,
            unknown_elements: 0,
        };
        let limits = ConservativePowerLimits {
            alpha2: *b"00",
            max_reg_power_dbm: 20,
            sar_limit_half_dbm: Some(12),
            external_safety_cap_half_dbm: Some(8),
        };
        let commands = encode_conservative_rate_tx_power_commands(capability, limits, 1).unwrap();
        assert_eq!(commands.len(), 8);
        for (index, command) in commands.iter().enumerate() {
            assert_eq!(&command[36..40], &[0x5d, 0xa0, 1, index as u8 + 1]);
            let request = &command[CONNAC2_MCU_TXD_BYTES..];
            assert!(request[4] >= 1 && request[4] <= 8);
            assert!(request[5] == 1 || request[5] == 2);
            for entry in request[44..].chunks_exact(1 + MT7921_SKU_RATE_COUNT) {
                if request[5] == 2 {
                    assert_eq!(&entry[1..5], &[127; 4]);
                    assert!(entry[5..].iter().all(|power| *power == 8));
                } else {
                    assert!(entry[1..].iter().all(|power| *power == 8));
                }
            }
        }
        assert_eq!(commands.last().unwrap()[CONNAC2_MCU_TXD_BYTES + 6], 1);
        assert!(
            commands[..7]
                .iter()
                .all(|command| command[CONNAC2_MCU_TXD_BYTES + 6] == 0)
        );

        assert_eq!(
            encode_conservative_rate_tx_power_commands(
                capability,
                ConservativePowerLimits {
                    sar_limit_half_dbm: None,
                    ..limits
                },
                1,
            ),
            Err(RateTxPowerError::MissingLimit)
        );
        assert_eq!(
            encode_conservative_rate_tx_power_commands(capability, limits, 9),
            Err(RateTxPowerError::InvalidSequence)
        );
        let mut six_ghz = capability;
        six_ghz.has_6ghz = Some(true);
        assert_eq!(
            encode_conservative_rate_tx_power_commands(six_ghz, limits, 1),
            Err(RateTxPowerError::Unsupported6Ghz)
        );
        six_ghz.has_6ghz = None;
        assert_eq!(
            encode_conservative_rate_tx_power_commands(six_ghz, limits, 1),
            Err(RateTxPowerError::Unsupported6Ghz)
        );

        struct PowerTransport {
            completed: usize,
            pse_reads: usize,
            fail_at: Option<usize>,
        }
        impl RateTxPowerTransport for PowerTransport {
            type Error = ();
            fn send_and_wait_consumed(&mut self, _encoded: &[u8]) -> Result<(), Self::Error> {
                if self.fail_at == Some(self.completed) {
                    return Err(());
                }
                self.completed += 1;
                Ok(())
            }
            fn query_pse_base(&mut self) -> Result<u32, Self::Error> {
                self.pse_reads += 1;
                Ok(0)
            }
        }
        let mut transport = PowerTransport {
            completed: 0,
            pse_reads: 0,
            fail_at: None,
        };
        let mut authorizer = RateTxPowerAuthorizer::new();
        let authorization = authorizer
            .submit(&mut transport, capability, limits, 1)
            .unwrap();
        assert!(authorizer.permits(&authorization));
        assert_eq!(transport.completed, 8);
        assert_eq!(transport.pse_reads, 8);

        let mut other_transport = PowerTransport {
            completed: 0,
            pse_reads: 0,
            fail_at: None,
        };
        let mut other_authorizer = RateTxPowerAuthorizer::new();
        let other_authorization = other_authorizer
            .submit(&mut other_transport, capability, limits, 1)
            .unwrap();
        assert!(other_authorizer.permits(&other_authorization));
        assert!(!authorizer.permits(&other_authorization));
        assert!(!other_authorizer.permits(&authorization));

        authorizer.reset();
        assert!(!authorizer.permits(&authorization));
        let mut transport = PowerTransport {
            completed: 0,
            pse_reads: 0,
            fail_at: Some(3),
        };
        assert_eq!(
            RateTxPowerAuthorizer::new().submit(&mut transport, capability, limits, 1),
            Err(RateTxPowerInstallError::Transport {
                command: 3,
                error: (),
            })
        );
        let mut authorizer = RateTxPowerAuthorizer::new();
        authorizer.set_regulatory_domain(*b"IN");
        let mut transport = PowerTransport {
            completed: 0,
            pse_reads: 0,
            fail_at: None,
        };
        assert_eq!(
            authorizer.submit(&mut transport, capability, limits, 1),
            Err(RateTxPowerInstallError::Encode(
                RateTxPowerError::NonWorldDomain
            ))
        );
        assert_eq!(transport.completed, 0);
    }

    #[test]
    fn source_exact_connac2_sae_auth_txwi_and_txp() {
        let mut frame = vec![0u8; 30];
        frame[0..2].copy_from_slice(&0x00b0u16.to_le_bytes());
        frame[0..2].copy_from_slice(&0x80b0u16.to_le_bytes());
        assert_eq!(
            encode_mt7921_5ghz_auth_tx(&frame, 0x1000, 0x2000, 0, 3, 19),
            Err(Mt7921MgmtTxError::InvalidFrame)
        );
        frame[0..2].copy_from_slice(&0x00b0u16.to_le_bytes());
        let tx = encode_mt7921_5ghz_auth_tx(&frame, 0x0102_0000, 0x0102_1000, 7, 3, 19).unwrap();
        let word = |index: usize| {
            u32::from_le_bytes(tx.txwi[index * 4..index * 4 + 4].try_into().unwrap())
        };
        assert_eq!(word(0), (0x10 << 25) | 62);
        assert_eq!(word(1), (1 << 31) | (2 << 16) | (12 << 11) | 19);
        assert_eq!(word(2), (1 << 31) | (1 << 13) | 0x0b);
        assert_eq!(word(3), (1 << 28) | (15 << 11));
        assert_eq!(word(5), (1 << 10) | 3);
        assert_eq!(word(6), (75 << 16) | 4);
        assert_eq!(word(7), 0x0b << 16);
        assert_eq!(&tx.txwi[32..34], &(0x8007u16).to_le_bytes());
        assert_eq!(&tx.txwi[40..44], &0x0102_1000u32.to_le_bytes());
        assert_eq!(&tx.txwi[44..46], &0x801eu16.to_le_bytes());
        assert_eq!(
            tx.descriptor,
            DmaDescriptor {
                buf0: 0x0102_0000,
                ctrl: (64 << 16) | (1 << 30),
                buf1: 0,
                info: 0,
            }
        );
    }

    #[test]
    fn independent_linux_qos_control_port_transcript_matches_e2e75() {
        let mpdu = [
            0x88, 0x01, 0, 0, 2, 2, 3, 4, 5, 6, 6, 5, 4, 3, 2, 1, 1, 0x80, 0xc2, 0, 0, 3,
            0, 0, 7, 0, 0xaa, 0xaa, 3, 0, 0, 0, 0x88, 0x8e, 1, 1, 0, 0,
        ];
        let reference =
            linux_qos_eapol_control_port_reference(&mpdu, 0x1234_5000, 4, 7).unwrap();
        let dword = |index: usize| {
            u32::from_le_bytes(
                reference[index * 4..index * 4 + 4]
                    .try_into()
                    .unwrap(),
            )
        };
        assert_eq!(
            (0..8).map(dword).collect::<Vec<_>>(),
            [
                0x0600_0046,
                0x8072_6807,
                0x8000_2028,
                0x1000_7800,
                0,
                0x0000_0407,
                0x004b_0004,
                0x0028_0000,
            ]
        );
        assert_eq!(&reference[32..40], &[4, 0x80, 0, 0, 0, 0, 0, 0]);
        assert_eq!(
            &reference[40..48],
            &[0, 0x50, 0x34, 0x12, 0x26, 0x80, 0, 0]
        );
        // Normal QoS control-port sequence is owned by mac80211 in the MPDU;
        // TXD3's SN_VALID and SEQ fields are both clear.
        assert_eq!(u16::from_le_bytes(mpdu[22..24].try_into().unwrap()), 0);
        assert_eq!(dword(3) & 0x8fff_0000, 0);
    }

    #[test]
    fn independent_linux_awake_qos_null_probe_is_source_exact() {
        let frame = [
            0xc8, 0x01, 0, 0, 2, 2, 3, 4, 5, 6, 6, 5, 4, 3, 2, 1, 2, 2, 3, 4, 5, 6, 0, 0, 7, 0,
        ];
        let encoded = linux_qos_null_probe_reference(&frame, 0x1234_5000, 3, 6).unwrap();
        let words = (0..8)
            .map(|index| u32::from_le_bytes(encoded[index * 4..index * 4 + 4].try_into().unwrap()))
            .collect::<Vec<_>>();
        assert_eq!(
            words,
            [
                0x0600_003a,
                0x8072_6807,
                0x8000_202c,
                0x1000_7800,
                0,
                0x406,
                0x004b_0004,
                0x002c_0000,
            ]
        );
        assert_eq!(
            &encoded[32..46],
            &[3, 0x80, 0, 0, 0, 0, 0, 0, 0, 0x50, 0x34, 0x12, 0x1a, 0x80]
        );
    }

    #[test]
    fn independent_linux_five_ghz_rate_context_exposes_legacy_fixture_divergence() {
        let rates = [0x8c, 0x12, 0x98, 0x24, 0xb0, 0x48, 0x60, 0x6c];
        let (basic, legacy) = linux_legacy_rate_context_reference(1, &rates).unwrap();
        assert_eq!(basic, 0x15);
        assert_eq!(legacy, 0x3fc0);

        let encoded = encode_legacy_wme_add_wcid_command(
            9,
            0,
            7,
            42,
            [0x10, 0x20, 0x30, 0x40, 0x50, 0x60],
            100,
            basic,
            legacy,
        )
        .unwrap();
        assert_eq!(&encoded[80..82], &[0x15, 0]);
        assert_eq!(&encoded[92..94], &[0xc0, 0x3f]);
        assert_ne!(&encoded[80..82], &[1, 0]);
        assert_ne!(&encoded[92..94], &[0x40, 0]);
    }

    #[test]
    fn group_20_sae_auth_preserves_the_source_exact_dma_envelope() {
        // 24-byte management header + 6-byte authentication fixed fields +
        // group 20's 146-byte H2E commit fields. Payload contents after the
        // public group ID are intentionally synthetic: TX treats them as
        // opaque MPDU bytes.
        let mut frame = vec![0xa5; 176];
        frame[0..2].copy_from_slice(&0x00b0u16.to_le_bytes());
        frame[24..26].copy_from_slice(&3u16.to_le_bytes());
        frame[26..28].copy_from_slice(&1u16.to_le_bytes());
        frame[28..30].copy_from_slice(&126u16.to_le_bytes());
        frame[30..32].copy_from_slice(&20u16.to_le_bytes());

        let tx = encode_mt7921_5ghz_auth_tx(&frame, 0x0103_0000, 0x0103_1000, 0, 3, 19).unwrap();
        assert_eq!(
            tx.txwi,
            [
                0xd0, 0x00, 0x00, 0x20, 0x13, 0x60, 0x02, 0x80, 0x0b, 0x20, 0x00, 0x80, 0x00, 0x78,
                0x00, 0x10, 0x00, 0x00, 0x00, 0x00, 0x03, 0x04, 0x00, 0x00, 0x04, 0x00, 0x4b, 0x00,
                0x00, 0x00, 0x0b, 0x00, 0x00, 0x80, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x10,
                0x03, 0x01, 0xb0, 0x80, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
                0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
            ]
        );
        assert_eq!(
            tx.descriptor,
            DmaDescriptor {
                buf0: 0x0103_0000,
                ctrl: (64 << 16) | (1 << 30),
                buf1: 0,
                info: 0,
            }
        );
    }

    #[test]
    fn management_payload_lengths_do_not_change_the_wfdma_envelope() {
        for frame_len in [128usize, 129, 176, 4095] {
            let mut frame = vec![0u8; frame_len];
            frame[0..2].copy_from_slice(&0x00b0u16.to_le_bytes());
            let tx =
                encode_mt7921_5ghz_auth_tx(&frame, 0x0103_0000, 0x0103_1000, 0, 3, 19).unwrap();
            let txd0 = u32::from_le_bytes(tx.txwi[0..4].try_into().unwrap());
            let txp_len = u16::from_le_bytes(tx.txwi[44..46].try_into().unwrap());
            assert_eq!(txd0 & 0xffff, (frame_len + 32) as u32);
            assert_eq!(txp_len, frame_len as u16 | 0x8000);
            assert_eq!(tx.descriptor.ctrl, (64 << 16) | (1 << 30));
        }
    }

    struct MatrixTransport {
        dmashdl: Vec<u32>,
        completions: alloc::collections::VecDeque<Mt7921MgmtMatrixCompletion>,
        published: Vec<(Mt7921MgmtMatrixCase, usize, u16, u8, u16)>,
        reclaimed: Vec<Mt7921MgmtMatrixCase>,
    }

    impl MatrixTransport {
        fn successful() -> Self {
            Self {
                dmashdl: vec![1 << 28; 10],
                completions: alloc::collections::VecDeque::new(),
                published: vec![],
                reclaimed: vec![],
            }
        }

        fn tx_free(token: u16, status: u8, attempts: u16) -> Vec<u8> {
            let mut raw = vec![0u8; 16];
            raw[0..4].copy_from_slice(&((6u32 << 27) | (1 << 16) | 16).to_le_bytes());
            raw[8..12].copy_from_slice(&((1u32 << 31) | (19 << 14)).to_le_bytes());
            raw[12..16].copy_from_slice(
                &((u32::from(token) << 16) | (u32::from(status) << 13) | u32::from(attempts))
                    .to_le_bytes(),
            );
            raw
        }

        fn tx_status(pid: u8, ack_error: u8) -> Vec<u8> {
            let mut raw = vec![0u8; 40];
            raw[0..4].copy_from_slice(&40u32.to_le_bytes());
            raw[8..12].copy_from_slice(&(u32::from(ack_error) << 16).to_le_bytes());
            raw[16..20].copy_from_slice(&(19u32 << 16).to_le_bytes());
            raw[20..24].copy_from_slice(&(u32::from(pid) << 24).to_le_bytes());
            raw
        }
    }

    impl Mt7921MgmtMatrixTransport for MatrixTransport {
        type Error = ();

        fn read_dmashdl_control(&mut self) -> Result<u32, Self::Error> {
            Ok(self.dmashdl.remove(0))
        }

        fn publish_ring0(
            &mut self,
            case: Mt7921MgmtMatrixCase,
            frame: &[u8],
            encoded: &Mt7921MgmtTx,
        ) -> Result<(), Self::Error> {
            let txp_token = u16::from_le_bytes(encoded.txwi[32..34].try_into().unwrap()) & 0x7fff;
            let txp_len = u16::from_le_bytes(encoded.txwi[44..46].try_into().unwrap()) & 0x0fff;
            self.published
                .push((case, frame.len(), txp_token, encoded.pid, txp_len));
            // Exercise both legal completion orders.
            if txp_token % 2 == 0 {
                self.completions
                    .push_back(Mt7921MgmtMatrixCompletion::TxFree(Self::tx_free(
                        txp_token, 0, 1,
                    )));
                self.completions
                    .push_back(Mt7921MgmtMatrixCompletion::TxStatus(Self::tx_status(
                        encoded.pid,
                        0,
                    )));
            } else {
                self.completions
                    .push_back(Mt7921MgmtMatrixCompletion::TxStatus(Self::tx_status(
                        encoded.pid,
                        0,
                    )));
                self.completions
                    .push_back(Mt7921MgmtMatrixCompletion::TxFree(Self::tx_free(
                        txp_token, 0, 1,
                    )));
            }
            Ok(())
        }

        fn next_completion(&mut self) -> Result<Option<Mt7921MgmtMatrixCompletion>, Self::Error> {
            Ok(self.completions.pop_front())
        }

        fn reclaim_ring0(
            &mut self,
            case: Mt7921MgmtMatrixCase,
            wiped_frame: &[u8],
        ) -> Result<(), Self::Error> {
            assert!(wiped_frame.iter().all(|byte| *byte == 0));
            self.reclaimed.push(case);
            Ok(())
        }
    }

    #[test]
    fn privacy_safe_management_matrix_is_serial_correlated_and_wiped() {
        let client = [2, 0, 0, 0, 0, 1];
        let bssid = [2, 0, 0, 0, 0, 2];
        let fixtures = mt7921_privacy_safe_mgmt_matrix(client, bssid);
        assert_eq!(
            fixtures.each_ref().map(|fixture| fixture.bytes.len()),
            [128, 129, 176, 176, 128]
        );
        assert_eq!(fixtures[0].bytes[24..30], [0xff, 0xff, 0, 0, 0xff, 0xff]);
        assert_eq!(fixtures[3].bytes[24..32], [3, 0, 1, 0, 126, 0, 20, 0]);
        assert!(fixtures[3].bytes[32..].iter().all(|byte| *byte == 0));
        let mut a = fixtures[0].bytes[..128].to_vec();
        let mut b = fixtures[1].bytes[..128].to_vec();
        let mut c = fixtures[2].bytes[..128].to_vec();
        a[22..24].fill(0);
        b[22..24].fill(0);
        c[22..24].fill(0);
        assert_eq!(a, b);
        assert_eq!(a, c);

        let mut transport = MatrixTransport::successful();
        let observations = run_mt7921_privacy_safe_mgmt_matrix(
            &mut transport,
            client,
            bssid,
            0x0103_0000,
            0x0103_1000,
        )
        .unwrap();
        assert_eq!(observations.len(), 5);
        assert!(observations.iter().all(|observation| {
            observation.pre_dmashdl_control == 1 << 28
                && observation.post_dmashdl_control == 1 << 28
                && observation.tx_free_status == 0
                && observation.attempts == 1
                && observation.tx_status_ack_error == 0
                && !observation.raw_tx_free.is_empty()
                && !observation.raw_tx_status.is_empty()
        }));
        assert_eq!(
            transport.published,
            [
                (Mt7921MgmtMatrixCase::Reserved128, 128, 0, 3, 128),
                (Mt7921MgmtMatrixCase::Reserved129, 129, 1, 4, 129),
                (Mt7921MgmtMatrixCase::Reserved176, 176, 2, 5, 176),
                (Mt7921MgmtMatrixCase::InvalidGroup20, 176, 3, 6, 176),
                (Mt7921MgmtMatrixCase::Reserved128Repeat, 128, 4, 7, 128),
            ]
        );
        assert_eq!(transport.reclaimed, Mt7921MgmtMatrixCase::ALL);
    }

    #[test]
    fn privacy_safe_management_matrix_fails_before_publish_without_dmashdl_bypass() {
        let mut transport = MatrixTransport::successful();
        transport.dmashdl[0] = 0;
        assert_eq!(
            run_mt7921_privacy_safe_mgmt_matrix(
                &mut transport,
                [2, 0, 0, 0, 0, 1],
                [2, 0, 0, 0, 0, 2],
                0x0103_0000,
                0x0103_1000,
            ),
            Err(Mt7921MgmtMatrixError::DmashdlBypassDisabled)
        );
        assert!(transport.published.is_empty());
        assert!(transport.reclaimed.is_empty());
    }

    #[test]
    fn privacy_safe_management_matrix_reclaims_after_bad_correlation() {
        let mut transport = MatrixTransport::successful();
        transport
            .completions
            .push_back(Mt7921MgmtMatrixCompletion::TxFree(
                MatrixTransport::tx_free(7, 1, 15),
            ));
        // Prevent publish_ring0 from placing its valid TX_FREE first.
        transport.dmashdl.truncate(2);
        assert_eq!(
            run_mt7921_privacy_safe_mgmt_matrix(
                &mut transport,
                [2, 0, 0, 0, 0, 1],
                [2, 0, 0, 0, 0, 2],
                0x0103_0000,
                0x0103_1000,
            ),
            Err(Mt7921MgmtMatrixError::UncorrelatedCompletion)
        );
        assert_eq!(transport.reclaimed, [Mt7921MgmtMatrixCase::Reserved128]);
    }

    #[test]
    fn privacy_safe_management_matrix_reclaims_when_dmashdl_changes_after_publish() {
        let mut transport = MatrixTransport::successful();
        transport.dmashdl[1] = 0;
        assert_eq!(
            run_mt7921_privacy_safe_mgmt_matrix(
                &mut transport,
                [2, 0, 0, 0, 0, 1],
                [2, 0, 0, 0, 0, 2],
                0x0103_0000,
                0x0103_1000,
            ),
            Err(Mt7921MgmtMatrixError::DmashdlBypassDisabled)
        );
        assert_eq!(transport.published.len(), 1);
        assert_eq!(transport.reclaimed, [Mt7921MgmtMatrixCase::Reserved128]);
    }

    #[test]
    fn management_tx_encoder_rejects_non_auth_and_unrepresentable_identity() {
        let mut frame = vec![0u8; 30];
        frame[0..2].copy_from_slice(&0x0080u16.to_le_bytes());
        assert_eq!(
            encode_mt7921_5ghz_auth_tx(&frame, 0x1000, 0x2000, 0, 3, 19),
            Err(Mt7921MgmtTxError::InvalidFrame)
        );
        frame[0..2].copy_from_slice(&0x00b0u16.to_le_bytes());
        assert_eq!(
            encode_mt7921_5ghz_auth_tx(&frame, 0x1000, 0x2000, 8192, 3, 19),
            Err(Mt7921MgmtTxError::InvalidToken)
        );
        assert_eq!(
            encode_mt7921_5ghz_auth_tx(&frame, 0x1000, 0x2000, 0, 2, 19),
            Err(Mt7921MgmtTxError::InvalidPid)
        );
        assert_eq!(
            encode_mt7921_5ghz_auth_tx(&frame, 0x1000, 0x2000, 0, 127, 19),
            Err(Mt7921MgmtTxError::InvalidPid)
        );
        assert_eq!(
            encode_mt7921_5ghz_auth_tx(&frame, 0x1000, 0x2000, 0, 3, 20),
            Err(Mt7921MgmtTxError::InvalidWcid)
        );
        assert_eq!(
            encode_mt7921_5ghz_auth_tx(&frame, u32::MAX as u64 - 62, 0x2000, 0, 3, 19),
            Err(Mt7921MgmtTxError::InvalidIova)
        );
        assert_eq!(
            encode_mt7921_5ghz_auth_tx(&frame, 0x1000, u32::MAX as u64 - 28, 0, 3, 19),
            Err(Mt7921MgmtTxError::InvalidIova)
        );
        let mut oversized = vec![0u8; 0x8000];
        oversized[0..2].copy_from_slice(&0x00b0u16.to_le_bytes());
        assert_eq!(
            encode_mt7921_5ghz_auth_tx(&oversized, 0x1000, 0x2000, 0, 3, 19),
            Err(Mt7921MgmtTxError::InvalidFrame)
        );
        let mut txp_oversized = vec![0u8; 0x1000];
        txp_oversized[0..2].copy_from_slice(&0x00b0u16.to_le_bytes());
        assert_eq!(
            encode_mt7921_5ghz_auth_tx(&txp_oversized, 0x1000, 0x2000, 0, 3, 19),
            Err(Mt7921MgmtTxError::InvalidFrame)
        );
    }

    #[test]
    fn parses_correlated_tx_free_and_txs_completion() {
        let mut free = [0u8; 12];
        free[0..4].copy_from_slice(&((6u32 << 27) | (1 << 16) | 12).to_le_bytes());
        free[8..12].copy_from_slice(&((7u32 << 16) | 1).to_le_bytes());
        assert_eq!(
            parse_mt7921_tx_free(&free),
            Ok(Mt7921TxFree {
                wcid: None,
                token: 7,
                dropped: false,
                attempts: 1
            })
        );

        let mut txs = [0u8; 40];
        txs[0..4].copy_from_slice(&40u32.to_le_bytes());
        txs[16..20].copy_from_slice(&0u32.to_le_bytes());
        txs[20..24].copy_from_slice(&(3u32 << 24).to_le_bytes());
        assert_eq!(
            parse_mt7921_tx_status(&txs),
            Ok(Mt7921TxStatus {
                wcid: 0,
                pid: 3,
                acked: true
            })
        );
        txs[8..12].copy_from_slice(&(1u32 << 16).to_le_bytes());
        assert_eq!(parse_mt7921_tx_status(&txs).unwrap().acked, false);

        let mut batched = [0u8; 72];
        batched[0..4].copy_from_slice(&72u32.to_le_bytes());
        assert_eq!(
            parse_mt7921_tx_status(&batched),
            Err(Mt7921TxCompletionError::InvalidFormat)
        );
        let mut invalid_wcid = txs;
        invalid_wcid[16..20].copy_from_slice(&(20u32 << 16).to_le_bytes());
        assert_eq!(
            parse_mt7921_tx_status(&invalid_wcid),
            Err(Mt7921TxCompletionError::InvalidFormat)
        );
        let mut stale_free_tail = [0u8; 16];
        stale_free_tail[0..4].copy_from_slice(&((6u32 << 27) | (1 << 16) | 16).to_le_bytes());
        stale_free_tail[8..12].copy_from_slice(&((1u32 << 31) | (19 << 14)).to_le_bytes());
        stale_free_tail[12..16].copy_from_slice(&((7u32 << 16) | 1).to_le_bytes());
        assert_eq!(
            parse_mt7921_tx_free(&stale_free_tail),
            Ok(Mt7921TxFree {
                wcid: Some(19),
                token: 7,
                dropped: false,
                attempts: 1
            })
        );
    }

    #[test]
    fn read_only_register_policy_matches_pinned_linux_mt7921_map() {
        assert_eq!(ReadRegister::McuCommand.bar_offset(), 0xd4000 + 0x1f0);
        assert_eq!(
            ReadRegister::HostInterruptStatus.bar_offset(),
            0xd4000 + 0x200
        );
        assert_eq!(
            ReadRegister::WfdmaGlobalConfig.bar_offset(),
            0xd4000 + 0x208
        );
        assert_eq!(
            ReadRegister::ConnOnLowPowerControl.bar_offset(),
            0xe0000 + 0x10
        );
        assert_eq!(ReadRegister::ConnOnMisc.bar_offset(), 0xe0000 + 0xf0);
        assert!(ReadRegister::ALL.windows(2).all(|pair| pair[0] != pair[1]));

        assert_eq!(
            ReadOnlyStatus::decode(3, 4, 0b1111),
            ReadOnlyStatus {
                firmware_powered: true,
                firmware_n9_ready: true,
                firmware_owns_device: true,
                tx_dma_enabled: true,
                tx_dma_busy: true,
                rx_dma_enabled: true,
                rx_dma_busy: true,
            }
        );
    }

    struct FakeOwnership {
        now: u64,
        status: u32,
        clear_after_writes: Option<u8>,
        writes: u8,
    }
    impl OwnershipTransport for FakeOwnership {
        type Error = ();
        fn now_ms(&self) -> u64 {
            self.now
        }
        fn write_clear_own(&mut self) -> Result<(), Self::Error> {
            self.writes += 1;
            if self.clear_after_writes == Some(self.writes) {
                self.status &= !PCIE_LPCR_HOST_OWN_SYNC;
            }
            Ok(())
        }
        fn read_low_power_control(&mut self) -> Result<u32, Self::Error> {
            Ok(self.status)
        }
        fn sleep_ms(&mut self, milliseconds: u64) {
            self.now += milliseconds;
        }
    }

    #[test]
    fn driver_ownership_succeeds_and_logs_each_transition() {
        let mut transport = FakeOwnership {
            now: 10,
            status: PCIE_LPCR_HOST_OWN_SYNC,
            clear_after_writes: Some(2),
            writes: 0,
        };
        let mut events = Vec::new();
        acquire_driver_ownership(&mut transport, |event| events.push(event)).unwrap();
        assert_eq!(transport.writes, 2);
        assert!(events.contains(&OwnershipEvent::AttemptExpired {
            attempt: 1,
            at_ms: 50
        }));
        assert_eq!(
            events.last(),
            Some(&OwnershipEvent::Acquired {
                attempt: 2,
                at_ms: 50
            })
        );
    }

    #[test]
    fn driver_ownership_times_out_at_hard_deadline() {
        let mut transport = FakeOwnership {
            now: 0,
            status: PCIE_LPCR_HOST_OWN_SYNC,
            clear_after_writes: None,
            writes: 0,
        };
        let mut events = Vec::new();
        assert_eq!(
            acquire_driver_ownership(&mut transport, |event| events.push(event)),
            Err(OwnershipError::Timeout)
        );
        assert_eq!(transport.writes, DRIVER_OWN_ATTEMPTS);
        assert_eq!(transport.now, DRIVER_OWN_HARD_DEADLINE_MS);
        assert_eq!(
            events.last(),
            Some(&OwnershipEvent::TimedOut {
                at_ms: DRIVER_OWN_HARD_DEADLINE_MS
            })
        );
    }

    #[test]
    fn driver_ownership_poll_masks_only_own_sync_like_linux() {
        let mut transport = FakeOwnership {
            now: 0,
            status: PCIE_LPCR_HOST_CLR_OWN,
            clear_after_writes: None,
            writes: 0,
        };
        let mut events = Vec::new();
        assert_eq!(
            acquire_driver_ownership(&mut transport, |event| events.push(event)),
            Ok(())
        );
        assert_eq!(transport.writes, 1);
        assert_eq!(
            events.last(),
            Some(&OwnershipEvent::Acquired {
                attempt: 1,
                at_ms: 0,
            })
        );
    }

    fn pcie_config(link_control: u16) -> [u8; 256] {
        let mut config = [0u8; 256];
        config[PCI_STATUS..PCI_STATUS + 2].copy_from_slice(&PCI_STATUS_CAP_LIST.to_le_bytes());
        config[PCI_CAPABILITY_LIST] = 0x40;
        config[0x40] = 0x05;
        config[0x41] = 0x60;
        config[0x60] = PCI_CAP_ID_EXP;
        config[0x61] = 0;
        config[0x60 + PCI_EXP_LNKCTL..0x60 + PCI_EXP_LNKCTL + 2]
            .copy_from_slice(&link_control.to_le_bytes());
        config
    }

    #[test]
    fn parses_endpoint_and_parent_aspm_like_pinned_mt76() {
        let disabled = pcie_config(0x0040);
        let endpoint_l0s = pcie_config(0x0041);
        let parent_l1 = pcie_config(0x0002);
        assert_eq!(pcie_link_control(&endpoint_l0s), Ok(0x0041));
        assert_eq!(mt76_pci_aspm_supported(&disabled, None), Ok(false));
        assert_eq!(mt76_pci_aspm_supported(&endpoint_l0s, None), Ok(true));
        assert_eq!(
            mt76_pci_aspm_supported(&disabled, Some(&parent_l1)),
            Ok(true)
        );
        assert_eq!(
            mt76_pci_aspm_supported(&disabled, Some(&disabled)),
            Ok(false)
        );
    }

    #[test]
    fn pcie_capability_parser_rejects_absence_loops_and_truncation() {
        let absent = [0u8; 256];
        assert_eq!(
            pcie_link_control(&absent),
            Err(PcieLinkControlError::PcieCapabilityAbsent)
        );
        let mut looped = [0u8; 256];
        looped[PCI_STATUS..PCI_STATUS + 2].copy_from_slice(&PCI_STATUS_CAP_LIST.to_le_bytes());
        looped[PCI_CAPABILITY_LIST] = 0x40;
        looped[0x40] = 0x05;
        looped[0x41] = 0x40;
        assert_eq!(
            pcie_link_control(&looped),
            Err(PcieLinkControlError::CapabilityLoop(0x40))
        );
        let mut truncated = [0u8; 72];
        truncated[PCI_STATUS..PCI_STATUS + 2].copy_from_slice(&PCI_STATUS_CAP_LIST.to_le_bytes());
        truncated[PCI_CAPABILITY_LIST] = 0x40;
        truncated[0x40] = PCI_CAP_ID_EXP;
        assert_eq!(
            pcie_link_control(&truncated),
            Err(PcieLinkControlError::TruncatedPcieCapability(0x40))
        );
    }

    struct FakeOwnershipRoundTrip {
        now: u64,
        status: u32,
        clear_after_writes: Option<u8>,
        set_after_writes: Option<u8>,
        clear_writes: u8,
        set_writes: u8,
        // A full Linux-shaped 10 x 50 ms ownership timeout performs more
        // than 255 one-millisecond polls. This test counter is deliberately
        // wider than the hardware attempt counter.
        reads: u64,
        fail_read: Option<u64>,
        fail_clear_after_write: bool,
        fail_set: bool,
        fail_set_after_write: bool,
        aspm_delays: Vec<(u64, u64)>,
    }

    impl OwnershipTransport for FakeOwnershipRoundTrip {
        type Error = &'static str;
        fn now_ms(&self) -> u64 {
            self.now
        }
        fn write_clear_own(&mut self) -> Result<(), Self::Error> {
            self.clear_writes += 1;
            if self.clear_after_writes == Some(self.clear_writes) {
                self.status &= !PCIE_LPCR_HOST_OWN_SYNC;
            }
            if self.fail_clear_after_write {
                return Err("clear");
            }
            Ok(())
        }
        fn read_low_power_control(&mut self) -> Result<u32, Self::Error> {
            self.reads += 1;
            if self.fail_read == Some(self.reads) {
                return Err("read");
            }
            Ok(self.status)
        }
        fn sleep_ms(&mut self, milliseconds: u64) {
            self.now += milliseconds;
        }
        fn sleep_us_range(&mut self, minimum: u64, maximum: u64) {
            self.aspm_delays.push((minimum, maximum));
            self.now += maximum.div_ceil(1_000);
        }
    }

    impl OwnershipRoundTripTransport for FakeOwnershipRoundTrip {
        fn write_set_own(&mut self) -> Result<(), Self::Error> {
            self.set_writes += 1;
            if self.fail_set {
                return Err("set");
            }
            if self.set_after_writes == Some(self.set_writes) {
                self.status |= PCIE_LPCR_HOST_OWN_SYNC;
            }
            if self.fail_set_after_write {
                return Err("set");
            }
            Ok(())
        }
    }

    fn round_trip_fake(initial: OwnershipState) -> FakeOwnershipRoundTrip {
        FakeOwnershipRoundTrip {
            now: 0,
            status: match initial {
                OwnershipState::DriverOwned => 0,
                OwnershipState::FirmwareOwned => PCIE_LPCR_HOST_OWN_SYNC,
            },
            clear_after_writes: Some(1),
            set_after_writes: Some(1),
            clear_writes: 0,
            set_writes: 0,
            reads: 0,
            fail_read: None,
            fail_clear_after_write: false,
            fail_set: false,
            fail_set_after_write: false,
            aspm_delays: Vec::new(),
        }
    }

    #[test]
    fn firmware_owned_round_trip_delays_acquires_and_restores() {
        let mut transport = round_trip_fake(OwnershipState::FirmwareOwned);
        let mut events = Vec::new();
        assert_eq!(
            round_trip_driver_ownership(&mut transport, true, |event| events.push(event)),
            Ok(OwnershipState::FirmwareOwned)
        );
        assert_eq!(transport.clear_writes, 1);
        assert_eq!(transport.set_writes, 1);
        assert_eq!(transport.status, PCIE_LPCR_HOST_OWN_SYNC);
        assert_eq!(
            transport.aspm_delays,
            vec![(DRIVER_OWN_ASPM_DELAY_MIN_US, DRIVER_OWN_ASPM_DELAY_MAX_US)]
        );
        assert_eq!(events[0], OwnershipRoundTripEvent::SnapshotReadBefore);
        assert!(events.contains(&OwnershipRoundTripEvent::Driver(
            OwnershipEvent::AspmDelay {
                attempt: 1,
                at_ms: 3,
                minimum_us: DRIVER_OWN_ASPM_DELAY_MIN_US,
                maximum_us: DRIVER_OWN_ASPM_DELAY_MAX_US,
            }
        )));
        assert_eq!(
            events.last(),
            Some(&OwnershipRoundTripEvent::Complete {
                restored: OwnershipState::FirmwareOwned
            })
        );
    }

    #[test]
    fn initially_driver_owned_never_issues_set_own() {
        let mut transport = round_trip_fake(OwnershipState::DriverOwned);
        assert_eq!(
            round_trip_driver_ownership(&mut transport, false, |_| {}),
            Ok(OwnershipState::DriverOwned)
        );
        assert_eq!(transport.clear_writes, 1);
        assert_eq!(transport.set_writes, 0);
        assert_eq!(transport.status, 0);
    }

    #[test]
    fn post_clear_failure_still_restores_firmware_ownership() {
        let mut transport = round_trip_fake(OwnershipState::FirmwareOwned);
        transport.fail_read = Some(2);
        assert_eq!(
            round_trip_driver_ownership(&mut transport, false, |_| {}),
            Err(OwnershipRoundTripError::Acquire(OwnershipError::Transport(
                "read"
            )))
        );
        assert_eq!(transport.clear_writes, 1);
        assert_eq!(transport.set_writes, 1);
        assert_eq!(transport.status, PCIE_LPCR_HOST_OWN_SYNC);
    }

    #[test]
    fn ambiguous_clear_error_settles_for_aspm_before_rollback() {
        let mut transport = round_trip_fake(OwnershipState::FirmwareOwned);
        transport.fail_clear_after_write = true;
        assert_eq!(
            round_trip_driver_ownership(&mut transport, true, |_| {}),
            Err(OwnershipRoundTripError::Acquire(OwnershipError::Transport(
                "clear"
            )))
        );
        assert_eq!(
            transport.aspm_delays,
            vec![(DRIVER_OWN_ASPM_DELAY_MIN_US, DRIVER_OWN_ASPM_DELAY_MAX_US)]
        );
        assert_eq!(transport.set_writes, 1);
        assert_eq!(transport.status, PCIE_LPCR_HOST_OWN_SYNC);
    }

    #[test]
    fn acquisition_timeout_still_runs_firmware_rollback() {
        let mut transport = round_trip_fake(OwnershipState::FirmwareOwned);
        transport.clear_after_writes = None;
        assert_eq!(
            round_trip_driver_ownership(&mut transport, false, |_| {}),
            Err(OwnershipRoundTripError::Acquire(OwnershipError::Timeout))
        );
        assert_eq!(transport.clear_writes, DRIVER_OWN_ATTEMPTS);
        assert_eq!(transport.set_writes, 1);
        assert_eq!(transport.status, PCIE_LPCR_HOST_OWN_SYNC);
    }

    #[test]
    fn aspm_timeout_preserves_all_ten_full_poll_windows() {
        let mut transport = round_trip_fake(OwnershipState::FirmwareOwned);
        transport.clear_after_writes = None;
        assert_eq!(
            round_trip_driver_ownership(&mut transport, true, |_| {}),
            Err(OwnershipRoundTripError::Acquire(OwnershipError::Timeout))
        );
        assert_eq!(transport.clear_writes, DRIVER_OWN_ATTEMPTS);
        assert_eq!(
            transport.aspm_delays.len(),
            usize::from(DRIVER_OWN_ATTEMPTS)
        );
        assert_eq!(transport.now, DRIVER_OWN_ASPM_HARD_DEADLINE_MS);
        assert_eq!(transport.set_writes, 1);
        assert_eq!(transport.status, PCIE_LPCR_HOST_OWN_SYNC);
    }

    #[test]
    fn rollback_timeout_is_terminal_and_never_claims_restoration() {
        let mut transport = round_trip_fake(OwnershipState::FirmwareOwned);
        transport.set_after_writes = None;
        let mut events = Vec::new();
        assert_eq!(
            round_trip_driver_ownership(&mut transport, false, |event| events.push(event)),
            Err(OwnershipRoundTripError::Restore(
                FirmwareOwnershipError::Timeout
            ))
        );
        assert_eq!(transport.clear_writes, 1);
        assert_eq!(transport.set_writes, DRIVER_OWN_ATTEMPTS);
        assert_eq!(transport.status, 0);
        assert!(
            !events
                .iter()
                .any(|event| matches!(event, OwnershipRoundTripEvent::Complete { .. }))
        );
    }

    #[test]
    fn rollback_read_error_is_retained_but_polling_continues_to_restoration() {
        let mut transport = round_trip_fake(OwnershipState::FirmwareOwned);
        transport.fail_read = Some(3);
        assert_eq!(
            round_trip_driver_ownership(&mut transport, false, |_| {}),
            Err(OwnershipRoundTripError::Restore(
                FirmwareOwnershipError::Transport("read")
            ))
        );
        assert_eq!(transport.set_writes, 1);
        assert_eq!(transport.status, PCIE_LPCR_HOST_OWN_SYNC);
    }

    #[test]
    fn ambiguous_set_error_is_polled_and_restoration_is_verified() {
        let mut transport = round_trip_fake(OwnershipState::FirmwareOwned);
        transport.fail_set_after_write = true;
        assert_eq!(
            round_trip_driver_ownership(&mut transport, false, |_| {}),
            Err(OwnershipRoundTripError::Restore(
                FirmwareOwnershipError::Transport("set")
            ))
        );
        assert_eq!(transport.set_writes, 1);
        assert_eq!(transport.status, PCIE_LPCR_HOST_OWN_SYNC);
    }

    #[test]
    fn primary_and_rollback_errors_are_both_retained() {
        let mut transport = round_trip_fake(OwnershipState::FirmwareOwned);
        transport.fail_read = Some(2);
        transport.fail_set = true;
        assert_eq!(
            round_trip_driver_ownership(&mut transport, false, |_| {}),
            Err(OwnershipRoundTripError::AcquireAndRestore {
                acquire: OwnershipError::Transport("read"),
                restore: FirmwareOwnershipError::TransportAndTimeout("set"),
            })
        );
        assert_eq!(transport.clear_writes, 1);
        assert_eq!(transport.set_writes, DRIVER_OWN_ATTEMPTS);
    }

    #[derive(Default)]
    struct FakeMemory {
        allocation: Option<RingAllocation>,
        freed: Vec<RingAllocation>,
    }
    impl Low32RingMemory for FakeMemory {
        type Error = ();
        fn allocate_low32(&mut self, size: usize, _: usize) -> Result<RingAllocation, Self::Error> {
            Ok(self.allocation.unwrap_or(RingAllocation {
                id: 1,
                iova: 0x1000_0000,
                len: size,
            }))
        }
        fn free(&mut self, allocation: RingAllocation) {
            self.freed.push(allocation);
        }
    }
    #[derive(Clone, Copy, Debug, Eq, PartialEq)]
    enum PublishEvent {
        Descriptor(u16),
        Release,
        Producer(u16),
        Acquire,
    }
    #[derive(Default)]
    struct FakePublisher(Vec<PublishEvent>);
    impl RingPublisher for FakePublisher {
        type Error = ();
        fn write_descriptor(&mut self, index: u16, _: DmaDescriptor) -> Result<(), Self::Error> {
            self.0.push(PublishEvent::Descriptor(index));
            Ok(())
        }
        fn release_fence(&mut self) {
            self.0.push(PublishEvent::Release)
        }
        fn publish_producer(&mut self, index: u16) -> Result<(), Self::Error> {
            self.0.push(PublishEvent::Producer(index));
            Ok(())
        }
        fn acquire_fence(&mut self) {
            self.0.push(PublishEvent::Acquire)
        }
    }

    #[test]
    fn inactive_wfdma_ring_orders_publish_reclaim_and_teardown() {
        let mut memory = FakeMemory::default();
        let mut ring = WfdmaRing::allocate(&mut memory, 2).unwrap();
        assert_eq!(ring.allocation().iova, 0x1000_0000);
        let mut publisher = FakePublisher::default();
        assert_eq!(
            ring.enqueue(
                &mut publisher,
                DmaSegment {
                    iova: 0x2000_0000,
                    len: 64
                },
                None,
                7,
            ),
            Ok(0)
        );
        assert_eq!(
            publisher.0,
            [
                PublishEvent::Descriptor(0),
                PublishEvent::Release,
                PublishEvent::Producer(1)
            ]
        );
        assert_eq!(ring.reclaim_one(&mut publisher), None);
        assert!(ring.complete(0));
        assert_eq!(ring.reclaim_one(&mut publisher), Some(0));
        assert_eq!(publisher.0.last(), Some(&PublishEvent::Acquire));
        ring.teardown(&mut memory);
        assert_eq!(memory.freed.len(), 1);
    }

    #[test]
    fn inactive_wfdma_ring_rejects_high_or_wrapping_allocations() {
        let mut memory = FakeMemory {
            allocation: Some(RingAllocation {
                id: 2,
                iova: 0xffff_fff0,
                len: 32,
            }),
            ..Default::default()
        };
        assert!(matches!(
            WfdmaRing::allocate(&mut memory, 2),
            Err(RingError::AllocationAbove32Bits)
        ));
        assert_eq!(memory.freed.len(), 1);
    }

    fn patch_image(section_type: u32, offset: u32, length: u32) -> Vec<u8> {
        let mut bytes = vec![0; PATCH_HEADER_LEN + PATCH_SECTION_LEN + length as usize];
        bytes[0..16].copy_from_slice(b"20260101-120000\0");
        bytes[16..20].copy_from_slice(b"ALPS");
        bytes[20..24].copy_from_slice(&0x8a10_8a10u32.to_be_bytes());
        bytes[44..48].copy_from_slice(&1u32.to_be_bytes());
        let section = PATCH_HEADER_LEN;
        bytes[section..section + 4].copy_from_slice(&section_type.to_be_bytes());
        bytes[section + 4..section + 8].copy_from_slice(&offset.to_be_bytes());
        bytes[section + 12..section + 16].copy_from_slice(&0x0090_0000u32.to_be_bytes());
        bytes[section + 16..section + 20].copy_from_slice(&length.to_be_bytes());
        bytes
    }

    #[test]
    fn parses_connac2_patch_header_and_bounded_sections() {
        let bytes = patch_image(0x0004_0002, 160, 4);
        let patch = Patch::parse(&bytes).unwrap();
        assert_eq!(patch.header.platform, b"ALPS");
        assert_eq!(patch.header.hardware_software_version, 0x8a10_8a10);
        assert_eq!(patch.region_count(), 1);
        let section = patch.sections().next().unwrap();
        assert_eq!(section.address, 0x0090_0000);
        assert_eq!(section.payload.len(), 4);

        assert_eq!(
            Patch::parse(&patch_image(1, 160, 4)),
            Err(PatchError::UnsupportedSectionType)
        );
        assert_eq!(
            Patch::parse(&patch_image(2, 200, 4)),
            Err(PatchError::PayloadOutOfBounds)
        );
    }

    #[derive(Default)]
    struct FakeL1 {
        selector: u32,
        writes: Vec<u32>,
        fail_window: Option<u16>,
    }
    impl DynamicL1Transport for FakeL1 {
        type Error = u16;
        fn read_selector(&mut self) -> Result<u32, Self::Error> {
            Ok(self.selector)
        }
        fn write_selector(&mut self, value: u32) -> Result<(), Self::Error> {
            self.selector = value;
            self.writes.push(value);
            Ok(())
        }
        fn read_window(&mut self, offset: u16) -> Result<u32, Self::Error> {
            if self.fail_window == Some(offset) {
                return Err(offset);
            }
            Ok((self.selector & 0xffff) << 16 | u32::from(offset))
        }
    }

    #[test]
    fn dynamic_l1_reads_only_fixed_targets_and_restores_selector() {
        let mut transport = FakeL1 {
            selector: 0xabcd_1234,
            ..Default::default()
        };
        let mut events = Vec::new();
        let status =
            read_dynamic_identity_status(&mut transport, |event| events.push(event)).unwrap();
        assert_eq!(status.chip_id, 0x7001_0200);
        assert_eq!(status.revision, 0x7001_0204);
        assert_eq!(status.hardware_bound, 0x7001_0020);
        assert_eq!(status.top_low_power_control, 0x1806_0010);
        let reads = events
            .iter()
            .filter_map(|event| match event {
                DynamicL1Event::RegisterRead { physical, .. } => Some(*physical),
                _ => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(reads, [0x7001_0200, 0x7001_0020, 0x7001_0204, 0x1806_0010]);
        assert_eq!(transport.selector, 0xabcd_1234);
        assert_eq!(transport.writes, [0xabcd_7001, 0xabcd_1806, 0xabcd_1234]);
        assert_eq!(
            events.last(),
            Some(&DynamicL1Event::SelectorRestored { raw: 0xabcd_1234 })
        );
    }

    #[test]
    fn dynamic_l1_restores_selector_after_window_failure() {
        let mut transport = FakeL1 {
            selector: 0x55aa_4321,
            fail_window: Some(0x0204),
            ..Default::default()
        };
        let mut events = Vec::new();
        assert_eq!(
            read_dynamic_identity_status(&mut transport, |event| events.push(event)),
            Err(DynamicL1Error::Transport(0x0204))
        );
        assert_eq!(transport.selector, 0x55aa_4321);
        assert_eq!(transport.writes.last(), Some(&0x55aa_4321));
        assert_eq!(
            events.last(),
            Some(&DynamicL1Event::SelectorRestored { raw: 0x55aa_4321 })
        );
    }

    struct FakeTopOwn {
        now: u64,
        selector: u32,
        status: u32,
        clear_after_ms: Option<u64>,
        writes: Vec<u32>,
    }
    impl TopOwnershipTransport for FakeTopOwn {
        type Error = ();
        fn now_ms(&self) -> u64 {
            self.now
        }
        fn read_selector(&mut self) -> Result<u32, Self::Error> {
            Ok(self.selector)
        }
        fn write_selector(&mut self, value: u32) -> Result<(), Self::Error> {
            self.selector = value;
            self.writes.push(value);
            Ok(())
        }
        fn write_top_driver_own(&mut self) -> Result<(), Self::Error> {
            Ok(())
        }
        fn read_top_low_power_control(&mut self) -> Result<u32, Self::Error> {
            if self
                .clear_after_ms
                .is_some_and(|deadline| self.now >= deadline)
            {
                self.status &= !MT_TOP_LPCR_HOST_FW_OWN;
            }
            Ok(self.status)
        }
        fn sleep_ms(&mut self, milliseconds: u64) {
            self.now += milliseconds;
        }
    }

    #[test]
    fn top_ownership_succeeds_and_restores_selector() {
        let mut transport = FakeTopOwn {
            now: 0,
            selector: 0xaaaa_5555,
            status: MT_TOP_LPCR_HOST_FW_OWN,
            clear_after_ms: Some(2),
            writes: Vec::new(),
        };
        let mut events = Vec::new();
        acquire_top_driver_ownership(&mut transport, |event| events.push(event)).unwrap();
        assert_eq!(transport.now, 2);
        assert_eq!(transport.selector, 0xaaaa_5555);
        assert_eq!(transport.writes, [0xaaaa_1806, 0xaaaa_5555]);
        assert_eq!(
            events.last(),
            Some(&TopOwnershipEvent::SelectorRestored { raw: 0xaaaa_5555 })
        );
    }

    #[test]
    fn top_ownership_times_out_and_restores_selector() {
        let mut transport = FakeTopOwn {
            now: 0,
            selector: 0x1234_5678,
            status: MT_TOP_LPCR_HOST_FW_OWN,
            clear_after_ms: None,
            writes: Vec::new(),
        };
        assert_eq!(
            acquire_top_driver_ownership(&mut transport, |_| {}),
            Err(TopOwnershipError::Timeout)
        );
        assert_eq!(transport.now, TOP_DRIVER_OWN_DEADLINE_MS);
        assert_eq!(transport.selector, 0x1234_5678);
    }

    #[test]
    fn top_ownership_rejects_driver_command_readback_and_restores() {
        let mut transport = FakeTopOwn {
            now: 0,
            selector: 0,
            status: MT_TOP_LPCR_HOST_DRV_OWN,
            clear_after_ms: None,
            writes: Vec::new(),
        };
        assert_eq!(
            acquire_top_driver_ownership(&mut transport, |_| {}),
            Err(TopOwnershipError::UnexpectedState(MT_TOP_LPCR_HOST_DRV_OWN))
        );
        assert_eq!(transport.selector, 0);
    }

    struct FakeFwdl {
        interrupt_enable: u32,
        global_config: u32,
        ring: FwdlRingRegisters,
        corrupt_count: bool,
        events: Vec<PublishEvent>,
        writes: Vec<(DisabledFwdlWrite, u32)>,
    }
    impl DisabledFwdlRingTransport for FakeFwdl {
        type Error = ();
        fn read(&mut self, register: DisabledFwdlRegister) -> Result<u32, Self::Error> {
            Ok(match register {
                DisabledFwdlRegister::HostInterruptEnable => self.interrupt_enable,
                DisabledFwdlRegister::WfdmaGlobalConfig => self.global_config,
                DisabledFwdlRegister::DescriptorBase => self.ring.descriptor_base,
                DisabledFwdlRegister::DescriptorCount => {
                    self.ring.descriptor_count
                        + u32::from(self.corrupt_count && self.ring.descriptor_count == 128)
                }
                DisabledFwdlRegister::CpuIndex => self.ring.cpu_index,
                DisabledFwdlRegister::DmaIndex => self.ring.dma_index,
            })
        }
        fn write(&mut self, register: DisabledFwdlWrite, value: u32) -> Result<(), Self::Error> {
            self.writes.push((register, value));
            match register {
                DisabledFwdlWrite::DescriptorBase => self.ring.descriptor_base = value,
                DisabledFwdlWrite::DescriptorCount => self.ring.descriptor_count = value,
                DisabledFwdlWrite::CpuIndex => self.ring.cpu_index = value,
            }
            Ok(())
        }
        fn release_fence(&mut self) {
            self.events.push(PublishEvent::Release)
        }
    }

    fn fake_fwdl() -> FakeFwdl {
        FakeFwdl {
            interrupt_enable: 0,
            global_config: 0x1010_b870,
            ring: FwdlRingRegisters {
                descriptor_base: 0xaaaa_0000,
                descriptor_count: 7,
                cpu_index: 3,
                dma_index: 3,
            },
            corrupt_count: false,
            events: Vec::new(),
            writes: Vec::new(),
        }
    }

    #[test]
    fn disabled_fwdl_ring_programs_after_fence_and_restores() {
        let mut transport = fake_fwdl();
        let snapshot = transport.ring;
        let mut events = Vec::new();
        let programmed =
            program_disabled_fwdl_ring(&mut transport, 0x0100_0000, |event| events.push(event))
                .unwrap();
        assert_eq!(programmed.descriptor_base, 0x0100_0000);
        assert_eq!(programmed.descriptor_count, 128);
        assert_eq!(transport.ring, snapshot);
        assert_eq!(transport.events, [PublishEvent::Release]);
        assert_eq!(events[2], DisabledFwdlEvent::DescriptorFence);
        assert_eq!(events.last(), Some(&DisabledFwdlEvent::Restored(snapshot)));
    }

    #[test]
    fn disabled_fwdl_ring_rejects_active_dma_or_interrupts_without_writes() {
        let mut transport = fake_fwdl();
        transport.interrupt_enable = 1;
        assert!(matches!(
            program_disabled_fwdl_ring(&mut transport, 0x0100_0000, |_| {}),
            Err(DisabledFwdlError::DmaOrInterruptActive { .. })
        ));
        assert!(transport.writes.is_empty());
    }

    #[test]
    fn disabled_fwdl_ring_restores_after_readback_mismatch() {
        let mut transport = fake_fwdl();
        let snapshot = transport.ring;
        transport.corrupt_count = true;
        assert!(matches!(
            program_disabled_fwdl_ring(&mut transport, 0x0100_0000, |_| {}),
            Err(DisabledFwdlError::Readback { .. })
        ));
        // The fake corrupts reads only; stored register values are restored.
        assert_eq!(transport.ring, snapshot);
    }

    struct FakeWfsys {
        now: u64,
        raw: u32,
        ready_at: Option<u64>,
        writes: Vec<u32>,
    }
    impl WfsysResetTransport for FakeWfsys {
        type Error = ();
        fn now_ms(&self) -> u64 {
            self.now
        }
        fn read_reset_control(&mut self) -> Result<u32, Self::Error> {
            if self.ready_at.is_some_and(|ready| self.now >= ready) {
                self.raw |= WFSYS_SW_INIT_DONE;
            }
            Ok(self.raw)
        }
        fn write_reset_control(&mut self, value: u32) -> Result<(), Self::Error> {
            self.raw = value;
            self.writes.push(value);
            Ok(())
        }
        fn sleep_ms(&mut self, milliseconds: u64) {
            self.now += milliseconds;
        }
    }

    #[test]
    fn wfsys_reset_matches_linux_assert_release_and_ready_poll() {
        let mut transport = FakeWfsys {
            now: 10,
            raw: 0x101,
            ready_at: Some(62),
            writes: Vec::new(),
        };
        let mut events = Vec::new();
        reset_wfsys(&mut transport, |event| events.push(event)).unwrap();
        assert_eq!(transport.writes, [0x100, 0x101]);
        assert_eq!(transport.now, 62);
        assert_eq!(
            events.last(),
            Some(&WfsysResetEvent::Ready {
                raw: 0x111,
                at_ms: 52
            })
        );
    }

    #[test]
    fn wfsys_reset_has_bounded_ready_timeout() {
        let mut transport = FakeWfsys {
            now: 0,
            raw: WFSYS_SW_RST_B,
            ready_at: None,
            writes: Vec::new(),
        };
        let mut events = Vec::new();
        assert_eq!(
            reset_wfsys(&mut transport, |event| events.push(event)),
            Err(WfsysResetError::Timeout)
        );
        assert_eq!(transport.now, WFSYS_ASSERT_MS + WFSYS_READY_DEADLINE_MS);
        assert_eq!(
            events.last(),
            Some(&WfsysResetEvent::TimedOut {
                raw: WFSYS_SW_RST_B,
                at_ms: WFSYS_ASSERT_MS + WFSYS_READY_DEADLINE_MS
            })
        );
    }

    struct FakeIrqReset {
        now: u64,
        raw: u32,
        fail_setup: Option<&'static str>,
        fail_cleanup: Vec<&'static str>,
        operations: Vec<&'static str>,
    }
    impl WfsysResetTransport for FakeIrqReset {
        type Error = &'static str;
        fn now_ms(&self) -> u64 {
            self.now
        }
        fn read_reset_control(&mut self) -> Result<u32, Self::Error> {
            self.raw |= WFSYS_SW_INIT_DONE;
            Ok(self.raw)
        }
        fn write_reset_control(&mut self, value: u32) -> Result<(), Self::Error> {
            self.operations.push("wfsys_write");
            if self.fail_setup == Some("wfsys_write") {
                return Err("wfsys_write");
            }
            self.raw = value;
            Ok(())
        }
        fn sleep_ms(&mut self, milliseconds: u64) {
            self.now += milliseconds;
        }
    }
    impl IrqResetTransport for FakeIrqReset {
        fn install_irq(&mut self, _: PciIrqCapability) -> Result<(), Self::Error> {
            self.operations.push("install_irq");
            if self.fail_setup == Some("install_irq") {
                Err("install")
            } else {
                Ok(())
            }
        }
        fn mask_host_irq(&mut self) -> Result<(), Self::Error> {
            self.operations.push("mask_host");
            if self.fail_setup == Some("mask_host") {
                Err("mask_host")
            } else {
                Ok(())
            }
        }
        fn enable_pcie_mac_irq(&mut self) -> Result<(), Self::Error> {
            self.operations.push("enable_mac");
            if self.fail_setup == Some("enable_mac") {
                Err("enable_mac")
            } else {
                Ok(())
            }
        }
        fn disable_pcie_mac_irq(&mut self) -> Result<(), Self::Error> {
            self.operations.push("disable_mac");
            if self.fail_cleanup.contains(&"disable_mac") {
                Err("disable_mac")
            } else {
                Ok(())
            }
        }
        fn disable_irq(&mut self) -> Result<(), Self::Error> {
            self.operations.push("disable_irq");
            if self.fail_cleanup.contains(&"disable_irq") {
                Err("disable_irq")
            } else {
                Ok(())
            }
        }
        fn containment_reset(&mut self) -> Result<(), Self::Error> {
            self.operations.push("containment_reset");
            if self.fail_cleanup.contains(&"containment_reset") {
                Err("containment_reset")
            } else {
                Ok(())
            }
        }
        fn verify_contained(&mut self) -> Result<(), Self::Error> {
            self.operations.push("verify_contained");
            if self.fail_cleanup.contains(&"verify_contained") {
                Err("verify_contained")
            } else {
                Ok(())
            }
        }
    }

    fn irq_capability() -> PciIrqCapability {
        PciIrqCapability {
            kind: PciIrqKind::Msi,
            count: 32,
            eventfd: true,
        }
    }

    #[test]
    fn irq_reset_boundary_orders_setup_before_dma_and_contains_it() {
        let mut transport = FakeIrqReset {
            now: 0,
            raw: WFSYS_SW_RST_B,
            fail_setup: None,
            fail_cleanup: Vec::new(),
            operations: Vec::new(),
        };
        exercise_irq_reset_boundary(&mut transport, irq_capability(), |_| {}).unwrap();
        assert_eq!(
            transport.operations,
            [
                "wfsys_write",
                "wfsys_write",
                "mask_host",
                "enable_mac",
                "install_irq",
                "mask_host",
                "disable_mac",
                "disable_irq",
                "containment_reset",
                "verify_contained"
            ]
        );
    }

    #[test]
    fn irq_reset_boundary_attempts_all_cleanup_after_ambiguous_install_error() {
        let mut transport = FakeIrqReset {
            now: 0,
            raw: WFSYS_SW_RST_B,
            fail_setup: Some("install_irq"),
            fail_cleanup: vec!["disable_mac", "disable_irq"],
            operations: Vec::new(),
        };
        assert_eq!(
            exercise_irq_reset_boundary(&mut transport, irq_capability(), |_| {}),
            Err(IrqResetError {
                primary: Some(IrqResetPrimaryError::Install("install")),
                cleanup: vec![
                    (IrqResetCleanupStep::DisablePcieMac, "disable_mac"),
                    (IrqResetCleanupStep::DisableIrq, "disable_irq"),
                ],
            })
        );
        assert_eq!(
            transport.operations,
            [
                "wfsys_write",
                "wfsys_write",
                "mask_host",
                "enable_mac",
                "install_irq",
                "mask_host",
                "disable_mac",
                "disable_irq",
                "containment_reset",
                "verify_contained"
            ]
        );
    }

    #[test]
    fn irq_reset_boundary_stops_setup_at_each_failure_and_still_contains() {
        for failed in ["wfsys_write", "mask_host", "enable_mac", "install_irq"] {
            let mut transport = FakeIrqReset {
                now: 0,
                raw: WFSYS_SW_RST_B,
                fail_setup: Some(failed),
                fail_cleanup: Vec::new(),
                operations: Vec::new(),
            };
            assert!(exercise_irq_reset_boundary(&mut transport, irq_capability(), |_| {}).is_err());
            assert_eq!(
                &transport.operations[transport.operations.len() - 5..],
                [
                    "mask_host",
                    "disable_mac",
                    "disable_irq",
                    "containment_reset",
                    "verify_contained"
                ]
            );
        }
    }

    #[test]
    fn irq_reset_boundary_rejects_invalid_vector_before_mmio() {
        let mut transport = FakeIrqReset {
            now: 0,
            raw: WFSYS_SW_RST_B,
            fail_setup: None,
            fail_cleanup: Vec::new(),
            operations: Vec::new(),
        };
        let invalid = PciIrqCapability {
            kind: PciIrqKind::Msi,
            count: 0,
            eventfd: true,
        };
        assert_eq!(
            exercise_irq_reset_boundary(&mut transport, invalid, |_| {}),
            Err(IrqResetError {
                primary: Some(IrqResetPrimaryError::InvalidCapability),
                cleanup: Vec::new(),
            })
        );
        assert!(transport.operations.is_empty());
    }

    struct FakeInterrupt {
        global: u32,
        enable: u32,
        status: u32,
        stuck: bool,
        writes: Vec<(&'static str, u32)>,
    }
    impl DisabledFwdlInterruptTransport for FakeInterrupt {
        type Error = ();
        fn read_global_config(&mut self) -> Result<u32, Self::Error> {
            Ok(self.global)
        }
        fn read_interrupt_enable(&mut self) -> Result<u32, Self::Error> {
            Ok(self.enable)
        }
        fn write_interrupt_enable(&mut self, value: u32) -> Result<(), Self::Error> {
            self.enable = value;
            self.writes.push(("enable", value));
            Ok(())
        }
        fn read_interrupt_status(&mut self) -> Result<u32, Self::Error> {
            Ok(self.status)
        }
        fn acknowledge_interrupt_status(&mut self, value: u32) -> Result<(), Self::Error> {
            self.writes.push(("ack", value));
            if !self.stuck {
                self.status &= !value;
            }
            Ok(())
        }
    }

    #[test]
    fn disabled_interrupt_acknowledges_only_fwdl_and_restores_mask() {
        let mut transport = FakeInterrupt {
            global: 0x1010_b870,
            enable: 0,
            status: MT7921_INT_TX_DONE_FWDL | 0x20,
            stuck: false,
            writes: Vec::new(),
        };
        let mut events = Vec::new();
        assert_eq!(
            mask_ack_disabled_fwdl_interrupt(&mut transport, |event| events.push(event)),
            Ok(0x20)
        );
        assert_eq!(
            transport.writes,
            [
                ("enable", 0),
                ("ack", MT7921_INT_TX_DONE_FWDL),
                ("enable", 0)
            ]
        );
        assert_eq!(transport.status, 0x20);
        assert_eq!(
            events.last(),
            Some(&DisabledInterruptEvent::MaskRestored { value: 0 })
        );
    }

    #[test]
    fn disabled_interrupt_rejects_active_state_without_writes() {
        let mut transport = FakeInterrupt {
            global: 1,
            enable: 0,
            status: 0,
            stuck: false,
            writes: Vec::new(),
        };
        assert_eq!(
            mask_ack_disabled_fwdl_interrupt(&mut transport, |_| {}),
            Err(DisabledInterruptError::DmaActive(1))
        );
        assert!(transport.writes.is_empty());
    }

    #[test]
    fn disabled_interrupt_reports_stuck_w1c_and_restores_mask() {
        let mut transport = FakeInterrupt {
            global: 0,
            enable: 0,
            status: MT7921_INT_TX_DONE_FWDL,
            stuck: true,
            writes: Vec::new(),
        };
        assert_eq!(
            mask_ack_disabled_fwdl_interrupt(&mut transport, |_| {}),
            Err(DisabledInterruptError::AckDidNotClear(
                MT7921_INT_TX_DONE_FWDL
            ))
        );
        assert_eq!(transport.writes.last(), Some(&("enable", 0)));
    }

    struct FakeFirmwareStage {
        calls: Vec<&'static str>,
        descriptor: DmaDescriptor,
        corrupt_readback: bool,
        fail_reset: bool,
    }
    impl Default for FakeFirmwareStage {
        fn default() -> Self {
            Self {
                calls: Vec::new(),
                descriptor: DmaDescriptor::reset(),
                corrupt_readback: false,
                fail_reset: false,
            }
        }
    }
    impl DisabledFirmwareStageTransport for FakeFirmwareStage {
        type Error = &'static str;
        fn write_payload(&mut self, _: &[u8]) -> Result<(), Self::Error> {
            self.calls.push("payload");
            Ok(())
        }
        fn write_descriptor(&mut self, descriptor: DmaDescriptor) -> Result<(), Self::Error> {
            self.calls.push("descriptor");
            self.descriptor = descriptor;
            Ok(())
        }
        fn release_fence(&mut self) {
            self.calls.push("fence");
        }
        fn read_descriptor(&mut self) -> Result<DmaDescriptor, Self::Error> {
            self.calls.push("readback");
            Ok(if self.corrupt_readback {
                DmaDescriptor::reset()
            } else {
                self.descriptor
            })
        }
        fn reset_descriptor(&mut self) -> Result<(), Self::Error> {
            self.calls.push("reset");
            if self.fail_reset {
                return Err("reset failed");
            }
            self.descriptor = DmaDescriptor::reset();
            Ok(())
        }
        fn zero_payload(&mut self, _: usize) -> Result<(), Self::Error> {
            self.calls.push("zero");
            Ok(())
        }
    }

    #[test]
    fn stages_one_disabled_firmware_descriptor_then_cleans_it() {
        let mut transport = FakeFirmwareStage::default();
        let mut events = Vec::new();
        let descriptor = stage_disabled_firmware_chunk(
            &mut transport,
            0x0100_1000,
            &[0x5a; MT7921_FWDL_CHUNK_BYTES],
            |event| events.push(event),
        )
        .unwrap();
        assert_eq!(descriptor.buf0, 0x0100_1000);
        assert_eq!(descriptor.ctrl, (4096 << 16) | (1 << 30));
        assert_eq!(
            transport.calls,
            [
                "payload",
                "descriptor",
                "fence",
                "readback",
                "reset",
                "zero"
            ]
        );
        assert_eq!(transport.descriptor, DmaDescriptor::reset());
        assert_eq!(
            events.last(),
            Some(&DisabledFirmwareStageEvent::PayloadZeroed { bytes: 4096 })
        );
    }

    #[test]
    fn rejects_malformed_firmware_staging_without_writes() {
        for (iova, payload) in [
            (0x1000, &[][..]),
            (0x1000, &[0u8; MT7921_FWDL_CHUNK_BYTES + 1][..]),
            (u64::from(u32::MAX), &[0u8; 2][..]),
        ] {
            let mut transport = FakeFirmwareStage::default();
            assert!(stage_disabled_firmware_chunk(&mut transport, iova, payload, |_| {}).is_err());
            assert!(transport.calls.is_empty());
        }
    }

    #[test]
    fn readback_mismatch_still_resets_and_zeros_staged_memory() {
        let mut transport = FakeFirmwareStage {
            corrupt_readback: true,
            ..Default::default()
        };
        assert!(matches!(
            stage_disabled_firmware_chunk(&mut transport, 0x1000, b"firmware", |_| {}),
            Err(DisabledFirmwareStageError::DescriptorReadback { .. })
        ));
        assert_eq!(transport.calls.last_chunk::<2>(), Some(&["reset", "zero"]));
        assert_eq!(transport.descriptor, DmaDescriptor::reset());
    }

    #[test]
    fn descriptor_reset_failure_still_zeros_payload() {
        let mut transport = FakeFirmwareStage {
            fail_reset: true,
            ..Default::default()
        };
        assert_eq!(
            stage_disabled_firmware_chunk(&mut transport, 0x1000, b"firmware", |_| {}),
            Err(DisabledFirmwareStageError::Reset("reset failed"))
        );
        assert_eq!(transport.calls.last_chunk::<2>(), Some(&["reset", "zero"]));
    }

    #[test]
    fn firmware_completion_tracks_partial_timeout_complete_and_malformed() {
        assert_eq!(FirmwareCompletionTracker::new(0, 0, 1), None);
        let mut tracker = FirmwareCompletionTracker::new(4, 100, 20).unwrap();
        assert_eq!(
            tracker.observe(2, 110),
            Some(FirmwareCompletion::Partial {
                completed: 2,
                total: 4
            })
        );
        assert_eq!(tracker.observe(1, 111), None);
        assert_eq!(tracker.observe(5, 111), None);
        assert_eq!(
            tracker.observe(2, 120),
            Some(FirmwareCompletion::TimedOut {
                completed: 2,
                total: 4
            })
        );
        assert_eq!(tracker.observe(4, 121), Some(FirmwareCompletion::Complete));
    }

    struct FakeGlobalTx {
        global: u32,
        interrupts: u32,
        rings: [TxRingState; MT7921_TX_RING_SLOTS],
        writes: Vec<(usize, u32)>,
        resets: Vec<u32>,
    }
    impl FakeGlobalTx {
        fn new() -> Self {
            let mut rings = [TxRingState {
                descriptor_base: 0,
                descriptor_count: 128,
                cpu_index: 0,
                dma_index: 0,
            }; MT7921_TX_RING_SLOTS];
            for (index, ring) in rings.iter_mut().enumerate() {
                ring.descriptor_base = 0x8000_0000 + index as u32 * 0x1000;
            }
            Self {
                global: 0x1010_b870,
                interrupts: 0,
                rings,
                writes: Vec::new(),
                resets: Vec::new(),
            }
        }
    }
    impl GlobalTxRingTransport for FakeGlobalTx {
        type Error = ();
        fn read_global_config(&mut self) -> Result<u32, Self::Error> {
            Ok(self.global)
        }
        fn read_interrupt_enable(&mut self) -> Result<u32, Self::Error> {
            Ok(self.interrupts)
        }
        fn read_tx_ring(&mut self, index: usize) -> Result<TxRingState, Self::Error> {
            Ok(self.rings[index])
        }
        fn write_tx_ring(
            &mut self,
            index: usize,
            descriptor_base: u32,
            descriptor_count: u32,
            cpu_index: u32,
        ) -> Result<(), Self::Error> {
            self.writes.push((index, descriptor_base));
            self.rings[index].descriptor_base = descriptor_base;
            self.rings[index].descriptor_count = descriptor_count;
            self.rings[index].cpu_index = cpu_index;
            Ok(())
        }
        fn reset_tx_indices(&mut self, value: u32) -> Result<(), Self::Error> {
            self.resets.push(value);
            for ring in &mut self.rings {
                ring.dma_index = 0;
            }
            Ok(())
        }
    }

    struct FakeMcuRx {
        global: u32,
        interrupts: u32,
        registers: McuRxRegisters,
        writes: Vec<&'static str>,
    }
    impl DisabledMcuRxTransport for FakeMcuRx {
        type Error = ();
        fn read_global_config(&mut self) -> Result<u32, Self::Error> {
            Ok(self.global)
        }
        fn read_interrupt_enable(&mut self) -> Result<u32, Self::Error> {
            Ok(self.interrupts)
        }
        fn read_registers(&mut self) -> Result<McuRxRegisters, Self::Error> {
            Ok(self.registers)
        }
        fn read_registers_at(&mut self, index: usize) -> Result<McuRxRegisters, Self::Error> {
            if index != 0 {
                return Err(());
            }
            Ok(self.registers)
        }
        fn write_initial(
            &mut self,
            descriptor_base: u32,
            descriptor_count: u32,
        ) -> Result<(), Self::Error> {
            self.writes.push("initial");
            self.registers = McuRxRegisters {
                descriptor_base,
                descriptor_count,
                cpu_index: 0,
                dma_index: 0,
            };
            Ok(())
        }
        fn publish_cpu_index(&mut self, cpu_index: u32) -> Result<(), Self::Error> {
            self.writes.push("publish");
            self.registers.cpu_index = cpu_index;
            Ok(())
        }
        fn write_ring_initial(
            &mut self,
            index: usize,
            descriptor_base: u32,
            descriptor_count: u32,
        ) -> Result<(), Self::Error> {
            if index != 0 {
                return Err(());
            }
            self.write_initial(descriptor_base, descriptor_count)
        }
        fn publish_ring_cpu_index(
            &mut self,
            index: usize,
            cpu_index: u32,
        ) -> Result<(), Self::Error> {
            if index != 0 {
                return Err(());
            }
            self.publish_cpu_index(cpu_index)
        }
        fn release_fence(&mut self) {
            self.writes.push("fence");
        }
    }

    #[test]
    fn disabled_mcu_rx_resets_both_indices_before_publish() {
        let mut transport = FakeMcuRx {
            global: 0x1010_b870,
            interrupts: 0,
            registers: McuRxRegisters {
                descriptor_base: 0x8000_0000,
                descriptor_count: 8,
                cpu_index: 3,
                dma_index: 3,
            },
            writes: Vec::new(),
        };
        assert_eq!(
            program_disabled_mcu_rx_ring(&mut transport, 0x0100_3000, |_| {}),
            Ok(McuRxRegisters {
                descriptor_base: 0x0100_3000,
                descriptor_count: 8,
                cpu_index: 7,
                dma_index: 0,
            })
        );
        assert_eq!(transport.writes, ["initial", "fence", "publish"]);
    }

    #[test]
    fn disabled_mcu_rx_rejects_dirty_ring_without_writes() {
        let mut transport = FakeMcuRx {
            global: 0x1010_b870,
            interrupts: 0,
            registers: McuRxRegisters {
                descriptor_base: 0,
                descriptor_count: 8,
                cpu_index: 1,
                dma_index: 0,
            },
            writes: Vec::new(),
        };
        assert!(matches!(
            program_disabled_mcu_rx_ring(&mut transport, 0x0100_3000, |_| {}),
            Err(DisabledMcuRxError::DirtyRing(_))
        ));
        assert!(transport.writes.is_empty());
    }

    struct FakeGlobalRx {
        rings: [McuRxRegisters; MT7921_RX_RING_SLOTS],
        writes: Vec<(usize, u32)>,
    }
    impl DisabledMcuRxTransport for FakeGlobalRx {
        type Error = ();
        fn read_global_config(&mut self) -> Result<u32, Self::Error> {
            Ok(0x5020_b870)
        }
        fn read_interrupt_enable(&mut self) -> Result<u32, Self::Error> {
            Ok(0)
        }
        fn read_registers(&mut self) -> Result<McuRxRegisters, Self::Error> {
            Ok(self.rings[0])
        }
        fn read_registers_at(&mut self, index: usize) -> Result<McuRxRegisters, Self::Error> {
            Ok(self.rings[index])
        }
        fn write_initial(
            &mut self,
            descriptor_base: u32,
            descriptor_count: u32,
        ) -> Result<(), Self::Error> {
            self.write_ring_initial(0, descriptor_base, descriptor_count)
        }
        fn publish_cpu_index(&mut self, cpu_index: u32) -> Result<(), Self::Error> {
            self.publish_ring_cpu_index(0, cpu_index)
        }
        fn write_ring_initial(
            &mut self,
            index: usize,
            descriptor_base: u32,
            descriptor_count: u32,
        ) -> Result<(), Self::Error> {
            self.rings[index] = McuRxRegisters {
                descriptor_base,
                descriptor_count,
                cpu_index: 0,
                dma_index: 0,
            };
            self.writes.push((index, 0));
            Ok(())
        }
        fn publish_ring_cpu_index(
            &mut self,
            index: usize,
            cpu_index: u32,
        ) -> Result<(), Self::Error> {
            self.rings[index].cpu_index = cpu_index;
            self.writes.push((index, cpu_index));
            Ok(())
        }
        fn release_fence(&mut self) {}
    }

    #[test]
    fn global_rx_preparation_owns_all_eight_slots_before_publish() {
        let empty = McuRxRegisters {
            descriptor_base: 0,
            descriptor_count: 0,
            cpu_index: 0,
            dma_index: 0,
        };
        let mut transport = FakeGlobalRx {
            rings: [empty; MT7921_RX_RING_SLOTS],
            writes: Vec::new(),
        };
        let owned =
            prepare_global_rx_rings(&mut transport, 0x0100_3000, 0x0100_4000, |_| {}).unwrap();
        assert_eq!(owned[0].descriptor_base, 0x0100_4000);
        assert_eq!(owned[0].cpu_index, 7);
        assert!(
            owned[1..]
                .iter()
                .all(|ring| ring.descriptor_base == 0x0100_3000)
        );
        assert!(owned[1..].iter().all(|ring| ring.cpu_index == 0));
        assert_eq!(transport.writes.len(), MT7921_RX_RING_SLOTS * 2);
        assert!(
            transport.writes[..MT7921_RX_RING_SLOTS]
                .iter()
                .all(|(_, cpu_index)| *cpu_index == 0)
        );
    }

    #[test]
    fn global_tx_preparation_owns_all_eighteen_rings_before_index_reset() {
        let mut transport = FakeGlobalTx::new();
        let mut events = Vec::new();
        let owned = prepare_global_tx_rings(
            &mut transport,
            0x0100_0000,
            0x0100_1000,
            0x0100_2000,
            |event| events.push(event),
        )
        .unwrap();
        assert_eq!(transport.writes.len(), MT7921_TX_RING_SLOTS);
        assert_eq!(transport.resets, [MT7921_RESET_ALL_TX_INDICES]);
        for (index, state) in owned.into_iter().enumerate() {
            assert_eq!(
                state.descriptor_base,
                if index == MT7921_FWDL_RING_INDEX {
                    0x0100_1000
                } else if index == MT7921_MCU_TX_RING_INDEX {
                    0x0100_2000
                } else {
                    0x0100_0000
                }
            );
            assert_eq!(
                state.descriptor_count,
                if index == MT7921_MCU_TX_RING_INDEX {
                    MT7921_MCU_TX_RING_COUNT
                } else {
                    MT7921_FWDL_RING_COUNT
                }
            );
            assert_eq!(state.cpu_index, 0);
            assert_eq!(state.dma_index, 0);
        }
        assert!(matches!(
            events.last(),
            Some(GlobalTxRingEvent::RingVerified { index: 17, .. })
        ));
    }

    #[test]
    fn global_tx_preparation_rejects_dirty_mmio_and_arenas_before_writes() {
        let mut dirty = FakeGlobalTx::new();
        dirty.rings[7].cpu_index += 1;
        assert!(matches!(
            prepare_global_tx_rings(&mut dirty, 0x0100_0000, 0x0100_1000, 0x0100_2000, |_| {}),
            Err(GlobalTxRingError::DirtyRing { index: 7, .. })
        ));
        assert!(dirty.writes.is_empty());

        let mut overlap = FakeGlobalTx::new();
        assert_eq!(
            prepare_global_tx_rings(&mut overlap, 0x0100_0000, 0x0100_0000, 0x0100_2000, |_| {}),
            Err(GlobalTxRingError::InvalidArena)
        );
        assert!(overlap.writes.is_empty());
    }

    #[test]
    fn encodes_connac2_patch_protocol_before_scatter_dma() {
        let semaphore = encode_download_command(DownloadCommand::PatchSemaphoreGet, 1).unwrap();
        assert_eq!(semaphore.len(), PATCH_SEMAPHORE_REQUEST_BYTES);
        assert_eq!(
            u32::from_le_bytes(semaphore[0..4].try_into().unwrap()),
            0x4100_0044
        );
        assert_eq!(
            u32::from_le_bytes(semaphore[4..8].try_into().unwrap()),
            0x8001_0000
        );
        assert_eq!(&semaphore[32..34], &36u16.to_le_bytes());
        assert_eq!(&semaphore[34..36], &0x8000u16.to_le_bytes());
        assert_eq!(&semaphore[36..40], &[0x10, 0xa0, 3, 1]);
        assert_eq!(&semaphore[64..68], &1u32.to_le_bytes());
        let release = encode_download_command(DownloadCommand::PatchSemaphoreRelease, 2).unwrap();
        assert_eq!(&release[36..40], &[0x10, 0xa0, 3, 2]);
        assert_eq!(&release[64..68], &0u32.to_le_bytes());
        let power = encode_download_command(DownloadCommand::NicPowerControl, 3).unwrap();
        assert_eq!(&power[36..40], &[0x04, 0xa0, 3, 3]);
        assert_eq!(&power[64..68], &[1, 0, 0, 0]);
        let capability = encode_download_command(DownloadCommand::GetNicCapability, 4).unwrap();
        assert_eq!(capability.len(), CONNAC2_MCU_TXD_BYTES);
        assert_eq!(&capability[34..36], &0x8000u16.to_le_bytes());
        assert_eq!(&capability[36..40], &[0x8a, 0xa0, 1, 4]);
        let eeprom = encode_download_command(
            DownloadCommand::ReadEepromBlock {
                address: MT7921_EEPROM_HW_TYPE_BLOCK,
            },
            5,
        )
        .unwrap();
        assert_eq!(eeprom.len(), CONNAC2_MCU_TXD_BYTES + 24);
        assert_eq!(&eeprom[36..44], &[0xed, 0xa0, 0, 5, 0, 1, 0, 1]);
        assert_eq!(&eeprom[64..68], &MT7921_EEPROM_HW_TYPE_BLOCK.to_le_bytes());
        assert_eq!(&eeprom[68..], &[0; 20]);
        assert_eq!(
            encode_download_command(DownloadCommand::ReadEepromBlock { address: 0x551 }, 5),
            Err(DownloadCommandError::InvalidEepromAddress)
        );

        let patch = encode_download_command(
            DownloadCommand::PatchStart {
                address: 0x0090_0000,
                length: 0x0001_6780,
                mode: 1 << 31,
            },
            2,
        )
        .unwrap();
        assert_eq!(patch.len(), PATCH_START_REQUEST_BYTES);
        assert_eq!(
            u32::from_le_bytes(patch[0..4].try_into().unwrap()),
            0x4100_004c
        );
        assert_eq!(&patch[36..40], &[0x05, 0xa0, 3, 2]);
        assert_eq!(&patch[64..68], &0x0090_0000u32.to_le_bytes());
        assert_eq!(&patch[68..72], &0x0001_6780u32.to_le_bytes());
        assert_eq!(&patch[72..76], &(1u32 << 31).to_le_bytes());
        let finish = encode_download_command(DownloadCommand::PatchFinish, 3).unwrap();
        assert_eq!(finish.len(), PATCH_FINISH_REQUEST_BYTES);
        assert_eq!(&finish[36..40], &[0x07, 0xa0, 3, 3]);
        assert_eq!(&finish[64..68], &[0; 4]);
        let start = encode_download_command(
            DownloadCommand::FirmwareStart {
                address: 0x0091_5000,
                option: 1,
            },
            4,
        )
        .unwrap();
        assert_eq!(start.len(), FIRMWARE_START_REQUEST_BYTES);
        assert_eq!(
            u32::from_le_bytes(start[0..4].try_into().unwrap()),
            0x4100_0048
        );
        assert_eq!(&start[34..36], &0x8000u16.to_le_bytes());
        assert_eq!(&start[36..40], &[0x02, 0xa0, 3, 4]);
        assert_eq!(&start[64..68], &1u32.to_le_bytes());
        assert_eq!(&start[68..72], &0x0091_5000u32.to_le_bytes());
        assert_eq!(
            encode_download_command(
                DownloadCommand::FirmwareStart {
                    address: 0,
                    option: 0,
                },
                4,
            ),
            Err(DownloadCommandError::InvalidFirmwareStart)
        );
        assert_eq!(
            encode_download_command(DownloadCommand::PatchSemaphoreGet, 0),
            Err(DownloadCommandError::InvalidSequence)
        );
    }

    #[test]
    fn derives_connac2_patch_download_security_mode_fail_closed() {
        assert_eq!(patch_download_mode(u32::MAX), Ok(DL_MODE_NEED_RESPONSE));
        assert_eq!(patch_download_mode(0), Ok(DL_MODE_NEED_RESPONSE));
        assert_eq!(
            patch_download_mode(0x0100_0002),
            Ok(DL_MODE_NEED_RESPONSE | DL_MODE_ENCRYPT | DL_MODE_RESET_SECURITY_IV | (2 << 1))
        );
        assert_eq!(
            patch_download_mode(0x0200_0000),
            Ok(DL_MODE_NEED_RESPONSE
                | DL_MODE_ENCRYPT
                | DL_MODE_RESET_SECURITY_IV
                | DL_MODE_ENCRYPTION_MODE_SELECT)
        );
        assert_eq!(
            patch_download_mode(0x0300_0000),
            Err(PatchSecurityError::UnsupportedEncryptionType(3))
        );
    }

    #[test]
    fn derives_connac2_ram_region_download_mode() {
        assert_eq!(firmware_download_mode(0, false), DL_MODE_NEED_RESPONSE);
        assert_eq!(
            firmware_download_mode(0b0001_0111, false),
            DL_MODE_NEED_RESPONSE
                | DL_MODE_ENCRYPT
                | DL_MODE_KEY_INDEX
                | DL_MODE_RESET_SECURITY_IV
                | DL_MODE_ENCRYPTION_MODE_SELECT
        );
        assert_eq!(firmware_download_mode(1 << 5, false), DL_MODE_NEED_RESPONSE);
        assert_eq!(
            firmware_download_mode(0, true),
            DL_MODE_NEED_RESPONSE | DL_MODE_WORKING_PDA_CR4
        );
    }

    #[test]
    fn parses_bounded_connac2_download_responses_by_sequence() {
        let mut bytes = [0u8; 40];
        bytes[24..26].copy_from_slice(&12u16.to_le_bytes());
        bytes[26..28].copy_from_slice(&0xa0u16.to_le_bytes());
        bytes[28] = 4;
        bytes[29] = 7;
        bytes[30] = 1;
        bytes[32] = 2;
        assert_eq!(
            parse_download_response(&bytes, 7),
            Ok(DownloadResponse {
                length: 12,
                packet_type: 0xa0,
                event_id: 4,
                sequence: 7,
                option: 1,
                extended_event_id: 2,
            })
        );
        assert_eq!(
            parse_download_response(&bytes, 6),
            Err(DownloadResponseError::SequenceMismatch {
                expected: 6,
                actual: 7
            })
        );
        assert_eq!(
            parse_download_response(&bytes[..35], 7),
            Err(DownloadResponseError::Truncated)
        );
        bytes[24..26].copy_from_slice(&41u16.to_le_bytes());
        assert_eq!(
            parse_download_response(&bytes, 7),
            Err(DownloadResponseError::InvalidLength)
        );
    }

    #[test]
    fn vfio_irq_selection_requires_eventfd_and_prefers_msix() {
        let capabilities = [
            PciIrqCapability {
                kind: PciIrqKind::Intx,
                count: 1,
                eventfd: true,
            },
            PciIrqCapability {
                kind: PciIrqKind::Msi,
                count: 1,
                eventfd: false,
            },
            PciIrqCapability {
                kind: PciIrqKind::Msix,
                count: 8,
                eventfd: true,
            },
        ];
        assert_eq!(select_vfio_irq(&capabilities), Some(capabilities[2]));
        assert_eq!(
            select_vfio_irq(&[PciIrqCapability {
                kind: PciIrqKind::Msi,
                count: 1,
                eventfd: false,
            }]),
            None
        );
    }

    #[test]
    fn irq_lifecycle_cannot_unmask_before_eventfd_install() {
        let capability = PciIrqCapability {
            kind: PciIrqKind::Msi,
            count: 1,
            eventfd: true,
        };
        assert!(!IrqLifecycle::Uninstalled.may_unmask_device());
        assert_eq!(
            IrqLifecycle::Uninstalled.enable_device_source(),
            Err(IrqLifecycleError::InvalidTransition)
        );
        let installed = IrqLifecycle::Uninstalled.install(capability).unwrap();
        assert!(installed.may_unmask_device());
        let enabled = installed.enable_device_source().unwrap();
        assert_eq!(enabled.observe_event(0), Err(IrqLifecycleError::EmptyEvent));
        let observed = enabled.observe_event(1).unwrap();
        assert_eq!(observed.disable(), Ok(IrqLifecycle::Disabled));
    }

    struct FakePinnedTeardown {
        now: u64,
        busy_until: Option<u64>,
        reset_fails: bool,
        calls: Vec<&'static str>,
    }
    impl PinnedDmaTeardownTransport for FakePinnedTeardown {
        type Error = &'static str;
        fn now_ms(&self) -> u64 {
            self.now
        }
        fn mask_device_interrupts(&mut self) -> Result<(), Self::Error> {
            self.calls.push("mask");
            Ok(())
        }
        fn disable_dma(&mut self) -> Result<(), Self::Error> {
            self.calls.push("disable");
            Ok(())
        }
        fn read_global_config(&mut self) -> Result<u32, Self::Error> {
            self.calls.push("read");
            Ok(if self.busy_until.is_none_or(|until| self.now < until) {
                (1 << 1) | (1 << 3)
            } else {
                0
            })
        }
        fn sleep_ms(&mut self, milliseconds: u64) {
            self.now += milliseconds;
        }
        fn reset_vfio_device(&mut self) -> Result<(), Self::Error> {
            self.calls.push("reset");
            if self.reset_fails {
                Err("reset failed")
            } else {
                Ok(())
            }
        }
        fn unmap_all(&mut self) -> Result<(), Self::Error> {
            self.calls.push("unmap");
            Ok(())
        }
    }

    #[test]
    fn pinned_dma_teardown_resets_before_unmapping_after_quiescence() {
        let mut transport = FakePinnedTeardown {
            now: 0,
            busy_until: Some(2),
            reset_fails: false,
            calls: Vec::new(),
        };
        teardown_pinned_dma(&mut transport, |_| {}).unwrap();
        let reset = transport
            .calls
            .iter()
            .position(|call| *call == "reset")
            .unwrap();
        let unmap = transport
            .calls
            .iter()
            .position(|call| *call == "unmap")
            .unwrap();
        assert!(reset < unmap);
    }

    #[test]
    fn pinned_dma_busy_timeout_resets_before_release() {
        let mut transport = FakePinnedTeardown {
            now: 0,
            busy_until: None,
            reset_fails: false,
            calls: Vec::new(),
        };
        assert_eq!(
            teardown_pinned_dma(&mut transport, |_| {}),
            Err(PinnedDmaTeardownError::BusyTimedOut((1 << 1) | (1 << 3)))
        );
        assert_eq!(transport.calls.last_chunk::<2>(), Some(&["reset", "unmap"]));
    }

    #[test]
    fn pinned_dma_reset_failure_never_unmaps() {
        let mut transport = FakePinnedTeardown {
            now: 0,
            busy_until: None,
            reset_fails: true,
            calls: Vec::new(),
        };
        assert_eq!(
            teardown_pinned_dma(&mut transport, |_| {}),
            Err(PinnedDmaTeardownError::Reset("reset failed"))
        );
        assert_eq!(transport.calls.last(), Some(&"reset"));
        assert!(!transport.calls.contains(&"unmap"));
    }

    #[test]
    fn encodes_single_and_paired_dma_segments_like_mt76() {
        let _: fn(DmaSegment, Option<DmaSegment>, u32) -> Result<DmaDescriptor, DescriptorError> =
            DmaDescriptor::tx;
        let _: fn(DmaSegment) -> Result<DmaDescriptor, DescriptorError> = DmaDescriptor::rx;

        let one = mt7921_dma_tx(
            DmaSegment {
                iova: 0x1234_5000,
                len: 0x345,
            },
            None,
            0xaabb_ccdd,
        )
        .unwrap();
        assert_eq!(one.ctrl, 0x4345_0000);
        assert_eq!(one.buf1, 0);
        assert_eq!(one.to_le_bytes()[..4], [0x00, 0x50, 0x34, 0x12]);

        let two = mt7921_dma_tx(
            DmaSegment {
                iova: 0x1000,
                len: 64,
            },
            Some(DmaSegment {
                iova: 0x2000,
                len: 1500,
            }),
            7,
        )
        .unwrap();
        assert_eq!(two.ctrl, (64 << 16) | 1500 | (1 << 14));
        assert_eq!(two.buf1, 0x2000);
        assert_eq!(DmaDescriptor::reset().ctrl, 1 << 31);
    }

    #[test]
    fn builds_owned_mcu_rx_ring_with_one_empty_slot() {
        let ring = prepare_mcu_rx_ring(0x0100_3000, 0x0100_4000).unwrap();
        assert_eq!(ring.producer_index, 7);
        for (index, descriptor) in ring.descriptors[..7].iter().enumerate() {
            assert_eq!(
                *descriptor,
                DmaDescriptor {
                    buf0: 0x0100_4000 + index as u32 * 2048,
                    ctrl: 2048 << 16,
                    buf1: 0,
                    info: 0,
                }
            );
        }
        assert_eq!(ring.descriptors[7], DmaDescriptor::reset());
        assert_eq!(
            prepare_mcu_rx_ring(0x0100_3000, 0x0100_3000),
            Err(DescriptorError::InvalidArena)
        );
    }

    #[test]
    fn rejects_values_the_mt7921_pci_descriptor_cannot_represent() {
        assert_eq!(
            mt7921_dma_tx(
                DmaSegment {
                    iova: 1u64 << 32,
                    len: 1
                },
                None,
                0
            ),
            Err(DescriptorError::IovaAbove32Bits)
        );
        assert_eq!(
            mt7921_dma_tx(
                DmaSegment {
                    iova: 0,
                    len: 0x4000
                },
                None,
                0
            ),
            Err(DescriptorError::SegmentTooLong)
        );
    }

    fn firmware_image(regions: &[(u32, u8, u8, &[u8])]) -> std::vec::Vec<u8> {
        let mut image = vec![];
        for (_, _, _, payload) in regions {
            image.extend_from_slice(payload);
        }
        for (address, feature, kind, payload) in regions {
            let mut record = [0u8; FW_REGION_LEN];
            record[16..20].copy_from_slice(&address.to_le_bytes());
            record[20..24].copy_from_slice(&(payload.len() as u32).to_le_bytes());
            record[24] = *feature;
            record[25] = *kind;
            image.extend_from_slice(&record);
        }
        let mut trailer = [0u8; FW_TRAILER_LEN];
        trailer[0] = 0x79;
        trailer[1] = 2;
        trailer[2] = regions.len() as u8;
        trailer[3] = 3;
        trailer[4] = 4;
        trailer[7..17].copy_from_slice(b"FW-TEST-01");
        trailer[17..32].copy_from_slice(b"20260101-120000");
        trailer[32..36].copy_from_slice(&0xdead_beefu32.to_le_bytes());
        image.extend_from_slice(&trailer);
        image
    }

    fn nic_capability_fixture() -> (Vec<u8>, NicCapability) {
        let mut bytes = vec![0; 4];
        bytes[0..2].copy_from_slice(&4u16.to_le_bytes());
        for (kind, value) in [
            (7u32, vec![0x00, 0x11, 0x22, 0x33, 0x44, 0x55]),
            (8, vec![1, 1, 1, 2, 2, 0, 1, 1, 1, 1, 3, 1]),
            (0x18, vec![1]),
            (0x20, 0x1122_3344_5566_7789u64.to_le_bytes().to_vec()),
        ] {
            bytes.extend_from_slice(&kind.to_le_bytes());
            bytes.extend_from_slice(&(value.len() as u32).to_le_bytes());
            bytes.extend_from_slice(&value);
        }
        (
            bytes,
            NicCapability {
                element_count: 4,
                mac_address: Some([0x00, 0x11, 0x22, 0x33, 0x44, 0x55]),
                phy: Some(NicPhyCapability {
                    ht: true,
                    vht: true,
                    has_5ghz: true,
                    max_bandwidth: 2,
                    spatial_streams: 2,
                    hardware_path: 3,
                    he: true,
                }),
                has_6ghz: Some(true),
                chip_capability: Some(0x1122_3344_5566_7789),
                unknown_elements: 0,
            },
        )
    }

    fn eeprom_hardware_fixture() -> (Vec<u8>, EepromBlock) {
        let mut bytes = vec![0; 24];
        bytes[0..4].copy_from_slice(&MT7921_EEPROM_HW_TYPE_BLOCK.to_le_bytes());
        bytes[4..8].copy_from_slice(&1u32.to_le_bytes());
        bytes[8 + 11] = 1;
        (
            bytes,
            EepromBlock {
                address: MT7921_EEPROM_HW_TYPE_BLOCK,
                valid: 1,
                data: {
                    let mut data = [0; MT7921_EEPROM_BLOCK_SIZE];
                    data[11] = 1;
                    data
                },
            },
        )
    }

    fn clc_fixture() -> Vec<u8> {
        let mut bytes = vec![0; 16];
        bytes[0..4].copy_from_slice(&33u32.to_le_bytes());
        bytes[4] = 0;
        bytes[5] = 1;
        bytes[6] = 1;
        bytes[7] = 1;
        bytes.extend_from_slice(b"00");
        bytes.extend_from_slice(b"-0");
        bytes.extend_from_slice(&11u16.to_le_bytes());
        bytes.extend_from_slice(&[0x5a; 11]);
        bytes
    }

    #[test]
    fn parses_bounded_nic_capability_tlvs() {
        let (bytes, expected) = nic_capability_fixture();
        assert_eq!(parse_nic_capability(&bytes), Ok(expected));
        let channels = candidate_channels(expected);
        assert_eq!(
            candidate_channel_summary(expected),
            CandidateChannelSummary {
                ghz2: 14,
                ghz5: 28,
                ghz6: 59,
            }
        );
        assert_eq!(channels.first().unwrap().frequency_mhz, 2412);
        assert_eq!(channels[13].frequency_mhz, 2484);
        assert_eq!(channels[14].number, 36);
        assert_eq!(channels.last().unwrap().number, 233);
        assert_eq!(
            parse_nic_capability(&bytes[..bytes.len() - 1]),
            Err(NicCapabilityError::TruncatedElement {
                index: 3,
                length: 8,
            })
        );
        assert_eq!(
            parse_nic_capability(&[1, 0, 0, 0]),
            Err(NicCapabilityError::TruncatedElementHeader { index: 0 })
        );
    }

    #[test]
    fn parses_bounded_eeprom_block_and_hardware_type() {
        let (bytes, expected) = eeprom_hardware_fixture();
        assert_eq!(
            parse_eeprom_block(&bytes, MT7921_EEPROM_HW_TYPE_BLOCK),
            Ok(expected)
        );
        assert_eq!(
            expected.hardware_info(),
            Ok(EepromHardwareInfo {
                raw_type: 1,
                encapsulated_calibration: true,
            })
        );
        assert_eq!(
            parse_eeprom_block(&bytes[..23], MT7921_EEPROM_HW_TYPE_BLOCK),
            Err(EepromBlockError::Truncated)
        );
        assert_eq!(
            parse_eeprom_block(&bytes, 0),
            Err(EepromBlockError::AddressMismatch {
                expected: 0,
                actual: MT7921_EEPROM_HW_TYPE_BLOCK,
            })
        );
    }

    #[test]
    fn discovers_selected_clc_rules_without_sending_configuration() {
        let clc = clc_fixture();
        let image = firmware_image(&[(0, FW_FEATURE_NON_DL, FW_TYPE_CLC, &clc)]);
        assert_eq!(
            discover_clc(
                Firmware::parse(&image).unwrap(),
                EepromHardwareInfo {
                    raw_type: 1,
                    encapsulated_calibration: true,
                }
            ),
            Ok(ClcDiscovery {
                segment_count: 1,
                selected_power_segments: 1,
                selected_power_rules: 1,
                channel_segments: 0,
                channel_rules: 0,
                unique_country_codes: 1,
                world_domain_available: true,
            })
        );
        let mut malformed = clc;
        malformed[0..4].copy_from_slice(&34u32.to_le_bytes());
        let image = firmware_image(&[(0, FW_FEATURE_NON_DL, FW_TYPE_CLC, &malformed)]);
        assert_eq!(
            discover_clc(
                Firmware::parse(&image).unwrap(),
                EepromHardwareInfo {
                    raw_type: 1,
                    encapsulated_calibration: true,
                }
            ),
            Err(ClcDiscoveryError::InvalidSegmentLength(34))
        );

        let clc = clc_fixture();
        let image = firmware_image(&[(0, FW_FEATURE_NON_DL, FW_TYPE_CLC, &clc)]);
        let commands = world_clc_commands(
            Firmware::parse(&image).unwrap(),
            EepromHardwareInfo {
                raw_type: 1,
                encapsulated_calibration: true,
            },
            19,
            0,
        )
        .unwrap();
        assert_eq!(
            commands,
            [ClcSetCommand {
                index: 0,
                environment: 1,
                acpi_configuration: 0,
                capability: 1,
                alpha2: *b"00",
                rule_type: *b"-0",
                environment_6ghz: 0,
                mtcl_configuration: 0xff,
                data: vec![0x5a; 11],
            }]
        );
        let encoded = encode_clc_set_command(&commands[0], 6).unwrap();
        assert_eq!(encoded.len(), 64 + 76 + 11);
        assert_eq!(&encoded[36..44], &[0x5c, 0xa0, 1, 6, 0, 0, 0, 0]);
        assert_eq!(&encoded[64..68], &[1, 0, 87, 0]);
        assert_eq!(&encoded[68..76], &[0, 1, 0, 1, b'0', b'0', b'-', b'0']);
        assert_eq!(&encoded[76..78], &[0, 0xff]);
        assert_eq!(&encoded[140..], &[0x5a; 11]);
        assert!(commands[0].expects_response());
        let acpi_commands = world_clc_commands(
            Firmware::parse(&image).unwrap(),
            EepromHardwareInfo {
                raw_type: 1,
                encapsulated_calibration: true,
            },
            19,
            1,
        )
        .unwrap();
        assert_eq!(acpi_commands[0].acpi_configuration, 1);
        assert_eq!(encode_clc_set_command(&acpi_commands[0], 6).unwrap()[70], 1);

        let mut no_event_capability = commands[0].clone();
        no_event_capability.capability = 0;
        assert!(!no_event_capability.expects_response());
        assert!(encode_clc_set_command(&no_event_capability, 6).is_ok());

        let mut zero_mtcl = commands[0].clone();
        zero_mtcl.mtcl_configuration = 0;
        assert_eq!(
            encode_clc_set_command(&zero_mtcl, 6),
            Err(DownloadCommandError::InvalidLength)
        );

        let mut response = vec![0; 72];
        response[4..6].copy_from_slice(&0x1234u16.to_le_bytes());
        response[6..8].copy_from_slice(&68u16.to_le_bytes());
        response[8] = 0x1f;
        assert_eq!(
            parse_clc_set_response(&response),
            Ok(ClcSetResponse {
                tag: 0x1234,
                length: 68,
                special_unii_mask: 0x1f,
            })
        );
        response[8] = 0x20;
        assert_eq!(
            parse_clc_set_response(&response),
            Err(ClcSetResponseError::InvalidMask(0x20))
        );
        assert_eq!(
            parse_clc_set_response(&response[..71]),
            Err(ClcSetResponseError::Truncated)
        );
    }

    #[test]
    fn channel_domain_is_exact_world_indoor_passive_intersection() {
        let capability = nic_capability_fixture().1;
        let command = conservative_channel_domain(capability, *b"00", true, 0).unwrap();
        assert_eq!(command.channels.len(), 39);
        assert_eq!(command.channels[0].number, 1);
        assert_eq!(command.channels[13].number, 14);
        assert_eq!(command.channels[14].number, 36);
        assert_eq!(command.channels.last().unwrap().number, 165);
        assert!(
            command
                .channels
                .iter()
                .all(|channel| channel.flags == 1 << 1)
        );
        assert!(
            !command
                .channels
                .iter()
                .any(|channel| channel.band == PhysicalBand::Ghz6 || channel.number >= 169)
        );

        let encoded = encode_channel_domain_command(&command, 2).unwrap();
        assert_eq!(encoded.len(), CONNAC2_MCU_TXD_BYTES + 12 + 39 * 8);
        assert_eq!(&encoded[36..44], &[0x0f, 0xa0, 1, 2, 0, 0, 0, 0]);
        assert_eq!(
            &encoded[64..76],
            &[b'0', b'0', 0, 0, 0, 3, 3, 0, 14, 25, 0, 0]
        );
        assert_eq!(&encoded[76..84], &[1, 0, 0, 0, 2, 0, 0, 0]);
        assert_eq!(&encoded[188..196], &[36, 0, 0, 0, 2, 0, 0, 0]);
        assert_eq!(
            encode_channel_domain_command(&command, 0),
            Err(ChannelDomainError::InvalidSequence)
        );
    }

    #[test]
    fn channel_domain_policy_and_encoder_fail_closed() {
        let capability = nic_capability_fixture().1;
        assert_eq!(
            conservative_channel_domain(capability, *b"US", true, 0),
            Err(ChannelDomainError::NonWorldDomain)
        );
        assert_eq!(
            conservative_channel_domain(capability, *b"00", false, 0),
            Err(ChannelDomainError::OutdoorEnvironment)
        );
        assert_eq!(
            conservative_channel_domain(capability, *b"00", true, 1),
            Err(ChannelDomainError::NonzeroSpecialUniiMask)
        );
        let mut command = conservative_channel_domain(capability, *b"00", true, 0).unwrap();
        command.channels[14].number = 169;
        assert_eq!(
            encode_channel_domain_command(&command, 1),
            Err(ChannelDomainError::InvalidChannelSet)
        );
        let mut command = conservative_channel_domain(capability, *b"00", true, 0).unwrap();
        command.channels[0].flags = 0;
        assert_eq!(
            encode_channel_domain_command(&command, 1),
            Err(ChannelDomainError::InvalidChannelSet)
        );
    }

    #[test]
    fn passive_command_closure_is_source_exact_and_has_no_tx_material() {
        let channel = CandidateChannel {
            band: PhysicalBand::Ghz2,
            number: 1,
            frequency_mhz: 2412,
        };
        let eeprom = encode_passive_mcu_command(&PassiveMcuCommand::EepromBufferMode, 1).unwrap();
        assert_eq!(&eeprom[36..44], &[0xed, 0xa0, 1, 1, 0, 0x21, 0, 1]);
        assert_eq!(&eeprom[64..], &[1, 0, 0, 0]);
        let switch = encode_passive_mcu_command(
            &PassiveMcuCommand::ChannelSwitch {
                channel,
                center_channel: channel.number as u8,
                bandwidth: 0,
                center_channel2: 0,
                antenna_mask: 3,
            },
            2,
        )
        .unwrap();
        assert_eq!(&switch[36..44], &[0xed, 0xa0, 1, 2, 0, 8, 0, 1]);
        assert_eq!(&switch[64..70], &[1, 1, 0, 2, 2, 9]);
        let channel36 = CandidateChannel {
            band: PhysicalBand::Ghz5,
            number: 36,
            frequency_mhz: 5180,
        };
        let wide = encode_passive_mcu_command(
            &PassiveMcuCommand::ChannelSwitch {
                channel: channel36,
                center_channel: 38,
                bandwidth: 1,
                center_channel2: 0,
                antenna_mask: 3,
            },
            3,
        )
        .unwrap();
        assert_eq!(&wide[64..72], &[36, 38, 1, 2, 2, 9, 0, 0]);

        let scan = encode_passive_mcu_command(
            &PassiveMcuCommand::StartScan {
                scan_sequence: 1,
                channel,
            },
            3,
        )
        .unwrap();
        let request = &scan[64..];
        assert_eq!(request.len(), 1186);
        assert_eq!(&request[..8], &[1, 0, 0, 1, 0, 0, 1 << 5, 1]);
        assert_eq!(&request[152..160], &[0, 0, 0, 0, 0, 0, 4, 1]);
        assert_eq!(&request[160..162], &[1, 1]);
        assert_eq!(request[224..826].iter().copied().sum::<u8>(), 0);
        assert_eq!(request[826], 0);
        assert_eq!(request[1185], 0);
        assert!(
            !PassiveMcuCommand::StartScan {
                scan_sequence: 1,
                channel
            }
            .expects_response()
        );

        let channel_5ghz = CandidateChannel {
            band: PhysicalBand::Ghz5,
            number: 36,
            frequency_mhz: 5180,
        };
        let switch_5ghz = encode_passive_mcu_command(
            &PassiveMcuCommand::ChannelSwitch {
                channel: channel_5ghz,
                center_channel: channel_5ghz.number as u8,
                bandwidth: 0,
                center_channel2: 0,
                antenna_mask: 3,
            },
            4,
        )
        .unwrap();
        assert_eq!(&switch_5ghz[64..75], &[36, 36, 0, 2, 2, 9, 0, 0, 0, 0, 1]);
        let scan_5ghz = encode_passive_mcu_command(
            &PassiveMcuCommand::StartScan {
                scan_sequence: 2,
                channel: channel_5ghz,
            },
            5,
        )
        .unwrap();
        assert_eq!(&scan_5ghz[64 + 158..64 + 162], &[4, 1, 2, 36]);

        let forbidden = CandidateChannel {
            band: PhysicalBand::Ghz5,
            number: 169,
            frequency_mhz: 5845,
        };
        assert_eq!(
            encode_passive_mcu_command(
                &PassiveMcuCommand::StartScan {
                    scan_sequence: 1,
                    channel: forbidden,
                },
                1,
            ),
            Err(PassiveMcuCommandError::UnsupportedChannel)
        );
    }

    #[test]
    fn parses_only_passive_scan_done_and_beacon_advertisements() {
        let mut done = vec![0; 56];
        done[24..26].copy_from_slice(&32u16.to_le_bytes());
        done[26..28].copy_from_slice(&0xa0u16.to_le_bytes());
        done[28] = 0x0d;
        done[36] = 1;
        done[40] = 1;
        done[44..48].copy_from_slice(&3u32.to_le_bytes());
        done[53..55].copy_from_slice(b"00");
        assert_eq!(
            parse_passive_scan_done(&done),
            Ok(PassiveScanDone {
                scan_sequence: 1,
                completed_channels: 1,
                beacon_scan_count: 3,
                alpha2: *b"00",
            })
        );

        let mut rx = vec![0; 24 + 8 + 36 + 5];
        let rxd0 = (2u32 << 27) | rx.len() as u32;
        rx[0..4].copy_from_slice(&rxd0.to_le_bytes());
        // Connac2 BAND_IDX is bit 28 and must not be classified as an RX error.
        rx[4..8].copy_from_slice(&((1u32 << 13) | (1 << 28)).to_le_bytes());
        rx[12..16].copy_from_slice(&(1u32 << 8).to_le_bytes());
        rx[28..32].copy_from_slice(&0x7878u32.to_le_bytes());
        let frame = &mut rx[32..];
        frame[0..2].copy_from_slice(&0x0080u16.to_le_bytes());
        frame[16..22].copy_from_slice(&[1, 2, 3, 4, 5, 6]);
        frame[32..34].copy_from_slice(&100u16.to_le_bytes());
        frame[34..36].copy_from_slice(&0x0431u16.to_le_bytes());
        frame[36..].copy_from_slice(&[0, 3, b'a', b'p', b'1']);
        assert_eq!(
            parse_passive_advertisement(&rx),
            Ok(PassiveAdvertisement {
                probe_response: false,
                bssid: [1, 2, 3, 4, 5, 6],
                beacon_interval_tu: 100,
                capability_info: 0x0431,
                ies: vec![0, 3, b'a', b'p', b'1'],
                band: PhysicalBand::Ghz2,
                channel: 1,
                rssi_dbm: -50,
            })
        );
        let mut normal_mcu = rx.clone();
        let rxd0 = (7u32 << 27) | (1 << 16) | normal_mcu.len() as u32;
        normal_mcu[0..4].copy_from_slice(&rxd0.to_le_bytes());
        assert_eq!(
            parse_passive_advertisement(&normal_mcu),
            parse_passive_advertisement(&rx)
        );
        let mut rx_5ghz = rx.clone();
        rx_5ghz[12..16].copy_from_slice(&(36u32 << 8).to_le_bytes());
        let mut expected_5ghz = parse_passive_advertisement(&rx).unwrap();
        expected_5ghz.band = PhysicalBand::Ghz5;
        expected_5ghz.channel = 36;
        assert_eq!(parse_passive_advertisement(&rx_5ghz), Ok(expected_5ghz));
        let mut stale_tail = rx.clone();
        stale_tail[0..4].copy_from_slice(&((2u32 << 27) | 68).to_le_bytes());
        let mut without_tail = parse_passive_advertisement(&rx).unwrap();
        without_tail.ies.clear();
        assert_eq!(parse_passive_advertisement(&stale_tail), Ok(without_tail));
        rx[32..34].copy_from_slice(&0x0008u16.to_le_bytes());
        assert_eq!(
            parse_passive_advertisement(&rx),
            Err(PassiveRxError::UnsupportedFrame)
        );
    }

    #[test]
    fn passive_advertisement_and_client_frame_share_exact_connac_envelope_parsing() {
        let mut data = vec![0; 24 + 8 + 36 + 5];
        data[4..8].copy_from_slice(&(1u32 << 13).to_le_bytes());
        data[12..16].copy_from_slice(&(1u32 << 8).to_le_bytes());
        data[28..32].copy_from_slice(&0x7878u32.to_le_bytes());
        let frame = &mut data[32..];
        frame[0..2].copy_from_slice(&0x0080u16.to_le_bytes());
        frame[16..22].copy_from_slice(&[1, 2, 3, 4, 5, 6]);
        frame[32..34].copy_from_slice(&100u16.to_le_bytes());
        frame[34..36].copy_from_slice(&0x0431u16.to_le_bytes());
        frame[36..].copy_from_slice(&[0, 3, b'a', b'p', b'1']);

        for packet in [2u32 << 27, (7u32 << 27) | (1 << 16)] {
            for with_group_5 in [false, true] {
                let mut envelope = if with_group_5 {
                    let mut envelope = data[..32].to_vec();
                    envelope.resize(32 + 72, 0);
                    envelope[56..60].copy_from_slice(&0x6464u32.to_le_bytes());
                    envelope.extend_from_slice(&data[32..]);
                    envelope[4..8].copy_from_slice(&((1u32 << 13) | (1 << 15)).to_le_bytes());
                    envelope
                } else {
                    data.clone()
                };
                let len = envelope.len() as u32;
                envelope[0..4].copy_from_slice(&(packet | len).to_le_bytes());

                let stripped = parse_connac2_rx_frame(&envelope).unwrap();
                let advertisement = parse_passive_advertisement(&envelope).unwrap();
                assert_eq!(advertisement.band, stripped.band);
                assert_eq!(advertisement.channel, stripped.channel);
                assert_eq!(advertisement.rssi_dbm, stripped.rssi_dbm);
                assert_eq!(advertisement.rssi_dbm, if with_group_5 { -60 } else { -50 });
                assert_eq!(advertisement.ies, stripped.bytes[36..]);
            }
        }
    }

    #[test]
    fn connac2_group1_follows_group4_and_descriptor_padding_is_not_frame_data() {
        // Linux 7.1.5 mt7921_mac_fill_rx consumes GROUP4 before GROUP1, then
        // GROUP2, GROUP3 and the two-byte RXD2 header offset. The DMA length
        // may include bytes beyond RXD0's reported packet length.
        let metadata_len = 24 + 16 + 16 + 8 + 8 + 2;
        let frame_len = 24 + 10;
        let reported_len = metadata_len + frame_len;
        let mut rx = vec![0xcc; 140];
        rx[0..4].copy_from_slice(&((2u32 << 27) | reported_len as u32).to_le_bytes());
        rx[4..8].copy_from_slice(&((1u32 << 14) | (1 << 11) | (1 << 12) | (1 << 13)).to_le_bytes());
        rx[8..12].copy_from_slice(&(1u32 << 14).to_le_bytes());
        rx[12..16].copy_from_slice(&(36u32 << 8).to_le_bytes());
        rx[24..40].fill(0xa5); // GROUP4 is not a packet number.
        rx[40..46].copy_from_slice(&[6, 5, 4, 3, 2, 1]);
        rx[68..72].copy_from_slice(&0x7878u32.to_le_bytes());
        rx[72..74].fill(0xee);
        {
            let frame = &mut rx[metadata_len..reported_len];
            frame[0..2].copy_from_slice(&0x0010u16.to_le_bytes());
            frame[24..34].copy_from_slice(&[1, 0, 0, 0, 42, 0, 1, 2, 0x82, 0x84]);
        }

        let parsed = parse_connac2_rx_frame(&rx).unwrap();
        assert_eq!(parsed.pn, Some([1, 2, 3, 4, 5, 6]));
        assert_eq!(parsed.bytes, rx[metadata_len..reported_len]);
        assert_eq!(parsed.bytes.len(), frame_len);
    }

    #[test]
    fn connac2_rejects_each_truncated_valid_group_and_header_padding() {
        for (rxd1, rxd2, len) in [
            (1u32 << 14, 0, 39),
            (1u32 << 11, 0, 39),
            (1u32 << 12, 0, 31),
            (1u32 << 13, 0, 31),
            ((1u32 << 13) | (1 << 15), 0, 103),
            (1u32 << 13, 1u32 << 14, 33),
        ] {
            let mut rx = vec![0; len];
            rx[0..4].copy_from_slice(&((2u32 << 27) | len as u32).to_le_bytes());
            rx[4..8].copy_from_slice(&rxd1.to_le_bytes());
            rx[8..12].copy_from_slice(&rxd2.to_le_bytes());
            rx[12..16].copy_from_slice(&(36u32 << 8).to_le_bytes());
            assert_eq!(
                parse_connac2_rx_frame(&rx),
                Err(PassiveRxError::Truncated),
                "rxd1={rxd1:#x} rxd2={rxd2:#x} len={len}"
            );
        }
    }

    #[test]
    fn strips_connac2_rx_metadata_without_interpreting_sae_fields() {
        let mut rx = vec![0; 24 + 8 + 30 + 4];
        let rxd0 = (2u32 << 27) | rx.len() as u32;
        rx[0..4].copy_from_slice(&rxd0.to_le_bytes());
        rx[4..8].copy_from_slice(&(1u32 << 13).to_le_bytes());
        rx[8..12].copy_from_slice(&(1u32 << 25).to_le_bytes());
        rx[12..16].copy_from_slice(&(36u32 << 8).to_le_bytes());
        let frame = &mut rx[32..];
        frame[0..2].copy_from_slice(&0x00b0u16.to_le_bytes());
        frame[4..10].copy_from_slice(&[2; 6]);
        frame[10..16].copy_from_slice(&[6; 6]);
        frame[16..22].copy_from_slice(&[6; 6]);
        frame[24..26].copy_from_slice(&3u16.to_le_bytes());
        frame[26..28].copy_from_slice(&1u16.to_le_bytes());
        frame[28..30].copy_from_slice(&0u16.to_le_bytes());
        frame[30..34].copy_from_slice(&[9, 8, 7, 6]);
        assert_eq!(parse_connac2_rx_frame(&rx).unwrap().bytes, rx[32..]);
        assert_eq!(
            parse_mt7921_auth_rx(&rx),
            Ok(Mt7921AuthRx {
                receiver: [2; 6],
                transmitter: [6; 6],
                bssid: [6; 6],
                algorithm: 3,
                sequence: 1,
                status: 0,
                fields: vec![9, 8, 7, 6],
            })
        );
        let mut stale_tail = rx.clone();
        stale_tail[0..4].copy_from_slice(&((2u32 << 27) | 62).to_le_bytes());
        assert_eq!(
            parse_mt7921_auth_rx(&stale_tail),
            Ok(Mt7921AuthRx {
                receiver: [2; 6],
                transmitter: [6; 6],
                bssid: [6; 6],
                algorithm: 3,
                sequence: 1,
                status: 0,
                fields: Vec::new(),
            })
        );
        let mut ordered = rx.clone();
        ordered[32..34].copy_from_slice(&0x80b0u16.to_le_bytes());
        assert_eq!(
            parse_mt7921_auth_rx(&ordered),
            Err(PassiveRxError::UnsupportedFrame)
        );
    }

    #[test]
    fn passive_mac_mmio_plan_covers_every_mandatory_source_write() {
        let plan = passive_mac_mmio_plan();
        assert_eq!(plan.len(), 41);
        assert_eq!(
            &plan[..3],
            &[
                PassiveMacMmioOperation::Rmw {
                    address: 0x820c_d004,
                    mask: 0xfff8,
                    value: 1536 << 3,
                },
                PassiveMacMmioOperation::Rmw {
                    address: 0x820c_d000,
                    mask: 1 << 15,
                    value: 1 << 15,
                },
                PassiveMacMmioOperation::Rmw {
                    address: 0x820c_d000,
                    mask: 1 << 19,
                    value: 1 << 19,
                },
            ]
        );
        for (index, operation) in plan[3..23].iter().enumerate() {
            assert_eq!(
                *operation,
                PassiveMacMmioOperation::WtblClear {
                    index: index as u8,
                    address: 0x820d_4230,
                    value: index as u32 | (1 << 12),
                    busy_mask: 1 << 31,
                    timeout_us: 5000,
                }
            );
        }
        assert_eq!(
            plan.last(),
            Some(&PassiveMacMmioOperation::Rmw {
                address: 0x820f_9008,
                mask: (3 << 30) | (3 << 24),
                value: 3 << 24,
            })
        );
    }

    #[test]
    fn passive_mac_addresses_use_exact_fixed_bar_map_and_fail_closed() {
        assert_eq!(
            passive_mac_source_rmw_value(0x1234_5678, 0x00ff_0000, 0x005a_0000),
            0x125a_5678
        );
        let fixtures = [
            (0x820c_d000, 0x0f000),
            (0x820c_d004, 0x0f004),
            (0x820d_4230, 0x34230),
            (0x820d_8700, 0x38700),
            (0x820d_8704, 0x38704),
            (0x820e_40f4, 0x210f4),
            (0x820e_5380, 0x21780),
            (0x820e_53c4, 0x217c4),
            (0x820e_7000, 0x21e00),
            (0x820e_9008, 0x23408),
            (0x820e_d004, 0x24804),
            (0x820f_40f4, 0xa10f4),
            (0x820f_5380, 0xa1780),
            (0x820f_53c4, 0xa17c4),
            (0x820f_7000, 0xa1e00),
            (0x820f_9008, 0xa3408),
            (0x820f_d004, 0xa4804),
        ];
        for &(physical, bar) in &fixtures {
            assert_eq!(passive_mac_bar_offset(physical), Ok(bar));
            assert_eq!(
                validate_passive_mac_bar_read(physical, 0x1234_5678),
                Ok((bar, 0x1234_5678))
            );
            assert_eq!(
                validate_passive_mac_bar_read(physical, u32::MAX),
                Err(PassiveMacBarError::AllOnes { address: physical })
            );
        }
        for operation in passive_mac_mmio_plan() {
            let address = match operation {
                PassiveMacMmioOperation::Rmw { address, .. }
                | PassiveMacMmioOperation::WtblClear { address, .. } => address,
            };
            assert!(fixtures.iter().any(|fixture| fixture.0 == address));
        }
        assert_eq!(
            passive_mac_bar_offset(0x820e_40f8),
            Err(PassiveMacBarError::UnsupportedAddress(0x820e_40f8))
        );
        assert_eq!(
            passive_mac_bar_offset(0x1800_0000),
            Err(PassiveMacBarError::UnsupportedAddress(0x1800_0000))
        );
    }

    #[derive(Clone, Debug, Eq, PartialEq)]
    enum LoaderTrace {
        Command(DownloadCommand, u8),
        PublishScatter(FirmwareImagePart, u8, usize),
        ScatterCompletion(FirmwareImagePart, u8, u64),
        DownloadState,
        N9Ready,
        Sleep(u64),
        Cleanup(FirmwareLoaderState),
        SetClc(u8, u8),
        SetChannelDomain(usize, u8),
        PassiveHook,
    }

    struct FakeFirmwareLoader {
        trace: Vec<LoaderTrace>,
        calls: usize,
        sequence: u8,
        now_ms: u64,
        fail_at: Option<usize>,
        download_states: Vec<u8>,
        download_poll: usize,
        n9_states: Vec<bool>,
        n9_poll: usize,
        semaphore_result: u8,
        completion_override: Option<(DownloadCommand, FirmwareCommandCompletion)>,
        fail_release: bool,
        fail_patch_publish: bool,
        fail_patch_completion: bool,
        clc_mask: u8,
    }

    impl Default for FakeFirmwareLoader {
        fn default() -> Self {
            Self {
                trace: vec![],
                calls: 0,
                sequence: 0,
                now_ms: 0,
                fail_at: None,
                download_states: vec![0, 1],
                download_poll: 0,
                n9_states: vec![false, true],
                n9_poll: 0,
                semaphore_result: 2,
                completion_override: None,
                fail_release: false,
                fail_patch_publish: false,
                fail_patch_completion: false,
                clc_mask: 0x1f,
            }
        }
    }

    impl FakeFirmwareLoader {
        fn step(&mut self) -> Result<(), &'static str> {
            self.calls += 1;
            if self.fail_at == Some(self.calls) {
                Err("injected transport failure")
            } else {
                Ok(())
            }
        }

        fn next_download_state(&mut self) -> u8 {
            let state = self
                .download_states
                .get(self.download_poll)
                .copied()
                .unwrap_or_else(|| *self.download_states.last().unwrap_or(&0));
            self.download_poll += 1;
            state
        }

        fn next_n9_state(&mut self) -> bool {
            let ready = self
                .n9_states
                .get(self.n9_poll)
                .copied()
                .unwrap_or_else(|| *self.n9_states.last().unwrap_or(&false));
            self.n9_poll += 1;
            ready
        }
    }

    impl FirmwareLoaderTransport for FakeFirmwareLoader {
        type Error = &'static str;

        fn next_sequence(&mut self) -> u8 {
            self.sequence = (self.sequence + 1) & 0x0f;
            if self.sequence == 0 {
                self.sequence = 1;
            }
            self.sequence
        }

        fn acpi_configuration(&self) -> u8 {
            0
        }

        fn command(
            &mut self,
            command: DownloadCommand,
            sequence: u8,
            encoded: &[u8],
        ) -> Result<FirmwareCommandCompletion, Self::Error> {
            assert_eq!(encoded[39], sequence);
            assert_eq!(&encoded[34..36], &0x8000u16.to_le_bytes());
            self.trace.push(LoaderTrace::Command(command, sequence));
            self.step()?;
            if self.fail_release && command == DownloadCommand::PatchSemaphoreRelease {
                return Err("injected release failure");
            }
            if let Some((overridden, completion)) = self.completion_override
                && overridden == command
            {
                return Ok(completion);
            }
            Ok(match command {
                DownloadCommand::NicPowerControl => FirmwareCommandCompletion::NoResponse,
                DownloadCommand::GetNicCapability => {
                    FirmwareCommandCompletion::NicCapability(nic_capability_fixture().1)
                }
                DownloadCommand::ReadEepromBlock { .. } => {
                    FirmwareCommandCompletion::EepromBlock(eeprom_hardware_fixture().1)
                }
                DownloadCommand::PatchSemaphoreGet => {
                    FirmwareCommandCompletion::PatchSemaphore(self.semaphore_result.into())
                }
                DownloadCommand::PatchSemaphoreRelease => {
                    FirmwareCommandCompletion::PatchSemaphore(PatchSemaphoreStatus::Released)
                }
                DownloadCommand::PatchFinish => FirmwareCommandCompletion::PatchFinish(0),
                DownloadCommand::PatchStart { .. }
                | DownloadCommand::TargetAddressLength { .. }
                | DownloadCommand::FirmwareStart { .. } => FirmwareCommandCompletion::Ack,
            })
        }

        fn set_clc(
            &mut self,
            command: &ClcSetCommand,
            sequence: u8,
            encoded: &[u8],
        ) -> Result<Option<ClcSetResponse>, Self::Error> {
            assert_eq!(encoded[39], sequence);
            assert_eq!(&encoded[36..39], &[0x5c, 0xa0, 1]);
            self.trace
                .push(LoaderTrace::SetClc(command.index, sequence));
            self.step()?;
            Ok(command.expects_response().then_some(ClcSetResponse {
                tag: 0,
                length: 68,
                special_unii_mask: self.clc_mask,
            }))
        }

        fn set_channel_domain(
            &mut self,
            command: &ChannelDomainCommand,
            sequence: u8,
            encoded: &[u8],
        ) -> Result<(), Self::Error> {
            assert_eq!(encoded[39], sequence);
            assert_eq!(&encoded[36..39], &[0x0f, 0xa0, 1]);
            self.trace.push(LoaderTrace::SetChannelDomain(
                command.channels.len(),
                sequence,
            ));
            self.step()
        }

        fn publish_scatter(
            &mut self,
            part: FirmwareImagePart,
            sequence: u8,
            chunk: &[u8],
        ) -> Result<(), Self::Error> {
            assert!(!chunk.is_empty());
            assert!(chunk.len() <= MT7921_FWDL_CHUNK_BYTES);
            self.trace
                .push(LoaderTrace::PublishScatter(part, sequence, chunk.len()));
            self.step()?;
            if self.fail_patch_publish && part == FirmwareImagePart::Patch {
                Err("injected patch publish failure")
            } else {
                Ok(())
            }
        }

        fn wait_scatter_completion(
            &mut self,
            part: FirmwareImagePart,
            sequence: u8,
            deadline_ms: u64,
        ) -> Result<(), Self::Error> {
            assert_eq!(
                deadline_ms,
                self.now_ms.saturating_add(SCATTER_COMPLETION_TIMEOUT_MS)
            );
            self.trace
                .push(LoaderTrace::ScatterCompletion(part, sequence, deadline_ms));
            self.step()?;
            if self.fail_patch_completion && part == FirmwareImagePart::Patch {
                Err("injected patch completion timeout")
            } else {
                Ok(())
            }
        }

        fn firmware_download_state(&mut self) -> Result<u8, Self::Error> {
            self.trace.push(LoaderTrace::DownloadState);
            self.step()?;
            Ok(self.next_download_state())
        }

        fn firmware_n9_ready(&mut self) -> Result<bool, Self::Error> {
            self.trace.push(LoaderTrace::N9Ready);
            self.step()?;
            Ok(self.next_n9_state())
        }

        fn now_ms(&self) -> u64 {
            self.now_ms
        }

        fn sleep_ms(&mut self, duration_ms: u64) {
            self.trace.push(LoaderTrace::Sleep(duration_ms));
            self.now_ms = self.now_ms.saturating_add(duration_ms);
        }

        fn fail_closed_cleanup(&mut self, state: FirmwareLoaderState) -> Result<(), Self::Error> {
            self.trace.push(LoaderTrace::Cleanup(state));
            self.step()
        }
    }

    fn loader_images() -> (Vec<u8>, Vec<u8>) {
        let patch = patch_image(0x0004_0002, 160, 4097);
        let ram_large = vec![0x5a; 4097];
        let clc = clc_fixture();
        let ram = firmware_image(&[
            (0x0091_5000, 1 << 5, 0, &ram_large),
            (0x0201_5c00, 0, 0, b"ram"),
            (0, FW_FEATURE_NON_DL, FW_TYPE_CLC, &clc),
        ]);
        (patch, ram)
    }

    #[test]
    fn clc_without_event_capability_advances_without_response() {
        let mut transport = FakeFirmwareLoader::default();
        let command = ClcSetCommand {
            index: 0,
            environment: 1,
            acpi_configuration: 0,
            capability: 0,
            alpha2: *b"00",
            rule_type: *b"-0",
            environment_6ghz: 0,
            mtcl_configuration: 0xff,
            data: vec![0x5a; 11],
        };
        assert_eq!(loader_set_clc(&mut transport, &command), Ok(None));
        assert!(matches!(
            transport.trace.as_slice(),
            [LoaderTrace::SetClc(0, 1)]
        ));
    }

    #[test]
    fn firmware_loader_matches_linux_transaction_golden_trace() {
        let (patch_bytes, ram_bytes) = loader_images();
        let mut transport = FakeFirmwareLoader::default();
        let report = load_mt7921_firmware(
            &mut transport,
            Patch::parse(&patch_bytes).unwrap(),
            Firmware::parse(&ram_bytes).unwrap(),
        )
        .unwrap();
        assert_eq!(
            report,
            FirmwareLoaderReport {
                download_ready_observed: true,
                patch: PatchDisposition::Downloaded,
                patch_sections: 1,
                ram_regions: 2,
                scatter_chunks: 5,
                scatter_bytes: 8197,
                nic_capability: nic_capability_fixture().1,
                candidate_channels: CandidateChannelSummary {
                    ghz2: 14,
                    ghz5: 28,
                    ghz6: 59,
                },
                eeprom_hardware: eeprom_hardware_fixture().1,
                clc: ClcDiscovery {
                    segment_count: 1,
                    selected_power_segments: 1,
                    selected_power_rules: 1,
                    channel_segments: 0,
                    channel_rules: 0,
                    unique_country_codes: 1,
                    world_domain_available: true,
                },
                clc_rules_applied: 1,
                special_unii_mask: 0x1f,
            }
        );
        assert_eq!(
            transport.trace,
            [
                LoaderTrace::Command(DownloadCommand::NicPowerControl, 1),
                LoaderTrace::DownloadState,
                LoaderTrace::Sleep(10),
                LoaderTrace::DownloadState,
                LoaderTrace::Command(DownloadCommand::PatchSemaphoreGet, 2),
                LoaderTrace::Command(
                    DownloadCommand::PatchStart {
                        address: 0x0090_0000,
                        length: 4097,
                        mode: DL_MODE_NEED_RESPONSE,
                    },
                    3,
                ),
                LoaderTrace::PublishScatter(FirmwareImagePart::Patch, 4, 4096),
                LoaderTrace::ScatterCompletion(FirmwareImagePart::Patch, 4, 3010),
                LoaderTrace::PublishScatter(FirmwareImagePart::Patch, 5, 1),
                LoaderTrace::ScatterCompletion(FirmwareImagePart::Patch, 5, 3010),
                LoaderTrace::Command(DownloadCommand::PatchFinish, 6),
                LoaderTrace::Command(DownloadCommand::PatchSemaphoreRelease, 7),
                LoaderTrace::Command(
                    DownloadCommand::TargetAddressLength {
                        address: 0x0091_5000,
                        length: 4097,
                        mode: DL_MODE_NEED_RESPONSE,
                    },
                    8,
                ),
                LoaderTrace::PublishScatter(FirmwareImagePart::Ram, 9, 4096),
                LoaderTrace::ScatterCompletion(FirmwareImagePart::Ram, 9, 3010),
                LoaderTrace::PublishScatter(FirmwareImagePart::Ram, 10, 1),
                LoaderTrace::ScatterCompletion(FirmwareImagePart::Ram, 10, 3010),
                LoaderTrace::Command(
                    DownloadCommand::TargetAddressLength {
                        address: 0x0201_5c00,
                        length: 3,
                        mode: DL_MODE_NEED_RESPONSE,
                    },
                    11,
                ),
                LoaderTrace::PublishScatter(FirmwareImagePart::Ram, 12, 3),
                LoaderTrace::ScatterCompletion(FirmwareImagePart::Ram, 12, 3010),
                LoaderTrace::Command(
                    DownloadCommand::FirmwareStart {
                        address: 0x0091_5000,
                        option: 1,
                    },
                    13,
                ),
                LoaderTrace::N9Ready,
                LoaderTrace::Sleep(10),
                LoaderTrace::N9Ready,
                LoaderTrace::Command(DownloadCommand::GetNicCapability, 14),
                LoaderTrace::Command(
                    DownloadCommand::ReadEepromBlock {
                        address: MT7921_EEPROM_HW_TYPE_BLOCK,
                    },
                    15,
                ),
                LoaderTrace::SetClc(0, 1),
                LoaderTrace::Cleanup(FirmwareLoaderState::Ready),
            ]
        );
    }

    #[test]
    fn firmware_bootstrap_stops_after_n9_capability_before_eeprom_or_clc() {
        let (patch_bytes, ram_bytes) = loader_images();
        let mut transport = FakeFirmwareLoader::default();
        let report = load_mt7921_firmware_bootstrap(
            &mut transport,
            Patch::parse(&patch_bytes).unwrap(),
            Firmware::parse(&ram_bytes).unwrap(),
        )
        .unwrap();

        assert!(report.download_ready_observed);
        assert_eq!(report.nic_capability, nic_capability_fixture().1);
        assert_eq!(report.eeprom_hardware.valid, 0);
        assert!(transport.trace.iter().any(|event| matches!(
            event,
            LoaderTrace::Command(DownloadCommand::GetNicCapability, _)
        )));
        assert!(!transport.trace.iter().any(|event| matches!(
            event,
            LoaderTrace::Command(DownloadCommand::ReadEepromBlock { .. }, _)
                | LoaderTrace::SetClc(..)
                | LoaderTrace::SetChannelDomain(..)
        )));
        assert!(matches!(
            transport.trace.last(),
            Some(LoaderTrace::Cleanup(FirmwareLoaderState::Ready))
        ));
    }

    #[test]
    fn channel_domain_loader_boundary_is_separate_and_cleans_up() {
        let (patch_bytes, ram_bytes) = loader_images();
        let mut transport = FakeFirmwareLoader {
            clc_mask: 0,
            ..Default::default()
        };
        let report = load_mt7921_firmware_through_channel_domain(
            &mut transport,
            Patch::parse(&patch_bytes).unwrap(),
            Firmware::parse(&ram_bytes).unwrap(),
        )
        .unwrap();
        assert_eq!(report.special_unii_mask, 0);
        assert!(matches!(
            &transport.trace[transport.trace.len() - 2..],
            [
                LoaderTrace::SetChannelDomain(39, 2),
                LoaderTrace::Cleanup(FirmwareLoaderState::Ready)
            ]
        ));

        let mut rejected = FakeFirmwareLoader::default();
        assert!(matches!(
            load_mt7921_firmware_through_channel_domain(
                &mut rejected,
                Patch::parse(&patch_bytes).unwrap(),
                Firmware::parse(&ram_bytes).unwrap(),
            ),
            Err(FirmwareLoaderError::Failed(
                FirmwareLoaderFailure::ChannelDomain(ChannelDomainError::NonzeroSpecialUniiMask)
            ))
        ));
        assert!(
            !rejected
                .trace
                .iter()
                .any(|event| matches!(event, LoaderTrace::SetChannelDomain(_, _)))
        );
        assert!(matches!(
            rejected.trace.last(),
            Some(LoaderTrace::Cleanup(FirmwareLoaderState::ClcConfigured))
        ));

        let mut failed = FakeFirmwareLoader {
            clc_mask: 0,
            fail_at: Some(transport.calls - 1),
            ..Default::default()
        };
        assert!(matches!(
            load_mt7921_firmware_through_channel_domain(
                &mut failed,
                Patch::parse(&patch_bytes).unwrap(),
                Firmware::parse(&ram_bytes).unwrap(),
            ),
            Err(FirmwareLoaderError::Failed(
                FirmwareLoaderFailure::Transport {
                    operation: FirmwareLoaderOperation::SetChannelDomain,
                    source: "injected transport failure",
                }
            ))
        ));
        assert!(matches!(
            failed.trace.last(),
            Some(LoaderTrace::Cleanup(FirmwareLoaderState::ClcConfigured))
        ));
    }

    #[test]
    fn passive_hook_is_inside_mandatory_loader_cleanup() {
        let (patch_bytes, ram_bytes) = loader_images();
        let mut transport = FakeFirmwareLoader {
            clc_mask: 0,
            ..Default::default()
        };
        let report = load_mt7921_firmware_with_passive_boundary(
            &mut transport,
            Patch::parse(&patch_bytes).unwrap(),
            Firmware::parse(&ram_bytes).unwrap(),
            |transport, report| {
                assert_eq!(report.special_unii_mask, 0);
                transport.trace.push(LoaderTrace::PassiveHook);
                Ok(())
            },
        )
        .unwrap();
        assert_eq!(report.special_unii_mask, 0);
        assert!(matches!(
            &transport.trace[transport.trace.len() - 3..],
            [
                LoaderTrace::SetChannelDomain(39, _),
                LoaderTrace::PassiveHook,
                LoaderTrace::Cleanup(FirmwareLoaderState::Ready)
            ]
        ));

        let mut failed = FakeFirmwareLoader {
            clc_mask: 0,
            ..Default::default()
        };
        assert!(matches!(
            load_mt7921_firmware_with_passive_boundary(
                &mut failed,
                Patch::parse(&patch_bytes).unwrap(),
                Firmware::parse(&ram_bytes).unwrap(),
                |transport, _| {
                    transport.trace.push(LoaderTrace::PassiveHook);
                    Err("passive failure")
                },
            ),
            Err(FirmwareLoaderError::Failed(
                FirmwareLoaderFailure::Transport {
                    operation: FirmwareLoaderOperation::PassiveBoundary,
                    source: "passive failure",
                }
            ))
        ));
        assert!(matches!(
            failed.trace.last(),
            Some(LoaderTrace::Cleanup(FirmwareLoaderState::Ready))
        ));
    }

    #[test]
    fn firmware_loader_injected_transport_failures_always_cleanup_and_release() {
        let (patch_bytes, ram_bytes) = loader_images();
        let mut baseline = FakeFirmwareLoader::default();
        load_mt7921_firmware(
            &mut baseline,
            Patch::parse(&patch_bytes).unwrap(),
            Firmware::parse(&ram_bytes).unwrap(),
        )
        .unwrap();
        for fail_at in 1..=baseline.calls {
            let mut transport = FakeFirmwareLoader {
                fail_at: Some(fail_at),
                ..Default::default()
            };
            assert!(
                load_mt7921_firmware(
                    &mut transport,
                    Patch::parse(&patch_bytes).unwrap(),
                    Firmware::parse(&ram_bytes).unwrap(),
                )
                .is_err()
            );
            assert!(matches!(
                transport.trace.last(),
                Some(LoaderTrace::Cleanup(_))
            ));
            let acquired = transport.trace.iter().any(|event| {
                matches!(
                    event,
                    LoaderTrace::Command(DownloadCommand::PatchSemaphoreGet, _)
                )
            }) && fail_at > 4;
            if acquired {
                assert!(transport.trace.iter().any(|event| {
                    matches!(
                        event,
                        LoaderTrace::Command(DownloadCommand::PatchSemaphoreRelease, _)
                    )
                }));
            }
        }
    }

    #[test]
    fn firmware_loader_preserves_patch_release_and_completion_failures() {
        let (patch_bytes, ram_bytes) = loader_images();
        let mut combined = FakeFirmwareLoader {
            fail_patch_publish: true,
            fail_release: true,
            ..Default::default()
        };
        assert!(matches!(
            load_mt7921_firmware(
                &mut combined,
                Patch::parse(&patch_bytes).unwrap(),
                Firmware::parse(&ram_bytes).unwrap(),
            ),
            Err(FirmwareLoaderError::Failed(
                FirmwareLoaderFailure::PatchRelease {
                    primary: Some(_),
                    release: _,
                }
            ))
        ));
        assert_eq!(
            combined.trace.last(),
            Some(&LoaderTrace::Cleanup(
                FirmwareLoaderState::PatchSemaphoreHeld
            ))
        );

        let mut completion_timeout = FakeFirmwareLoader {
            fail_patch_completion: true,
            ..Default::default()
        };
        assert!(matches!(
            load_mt7921_firmware(
                &mut completion_timeout,
                Patch::parse(&patch_bytes).unwrap(),
                Firmware::parse(&ram_bytes).unwrap(),
            ),
            Err(FirmwareLoaderError::Failed(
                FirmwareLoaderFailure::Transport {
                    operation: FirmwareLoaderOperation::WaitScatterCompletion(
                        FirmwareImagePart::Patch
                    ),
                    ..
                }
            ))
        ));
        assert!(completion_timeout.trace.iter().any(|event| matches!(
            event,
            LoaderTrace::Command(DownloadCommand::PatchSemaphoreRelease, _)
        )));
    }

    #[test]
    fn firmware_loader_rejects_wrong_completions_and_wraps_sequence() {
        let (patch_bytes, ram_bytes) = loader_images();
        let mut wrong = FakeFirmwareLoader {
            completion_override: Some((
                DownloadCommand::NicPowerControl,
                FirmwareCommandCompletion::Ack,
            )),
            ..Default::default()
        };
        assert!(matches!(
            load_mt7921_firmware(
                &mut wrong,
                Patch::parse(&patch_bytes).unwrap(),
                Firmware::parse(&ram_bytes).unwrap(),
            ),
            Err(FirmwareLoaderError::Failed(
                FirmwareLoaderFailure::UnexpectedCommandCompletion {
                    command: DownloadCommand::NicPowerControl,
                    completion: FirmwareCommandCompletion::Ack,
                }
            ))
        ));

        let mut wrong_and_cleanup = FakeFirmwareLoader {
            fail_at: Some(2),
            completion_override: Some((
                DownloadCommand::NicPowerControl,
                FirmwareCommandCompletion::Ack,
            )),
            ..Default::default()
        };
        assert!(matches!(
            load_mt7921_firmware(
                &mut wrong_and_cleanup,
                Patch::parse(&patch_bytes).unwrap(),
                Firmware::parse(&ram_bytes).unwrap(),
            ),
            Err(FirmwareLoaderError::Cleanup {
                failure: Some(FirmwareLoaderFailure::UnexpectedCommandCompletion { .. }),
                source: "injected transport failure",
            })
        ));

        let mut finish_status = FakeFirmwareLoader {
            completion_override: Some((
                DownloadCommand::PatchFinish,
                FirmwareCommandCompletion::PatchFinish(9),
            )),
            ..Default::default()
        };
        assert!(matches!(
            load_mt7921_firmware(
                &mut finish_status,
                Patch::parse(&patch_bytes).unwrap(),
                Firmware::parse(&ram_bytes).unwrap(),
            ),
            Err(FirmwareLoaderError::Failed(
                FirmwareLoaderFailure::UnexpectedPatchFinish(9)
            ))
        ));

        let mut wrapped = FakeFirmwareLoader {
            sequence: 14,
            ..Default::default()
        };
        load_mt7921_firmware(
            &mut wrapped,
            Patch::parse(&patch_bytes).unwrap(),
            Firmware::parse(&ram_bytes).unwrap(),
        )
        .unwrap();
        assert_eq!(
            wrapped.trace[0],
            LoaderTrace::Command(DownloadCommand::NicPowerControl, 15)
        );
        assert!(wrapped.trace.iter().any(|event| matches!(
            event,
            LoaderTrace::Command(DownloadCommand::PatchSemaphoreGet, 1)
        )));
        assert!(!wrapped.trace.iter().any(|event| matches!(
            event,
            LoaderTrace::Command(_, 0)
                | LoaderTrace::PublishScatter(_, 0, _)
                | LoaderTrace::ScatterCompletion(_, 0, _)
        )));
    }

    #[test]
    fn firmware_loader_bounds_readiness_warning_and_skips_an_existing_patch() {
        let (patch_bytes, ram_bytes) = loader_images();
        let mut timeout = FakeFirmwareLoader {
            download_states: vec![0],
            ..Default::default()
        };
        let timed_out_report = load_mt7921_firmware(
            &mut timeout,
            Patch::parse(&patch_bytes).unwrap(),
            Firmware::parse(&ram_bytes).unwrap(),
        )
        .unwrap();
        assert!(!timed_out_report.download_ready_observed);
        assert_eq!(timeout.now_ms, DOWNLOAD_READY_TIMEOUT_MS + 10);
        assert_eq!(
            timeout
                .trace
                .iter()
                .filter(|event| matches!(event, LoaderTrace::DownloadState))
                .count(),
            101
        );
        assert_eq!(
            timeout.trace.last(),
            Some(&LoaderTrace::Cleanup(FirmwareLoaderState::Ready))
        );

        let mut existing = FakeFirmwareLoader {
            semaphore_result: 1,
            ..Default::default()
        };
        let report = load_mt7921_firmware(
            &mut existing,
            Patch::parse(&patch_bytes).unwrap(),
            Firmware::parse(&ram_bytes).unwrap(),
        )
        .unwrap();
        assert_eq!(report.patch, PatchDisposition::AlreadyDownloaded);
        assert_eq!(report.patch_sections, 0);
        assert!(!existing.trace.iter().any(|event| matches!(
            event,
            LoaderTrace::PublishScatter(FirmwareImagePart::Patch, ..)
        )));
        assert_eq!(
            existing
                .trace
                .iter()
                .filter(|event| matches!(
                    event,
                    LoaderTrace::Command(DownloadCommand::TargetAddressLength { .. }, _)
                ))
                .count(),
            2
        );
        assert!(!existing.trace.iter().any(|event| matches!(
            event,
            LoaderTrace::Command(DownloadCommand::PatchSemaphoreRelease, _)
        )));

        let mut n9_timeout = FakeFirmwareLoader {
            n9_states: vec![false],
            ..Default::default()
        };
        assert!(matches!(
            load_mt7921_firmware(
                &mut n9_timeout,
                Patch::parse(&patch_bytes).unwrap(),
                Firmware::parse(&ram_bytes).unwrap(),
            ),
            Err(FirmwareLoaderError::Failed(
                FirmwareLoaderFailure::N9ReadyTimeout
            ))
        ));
        assert_eq!(
            n9_timeout.trace.last(),
            Some(&LoaderTrace::Cleanup(FirmwareLoaderState::FirmwareStarted))
        );
        assert_eq!(
            n9_timeout
                .trace
                .iter()
                .filter(|event| matches!(event, LoaderTrace::N9Ready))
                .count(),
            151
        );
    }

    #[test]
    fn parses_download_and_non_download_clc_regions() {
        let image = firmware_image(&[
            (0x0040_0000, 0, 0, b"ram"),
            (0, FW_FEATURE_NON_DL, FW_TYPE_CLC, b"clc-data"),
        ]);
        let firmware = Firmware::parse(&image).unwrap();
        assert_eq!(firmware.region_count(), 2);
        assert_eq!(firmware.trailer.chip_id, 0x79);
        assert_eq!(firmware.trailer.firmware_version, b"FW-TEST-01");
        assert_eq!(firmware.trailer.crc, 0xdead_beef);

        let mut regions = firmware.regions();
        let ram = regions.next().unwrap();
        assert!(ram.is_downloadable());
        assert_eq!(ram.address, 0x0040_0000);
        assert_eq!(ram.payload, b"ram");
        let clc = regions.next().unwrap();
        assert!(!clc.is_downloadable());
        assert!(clc.is_clc());
        assert_eq!(clc.payload, b"clc-data");
        assert_eq!(regions.next(), None);
    }

    #[test]
    fn rejects_truncated_tables_and_payload_overlapping_metadata() {
        assert_eq!(
            Firmware::parse(&[0; FW_TRAILER_LEN - 1]),
            Err(FirmwareError::MissingTrailer)
        );

        let mut table_too_large = [0u8; FW_TRAILER_LEN];
        table_too_large[2] = 1;
        assert_eq!(
            Firmware::parse(&table_too_large),
            Err(FirmwareError::RegionTableTooLarge)
        );

        let mut image = firmware_image(&[(0, 0, 0, b"x")]);
        let record = image.len() - FW_TRAILER_LEN - FW_REGION_LEN;
        image[record + 20..record + 24].copy_from_slice(&2u32.to_le_bytes());
        assert_eq!(
            Firmware::parse(&image),
            Err(FirmwareError::PayloadOverlapsMetadata)
        );
    }
}

/// The actual affine provenance carrier and its owning session are deliberately
/// binary-private and cannot be constructed by an external crate.
///
/// ```compile_fail
/// let _ = mt7921_port_spike::ProvenanceHandle {
///     session: unsafe { std::num::NonZeroU64::new_unchecked(1) },
///     generation: 1,
///     index: 0,
/// };
/// ```
pub struct PrivateProvenanceIsNotExported;
