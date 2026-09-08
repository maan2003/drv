// PORT-MAP: local-seam
#![no_std]
#![forbid(unsafe_code)]
//! Re-export of the project portable hardware contract for ath11k backends.
//! Concrete VFIO-platform code implements Backend; drivers consume only
//! generation-tied bounded MMIO, directional DMA, interrupts, reset and drop.
pub use hardware_api::{
    Backend, Bidirectional, CoherentDma, CpuRead, CpuWrite, Device, DeviceAddress, DeviceRead,
    DeviceWrite, Direction, DmaConstraints, DmaDirection, Error, FromDevice, Interrupt, IrqEvent,
    MmioRegion, Result, StreamingDma, ToDevice,
};
