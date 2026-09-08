//! Linux VFIO/iommufd exercise for the QEMU edu vertical slice.
#![cfg(target_os = "linux")]
#![forbid(unsafe_code)]

use drv_hardware::Device;
use drv_hardware_backends::{LinuxVfio, run_edu_sequence};

fn run_physical_probe(path: &str) {
    let backend = LinuxVfio::open_coherent(path).expect("initialize coherent VFIO/iommufd backend");
    let device = Device::from_backend(backend);
    let mut dma = device
        .alloc_streaming::<drv_hardware::Bidirectional>(4096, 4096)
        .expect("map private DMA arena");
    let marker = b"drv physical VFIO probe";
    dma.write(0, marker).expect("write private DMA arena");
    dma.sync_for_device(0, marker.len())
        .expect("publish private DMA arena");
    drop(dma);
    drop(device);
    println!("safe VFIO/iommufd DMA map/unmap probe passed without device MMIO");
}

fn main() {
    let mut args = std::env::args().skip(1);
    let first = args
        .next()
        .expect("usage: vfio_edu [--probe] /dev/vfio/devices/vfioN");
    if first == "--probe" {
        let path = args
            .next()
            .or_else(|| std::env::var("DRV_VFIO_DEVICE").ok())
            .expect("--probe requires a VFIO cdev path or DRV_VFIO_DEVICE");
        run_physical_probe(&path);
        return;
    }

    let backend =
        LinuxVfio::open_coherent(&first).expect("initialize coherent VFIO/iommufd backend");
    let device = Device::from_backend(backend);
    let payload = run_edu_sequence(&device).expect("safe edu sequence");
    eprintln!("DMA payload {payload:?}");
    println!("safe VFIO edu MMIO/DMA/IRQ/reset sequence passed");
}
