//! Linux VFIO/iommufd exercise for the QEMU edu vertical slice.
#![cfg(target_os = "linux")]
#![forbid(unsafe_code)]

use drv_hardware::Device;
use drv_hardware_backends::{
    LinuxVfio, LinuxVfioPciCapabilities, run_edu_sequence, run_locked_vfio_edu_mechanics,
};

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
        .expect("usage: vfio_edu [--probe|--locked-proof] /dev/vfio/devices/vfioN [PCI_CONFIG]");
    if first == "--probe" {
        let path = args
            .next()
            .or_else(|| std::env::var("DRV_VFIO_DEVICE").ok())
            .expect("--probe requires a VFIO cdev path or DRV_VFIO_DEVICE");
        run_physical_probe(&path);
        return;
    }
    if first == "--locked-proof" {
        let cdev = args.next().expect("--locked-proof requires VFIO cdev");
        let pci = args.next().expect("--locked-proof requires PCI config");
        let capabilities = LinuxVfioPciCapabilities::open(cdev, pci)
            .expect("open inert QEMU edu capabilities")
            .lock_down()
            .expect("install MT7921 VFIO lockdown");
        let report = run_locked_vfio_edu_mechanics(capabilities)
            .expect("prove locked VFIO/iommufd mechanics");
        println!(
            "locked_vfio_edu=PASS bind=true attach=true info=true pci_config_rw={} region={} bar_mmap=true bar_round_trip={} dma_round_trip={} irq={} irq_deliveries={} irq_disabled=true reset_admitted=true reset_supported={} reset_succeeded={} ioas_destroyed=true in_process_denial_injection=false fatal_denials=separate-sandbox-subprocess-proof clean_teardown=true",
            report.pci_config_rw,
            report.region_index,
            report.bar_round_trip,
            report.dma_round_trip,
            report.irq_index,
            report.irq_deliveries,
            report.reset_supported,
            report.reset_succeeded
        );
        println!("{}", linux_self_sandbox::MT7921_VFIO_AUTHORITY_INVENTORY);
        return;
    }

    let backend =
        LinuxVfio::open_coherent(&first).expect("initialize coherent VFIO/iommufd backend");
    let device = Device::from_backend(backend);
    let payload = run_edu_sequence(&device).expect("safe edu sequence");
    eprintln!("DMA payload {payload:?}");
    println!("safe VFIO edu MMIO/DMA/IRQ/reset sequence passed");
}
