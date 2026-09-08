//! Non-coherent packet-buffer DMA lifecycle owned by the data path.

use alloc::vec;
use alloc::vec::Vec;
use ath11k_platform_backend::{Backend, Device, FromDevice, StreamingDma, ToDevice};

use crate::DpError;

/// A TX mapping made at the `dma_map_single(..., DMA_TO_DEVICE)` boundary.
pub struct TxBuffer<B: Backend> {
    dma: StreamingDma<B, ToDevice>,
    length: usize,
}

impl<B: Backend> TxBuffer<B> {
    /// Copy the stack frame into streaming memory and make it device-visible
    /// before the caller publishes its TCL descriptor.
    pub fn map(device: &Device<B>, frame: &[u8]) -> Result<Self, DpError> {
        let mut dma = device
            .alloc_streaming::<ToDevice>(frame.len(), 4)
            .map_err(|_| DpError::NoResources)?;
        dma.write(0, frame).map_err(|_| DpError::DeviceFault)?;
        dma.sync_for_device(0, frame.len())
            .map_err(|_| DpError::DeviceFault)?;
        Ok(Self {
            dma,
            length: frame.len(),
        })
    }

    pub fn length(&self) -> usize {
        self.length
    }

    pub fn device_address(
        &self,
    ) -> Result<ath11k_platform_backend::DeviceAddress<'_, B, ToDevice>, DpError> {
        self.dma.device_address(0).map_err(|_| DpError::DeviceFault)
    }
}

/// An RX mapping made at the `dma_map_single(..., DMA_FROM_DEVICE)` refill
/// boundary. It remains owned until REO/WBM returns its cookie.
pub struct RxBuffer<B: Backend> {
    dma: StreamingDma<B, FromDevice>,
}

impl<B: Backend> RxBuffer<B> {
    pub fn replenish(device: &Device<B>, size: usize) -> Result<Self, DpError> {
        let dma = device
            .alloc_streaming::<FromDevice>(size, 4)
            .map_err(|_| DpError::NoResources)?;
        Ok(Self { dma })
    }

    pub fn len(&self) -> usize {
        self.dma.len()
    }

    pub fn is_empty(&self) -> bool {
        self.dma.is_empty()
    }

    pub fn device_address(
        &self,
    ) -> Result<ath11k_platform_backend::DeviceAddress<'_, B, FromDevice>, DpError> {
        self.dma.device_address(0).map_err(|_| DpError::DeviceFault)
    }

    /// The `dma_sync_single_for_cpu(..., DMA_FROM_DEVICE)` boundary after a
    /// REO/WBM completion and before parsing the RX descriptor or payload.
    pub fn sync_and_read(&mut self, length: usize) -> Result<Vec<u8>, DpError> {
        self.dma
            .sync_for_cpu(0, length)
            .map_err(|_| DpError::DeviceFault)?;
        let mut bytes = vec![0; length];
        self.dma
            .read(0, &mut bytes)
            .map_err(|_| DpError::DeviceFault)?;
        Ok(bytes)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use drv_hardware_backends::{DeterministicBackend, Operation};

    #[test]
    fn tx_and_rx_use_directional_streaming_syncs() {
        let (device, operations) = DeterministicBackend::recording_device();
        let tx = TxBuffer::map(&device, &[1, 2, 3, 4]).unwrap();
        assert_eq!(tx.length(), 4);
        tx.device_address().unwrap();

        let mut rx = RxBuffer::replenish(&device, 64).unwrap();
        assert_eq!(rx.len(), 64);
        rx.device_address().unwrap();
        // The model rejects the wrong directional sync internally. Reaching
        // this read proves sync_for_cpu happened before the CPU-side read.
        assert_eq!(rx.sync_and_read(4).unwrap(), [0, 0, 0, 0]);
        assert_eq!(
            *operations.borrow(),
            [
                Operation::SyncForDevice {
                    dma: 1,
                    range: 0..4,
                },
                Operation::SyncForCpu {
                    dma: 2,
                    range: 0..4,
                },
            ]
        );
    }

    #[test]
    fn rx_completion_length_is_bounded_without_panicking() {
        let device = DeterministicBackend::device();
        let mut rx = RxBuffer::replenish(&device, 32).unwrap();
        assert_eq!(rx.sync_and_read(33), Err(DpError::DeviceFault));
    }
}
