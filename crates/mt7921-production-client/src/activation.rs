//! Private typed activation of the bounded firmware-loader transport.

use crate::{
    AcquisitionLedger, ContainmentLedger, DmaArenas, HardwareResource, OwnedHardwareResources,
};
use drv_hardware::{Backend, Bidirectional, CoherentDma, MmioRegion};
use mt7921_core::{
    ActivationFailure, DMA_DESCRIPTOR_LEN, DisabledMcuRxTransport, DmaDescriptor,
    DmashdlInvariantIo, GlobalTxRingTransport, MT_HIF_REMAP_L1_BAR_OFFSET,
    MT7921_MCU_RX_RING_COUNT, McuRxIrqTopology, McuRxRegisters, OwnershipTransport,
    PCIE_LPCR_HOST_CLR_OWN, TopOwnershipTransport, TransportActivationOps, TxRingState,
    WfsysResetTransport, acquire_driver_ownership, acquire_top_driver_ownership,
    activate_transport, ensure_linux_dmashdl_invariant, prepare_global_rx_rings,
    prepare_global_tx_rings, prepare_mcu_rx_ring, reset_wfsys,
};
use std::time::{Duration, Instant};

const PAGE: usize = 4096;
const HOST_INT_STATUS: usize = 0x200;
const HOST_INT_ENABLE: usize = 0x204;
const WFDMA_GLO_CFG: usize = 0x208;
const WFDMA_RST_DTX_PTR: usize = 0x20c;
const WFDMA_RST_DRX_PTR: usize = 0x100;
const WFDMA_GLO_CFG_EXT0: usize = 0x2b0;
const DMASHDL_SW_CONTROL: usize = 0x004;
const DMASHDL_BYPASS: u32 = 1 << 28;
const WFDMA_TX_DMASHDL_ENABLE: u32 = 1 << 6;
const PCIE_MAC_INT_ENABLE: usize = 0x188;
const PCIE_MAC_PM: usize = 0x194;
const SWDEF_MODE: usize = 0x23c;

pub(super) trait ActivationPci {
    fn disable_intx(&mut self) -> Result<u16, String>;
    fn enable_bus_master(&mut self) -> Result<u16, String>;
    fn verify_bus_master_enabled(&mut self) -> Result<u16, String>;
    fn disable_bus_master(&mut self) -> Result<u16, String>;
    fn verify_bus_master_disabled(&mut self) -> Result<u16, String>;
}

impl ActivationPci for drv_hardware_backends::PciControl {
    fn disable_intx(&mut self) -> Result<u16, String> {
        self.disable_intx().map_err(|error| error.to_string())
    }
    fn enable_bus_master(&mut self) -> Result<u16, String> {
        self.enable_bus_master().map_err(|error| error.to_string())
    }
    fn verify_bus_master_enabled(&mut self) -> Result<u16, String> {
        self.verify_bus_master_enabled()
            .map_err(|error| error.to_string())
    }
    fn disable_bus_master(&mut self) -> Result<u16, String> {
        self.disable_bus_master().map_err(|error| error.to_string())
    }
    fn verify_bus_master_disabled(&mut self) -> Result<u16, String> {
        self.verify_dma_disabled()
            .map(|snapshot| snapshot.command())
            .map_err(|error| error.to_string())
    }
}

fn initialize_page<B: Backend>(
    dma: &mut CoherentDma<B, Bidirectional>,
) -> Result<(), drv_hardware::Error> {
    let reset = DmaDescriptor::reset().to_le_bytes();
    for offset in (0..dma.len()).step_by(DMA_DESCRIPTOR_LEN) {
        dma.write(offset, &reset)?;
    }
    Ok(())
}

fn initialize_descriptors<B: Backend>(dma: &mut DmaArenas<B>) -> Result<(), drv_hardware::Error> {
    for ring in [
        &mut dma.tx_guard,
        &mut dma.fwdl_ring,
        &mut dma.mcu_tx_ring,
        &mut dma.rx_guard,
        &mut dma.mcu_rx_ring,
        &mut dma.wa_rx_ring,
        &mut dma.data_rx_ring,
        &mut dma.management_tx_ring,
    ] {
        initialize_page(ring)?;
    }
    for (ring, buffers) in [
        (&mut dma.mcu_rx_ring, &dma.mcu_rx_buffers),
        (&mut dma.wa_rx_ring, &dma.wa_rx_buffers),
    ] {
        let prepared = prepare_mcu_rx_ring(
            ring.device_address(0)?.bits(),
            buffers.device_address(0)?.bits(),
        )
        .map_err(|_| drv_hardware::Error::Invalid)?;
        for (index, descriptor) in prepared.descriptors.into_iter().enumerate() {
            ring.write(index * DMA_DESCRIPTOR_LEN, &descriptor.to_le_bytes())?;
        }
    }
    Ok(())
}

struct OwnershipIo<B: Backend> {
    conn: MmioRegion<B>,
    start: Instant,
}
impl<B: Backend> OwnershipTransport for OwnershipIo<B> {
    type Error = drv_hardware::Error;
    fn now_ms(&self) -> u64 {
        self.start
            .elapsed()
            .as_millis()
            .try_into()
            .unwrap_or(u64::MAX)
    }
    fn write_clear_own(&mut self) -> Result<(), Self::Error> {
        self.conn.write_u32(0x10, PCIE_LPCR_HOST_CLR_OWN)
    }
    fn read_low_power_control(&mut self) -> Result<u32, Self::Error> {
        self.conn.read_u32(0x10)
    }
    fn sleep_ms(&mut self, ms: u64) {
        std::thread::sleep(Duration::from_millis(ms));
    }
}

struct WfsysIo<B: Backend> {
    selector: MmioRegion<B>,
    window: MmioRegion<B>,
    saved: u32,
    start: Instant,
}
impl<B: Backend> WfsysIo<B> {
    fn select(&mut self) -> Result<(), drv_hardware::Error> {
        self.selector
            .write_u32(0x24c, (self.saved & !0xffff) | 0x1800)?;
        let raw = self.selector.read_u32(0x24c)?;
        (raw & 0xffff == 0x1800)
            .then_some(())
            .ok_or(drv_hardware::Error::DeviceFault)
    }
    fn restore(&mut self) -> Result<(), drv_hardware::Error> {
        self.selector.write_u32(0x24c, self.saved)
    }
}
impl<B: Backend> WfsysResetTransport for WfsysIo<B> {
    type Error = drv_hardware::Error;
    fn now_ms(&self) -> u64 {
        self.start
            .elapsed()
            .as_millis()
            .try_into()
            .unwrap_or(u64::MAX)
    }
    fn read_reset_control(&mut self) -> Result<u32, Self::Error> {
        self.select()?;
        let raw = self.window.read_u32(0x140)?;
        (raw != u32::MAX)
            .then_some(raw)
            .ok_or(drv_hardware::Error::DeviceFault)
    }
    fn write_reset_control(&mut self, value: u32) -> Result<(), Self::Error> {
        self.select()?;
        self.window.write_u32(0x140, value)
    }
    fn sleep_ms(&mut self, ms: u64) {
        std::thread::sleep(Duration::from_millis(ms));
    }
}

struct TopIo<B: Backend> {
    selector: MmioRegion<B>,
    window: MmioRegion<B>,
    start: Instant,
}
impl<B: Backend> TopOwnershipTransport for TopIo<B> {
    type Error = drv_hardware::Error;
    fn now_ms(&self) -> u64 {
        self.start
            .elapsed()
            .as_millis()
            .try_into()
            .unwrap_or(u64::MAX)
    }
    fn read_selector(&mut self) -> Result<u32, Self::Error> {
        self.selector.read_u32(MT_HIF_REMAP_L1_BAR_OFFSET - 0xfe000)
    }
    fn write_selector(&mut self, value: u32) -> Result<(), Self::Error> {
        self.selector
            .write_u32(MT_HIF_REMAP_L1_BAR_OFFSET - 0xfe000, value)
    }
    fn write_top_driver_own(&mut self) -> Result<(), Self::Error> {
        self.window.write_u32(0x10, 1 << 1)
    }
    fn read_top_low_power_control(&mut self) -> Result<u32, Self::Error> {
        self.window.read_u32(0x10)
    }
    fn sleep_ms(&mut self, ms: u64) {
        std::thread::sleep(Duration::from_millis(ms));
    }
}

struct TxIo<B: Backend>(MmioRegion<B>);
impl<B: Backend> GlobalTxRingTransport for TxIo<B> {
    type Error = drv_hardware::Error;
    fn read_global_config(&mut self) -> Result<u32, Self::Error> {
        self.0.read_u32(WFDMA_GLO_CFG)
    }
    fn read_interrupt_enable(&mut self) -> Result<u32, Self::Error> {
        self.0.read_u32(HOST_INT_ENABLE)
    }
    fn read_tx_ring(&mut self, index: usize) -> Result<TxRingState, Self::Error> {
        let base = 0x300 + index * 0x10;
        Ok(TxRingState {
            descriptor_base: self.0.read_u32(base)?,
            descriptor_count: self.0.read_u32(base + 4)?,
            cpu_index: self.0.read_u32(base + 8)?,
            dma_index: self.0.read_u32(base + 12)?,
        })
    }
    fn write_tx_ring(
        &mut self,
        index: usize,
        base: u32,
        count: u32,
        cidx: u32,
    ) -> Result<(), Self::Error> {
        let offset = 0x300 + index * 0x10;
        self.0.write_u32(offset, base)?;
        self.0.write_u32(offset + 4, count)?;
        self.0.write_u32(offset + 8, cidx)
    }
    fn reset_tx_indices(&mut self, value: u32) -> Result<(), Self::Error> {
        self.0.write_u32(WFDMA_RST_DTX_PTR, value)
    }
}

struct RxIo<B: Backend>(MmioRegion<B>);
impl<B: Backend> DisabledMcuRxTransport for RxIo<B> {
    type Error = drv_hardware::Error;
    fn read_global_config(&mut self) -> Result<u32, Self::Error> {
        self.0.read_u32(WFDMA_GLO_CFG)
    }
    fn read_interrupt_enable(&mut self) -> Result<u32, Self::Error> {
        self.0.read_u32(HOST_INT_ENABLE)
    }
    fn read_registers(&mut self) -> Result<McuRxRegisters, Self::Error> {
        self.read_registers_at(0)
    }
    fn read_registers_at(&mut self, index: usize) -> Result<McuRxRegisters, Self::Error> {
        let base = 0x500 + index * 0x10;
        Ok(McuRxRegisters {
            descriptor_base: self.0.read_u32(base)?,
            descriptor_count: self.0.read_u32(base + 4)?,
            cpu_index: self.0.read_u32(base + 8)?,
            dma_index: self.0.read_u32(base + 12)?,
        })
    }
    fn write_initial(&mut self, base: u32, count: u32) -> Result<(), Self::Error> {
        self.write_ring_initial(0, base, count)
    }
    fn publish_cpu_index(&mut self, cidx: u32) -> Result<(), Self::Error> {
        self.publish_ring_cpu_index(0, cidx)
    }
    fn write_ring_initial(
        &mut self,
        index: usize,
        base: u32,
        count: u32,
    ) -> Result<(), Self::Error> {
        let offset = 0x500 + index * 0x10;
        self.0.write_u32(offset, base)?;
        self.0.write_u32(offset + 4, count)?;
        self.0.write_u32(offset + 8, 0)?;
        self.0.write_u32(offset + 12, 0)
    }
    fn publish_ring_cpu_index(&mut self, index: usize, cidx: u32) -> Result<(), Self::Error> {
        self.0.write_u32(0x500 + index * 0x10 + 8, cidx)
    }
    fn release_fence(&mut self) {
        std::sync::atomic::fence(std::sync::atomic::Ordering::Release);
    }
}

struct DmashdlIo<B: Backend> {
    wfdma: MmioRegion<B>,
    dmashdl: MmioRegion<B>,
}
impl<B: Backend> DmashdlInvariantIo for DmashdlIo<B> {
    type Error = drv_hardware::Error;
    fn read_ext0(&mut self) -> Result<u32, Self::Error> {
        self.wfdma.read_u32(WFDMA_GLO_CFG_EXT0)
    }
    fn write_ext0(&mut self, value: u32) -> Result<(), Self::Error> {
        self.wfdma.write_u32(WFDMA_GLO_CFG_EXT0, value)
    }
    fn read_control(&mut self) -> Result<u32, Self::Error> {
        self.dmashdl.read_u32(DMASHDL_SW_CONTROL)
    }
    fn write_control(&mut self, value: u32) -> Result<(), Self::Error> {
        self.dmashdl.write_u32(DMASHDL_SW_CONTROL, value)
    }
}

fn readback(
    region: &MmioRegion<impl Backend>,
    offset: usize,
    expected: u32,
    name: &str,
) -> Result<(), String> {
    let value = region
        .read_u32(offset)
        .map_err(|error| format!("read {name}: {error:?}"))?;
    if value == u32::MAX || value != expected {
        Err(format!(
            "{name} readback {value:#010x}, expected {expected:#010x}"
        ))
    } else {
        Ok(())
    }
}

struct HardwareActivationOps<'a, B: Backend, P> {
    resources: &'a mut OwnedHardwareResources<B>,
    pci: &'a mut P,
    acquisition: &'a mut AcquisitionLedger,
    containment: &'a mut ContainmentLedger,
    enabled_wfdma: Option<u32>,
    interrupt_install_attempted: bool,
}

impl<B: Backend, P: ActivationPci> HardwareActivationOps<'_, B, P> {
    fn region(&self, offset: usize) -> Result<MmioRegion<B>, String> {
        self.resources
            .bar0
            .slice(offset, PAGE)
            .map_err(|error| format!("slice BAR at {offset:#x}: {error:?}"))
    }
}

impl<B: Backend, P: ActivationPci> TransportActivationOps for HardwareActivationOps<'_, B, P> {
    type Error = String;

    fn prepare_descriptors(&mut self) -> Result<(), Self::Error> {
        initialize_descriptors(&mut self.resources.dma)
            .map_err(|error| format!("initialize descriptors: {error:?}"))
    }

    fn mask_and_verify_mac_interrupts(&mut self) -> Result<(), Self::Error> {
        let mac = self.region(0x10000)?;
        mac.write_u32(PCIE_MAC_INT_ENABLE, 0)
            .map_err(|error| format!("mask MAC interrupts: {error:?}"))?;
        readback(&mac, PCIE_MAC_INT_ENABLE, 0, "MAC interrupt enable")?;
        self.pci.disable_intx().map(|_| ())
    }

    fn mask_ack_and_verify_host_interrupts(&mut self) -> Result<(), Self::Error> {
        let wfdma = self.region(0xd4000)?;
        wfdma
            .write_u32(HOST_INT_ENABLE, 0)
            .map_err(|error| format!("mask host interrupts: {error:?}"))?;
        readback(&wfdma, HOST_INT_ENABLE, 0, "host interrupt enable")?;
        wfdma
            .write_u32(HOST_INT_STATUS, u32::MAX)
            .map_err(|error| format!("ack host interrupt status: {error:?}"))?;
        let status = wfdma
            .read_u32(HOST_INT_STATUS)
            .map_err(|error| format!("read host interrupt status: {error:?}"))?;
        if status != 0 {
            Err(format!(
                "host interrupt status did not clear: {status:#010x}"
            ))
        } else {
            Ok(())
        }
    }

    fn acquire_conn_ownership(&mut self) -> Result<(), Self::Error> {
        acquire_driver_ownership(
            &mut OwnershipIo {
                conn: self.region(0xe0000)?,
                start: Instant::now(),
            },
            |_| {},
        )
        .map_err(|error| format!("{error:?}"))
    }

    fn reset_wfsys(&mut self) -> Result<(), Self::Error> {
        let selector = self.region(0xfe000)?;
        let saved = selector
            .read_u32(0x24c)
            .map_err(|error| format!("read selector: {error:?}"))?;
        if saved == u32::MAX {
            return Err("selector returned all ones".into());
        }
        let mut reset = WfsysIo {
            selector,
            window: self.region(0x40000)?,
            saved,
            start: Instant::now(),
        };
        let operation = reset_wfsys(&mut reset, |_| {}).map_err(|error| format!("{error:?}"));
        let restore = reset
            .restore()
            .map_err(|error| format!("selector restore: {error:?}"));
        operation.and(restore)
    }

    fn disable_wfdma_and_wait_idle(&mut self) -> Result<(), Self::Error> {
        let wfdma = self.region(0xd4000)?;
        let initial = wfdma
            .read_u32(WFDMA_GLO_CFG)
            .map_err(|error| format!("read WFDMA global: {error:?}"))?;
        if initial == u32::MAX {
            return Err("WFDMA global returned all ones".into());
        }
        let disabled =
            initial & !((1 << 0) | (1 << 2) | (1 << 15) | (1 << 21) | (1 << 27) | (1 << 28));
        wfdma
            .write_u32(WFDMA_GLO_CFG, disabled)
            .map_err(|error| format!("disable WFDMA: {error:?}"))?;
        let deadline = Instant::now() + Duration::from_millis(100);
        loop {
            let value = wfdma
                .read_u32(WFDMA_GLO_CFG)
                .map_err(|error| format!("read WFDMA idle: {error:?}"))?;
            if value == u32::MAX {
                return Err("WFDMA idle read returned all ones".into());
            }
            if value & ((1 << 1) | (1 << 3)) == 0 {
                break;
            }
            if Instant::now() >= deadline {
                return Err(format!("WFDMA did not quiesce: {value:#010x}"));
            }
            std::thread::sleep(Duration::from_millis(1));
        }
        let reset_indices = wfdma
            .read_u32(WFDMA_RST_DRX_PTR)
            .map_err(|e| format!("read RX reset: {e:?}"))?;
        if reset_indices == u32::MAX {
            return Err("WFDMA reset control returned all ones".into());
        }
        wfdma
            .write_u32(WFDMA_RST_DRX_PTR, reset_indices & !0x30)
            .map_err(|e| format!("clear RX reset: {e:?}"))?;
        wfdma
            .write_u32(WFDMA_RST_DRX_PTR, reset_indices | 0x30)
            .map_err(|e| format!("set RX reset: {e:?}"))?;
        Ok(())
    }

    fn configure_dmashdl(&mut self) -> Result<(), Self::Error> {
        let invariant = ensure_linux_dmashdl_invariant(&mut DmashdlIo {
            wfdma: self.region(0xd4000)?,
            dmashdl: self.region(0xd6000)?,
        })
        .map_err(|error| format!("{error:?}"))?;
        if invariant.ext0_after & WFDMA_TX_DMASHDL_ENABLE != 0
            || invariant.control_after & DMASHDL_BYPASS == 0
        {
            Err("DMASHDL invariant mismatch".into())
        } else {
            Ok(())
        }
    }

    fn route_rings(&mut self) -> Result<(), Self::Error> {
        let tx_guard = self
            .resources
            .dma
            .tx_guard
            .device_address(0)
            .map_err(|e| format!("TX guard: {e:?}"))?
            .bits();
        let fwdl = self
            .resources
            .dma
            .fwdl_ring
            .device_address(0)
            .map_err(|e| format!("FWDL ring: {e:?}"))?
            .bits();
        let mcu_tx = self
            .resources
            .dma
            .mcu_tx_ring
            .device_address(0)
            .map_err(|e| format!("MCU TX ring: {e:?}"))?
            .bits();
        let rx_guard = self
            .resources
            .dma
            .rx_guard
            .device_address(0)
            .map_err(|e| format!("RX guard: {e:?}"))?
            .bits();
        let wm = self
            .resources
            .dma
            .mcu_rx_ring
            .device_address(0)
            .map_err(|e| format!("WM ring: {e:?}"))?
            .bits();
        let wm2_base = self
            .resources
            .dma
            .wa_rx_ring
            .device_address(0)
            .map_err(|e| format!("WM2 ring: {e:?}"))?
            .bits() as u32;
        prepare_global_tx_rings(
            &mut TxIo(self.region(0xd4000)?),
            tx_guard,
            fwdl,
            mcu_tx,
            |_| {},
        )
        .map_err(|e| format!("{e:?}"))?;
        prepare_global_rx_rings(&mut RxIo(self.region(0xd4000)?), rx_guard, wm, |_| {})
            .map_err(|e| format!("{e:?}"))?;
        let wfdma = self.region(0xd4000)?;
        let wm2 = 0x500 + 4 * 0x10;
        for (word, value) in [
            wm2_base,
            MT7921_MCU_RX_RING_COUNT as u32,
            (MT7921_MCU_RX_RING_COUNT - 1) as u32,
            0,
        ]
        .into_iter()
        .enumerate()
        {
            wfdma
                .write_u32(wm2 + word * 4, value)
                .map_err(|e| format!("route WM2: {e:?}"))?;
        }
        Ok(())
    }

    fn install_interrupt(&mut self) -> Result<(), Self::Error> {
        if self.resources.interrupt.is_some() {
            return Err("interrupt already installed".into());
        }
        self.interrupt_install_attempted = true;
        let interrupt = self
            .resources
            .device
            .open_interrupt(0)
            .map_err(|e| format!("install MSI vector 0: {e:?}"))?;
        self.resources.interrupt = Some(interrupt);
        self.acquisition.record(HardwareResource::Interrupt);
        self.containment.irq_installed = true;
        Ok(())
    }

    fn verify_interrupt_quiet(&mut self) -> Result<(), Self::Error> {
        match self
            .resources
            .interrupt
            .as_ref()
            .ok_or("interrupt not installed")?
            .wait_until(0)
            .map_err(|e| format!("quiet interrupt check: {e:?}"))?
        {
            None => Ok(()),
            Some(_) => Err("unexpected interrupt before source enable".into()),
        }
    }

    fn configure_prefetch(&mut self) -> Result<(), Self::Error> {
        let wfdma = self.region(0xd4000)?;
        for (offset, value) in [
            (0x2f0, 0),
            (0x680, 4),
            (0x688, 0x0040_0004),
            (0x690, 0x00c0_0004),
            (0x640, 0x0340_0004),
            (0x644, 0x0380_0004),
        ] {
            wfdma
                .write_u32(offset, value)
                .map_err(|e| format!("configure prefetch: {e:?}"))?;
        }
        Ok(())
    }

    fn enable_bus_master(&mut self) -> Result<(), Self::Error> {
        self.containment.bus_master_enabled = true;
        self.pci.enable_bus_master().map(|_| ())
    }
    fn verify_bus_master_enabled(&mut self) -> Result<(), Self::Error> {
        self.pci
            .verify_bus_master_enabled()
            .and_then(|command| {
                if command & 4 != 0 {
                    Ok(command)
                } else {
                    Err("BME readback remained clear".into())
                }
            })
            .map(|_| ())
    }
    fn enable_mac_interrupt(&mut self) -> Result<(), Self::Error> {
        let mac = self.region(0x10000)?;
        mac.write_u32(PCIE_MAC_INT_ENABLE, 0xff)
            .map_err(|e| format!("enable MAC IRQ: {e:?}"))?;
        readback(&mac, PCIE_MAC_INT_ENABLE, 0xff, "MAC interrupt enable")
    }
    fn enable_wfdma(&mut self) -> Result<(), Self::Error> {
        let wfdma = self.region(0xd4000)?;
        let current = wfdma
            .read_u32(WFDMA_GLO_CFG)
            .map_err(|e| format!("read WFDMA global: {e:?}"))?;
        if current == u32::MAX {
            return Err("WFDMA global returned all ones".into());
        }
        let global = current
            | (1 << 0)
            | (1 << 2)
            | (3 << 4)
            | (1 << 6)
            | (1 << 11)
            | (1 << 12)
            | (1 << 13)
            | (1 << 15)
            | (1 << 21)
            | (1 << 28)
            | (1 << 30);
        wfdma
            .write_u32(WFDMA_GLO_CFG, global)
            .map_err(|e| format!("enable WFDMA: {e:?}"))?;
        readback(&wfdma, WFDMA_GLO_CFG, global, "WFDMA global")
            .inspect(|()| self.enabled_wfdma = Some(global))
    }
    fn enable_host_interrupt(&mut self) -> Result<(), Self::Error> {
        let wfdma = self.region(0xd4000)?;
        let mask = McuRxIrqTopology::firmware().mask();
        wfdma
            .write_u32(HOST_INT_ENABLE, mask)
            .map_err(|e| format!("enable host IRQ: {e:?}"))?;
        readback(&wfdma, HOST_INT_ENABLE, mask, "host interrupt enable")
    }
    fn acquire_top_ownership(&mut self) -> Result<(), Self::Error> {
        acquire_top_driver_ownership(
            &mut TopIo {
                selector: self.region(0xfe000)?,
                window: self.region(0x40000)?,
                start: Instant::now(),
            },
            |_| {},
        )
        .map_err(|e| format!("{e:?}"))
    }
    fn disable_l0s(&mut self) -> Result<(), Self::Error> {
        let mac = self.region(0x10000)?;
        let pm = mac
            .read_u32(PCIE_MAC_PM)
            .map_err(|e| format!("read PCIe PM: {e:?}"))?;
        if pm == u32::MAX {
            return Err("PCIe PM returned all ones".into());
        }
        mac.write_u32(PCIE_MAC_PM, pm | (1 << 8))
            .map_err(|e| format!("disable L0s: {e:?}"))?;
        let after = mac
            .read_u32(PCIE_MAC_PM)
            .map_err(|e| format!("verify L0s: {e:?}"))?;
        if after != u32::MAX && after & (1 << 8) != 0 {
            Ok(())
        } else {
            Err("PCIe L0s disable did not latch".into())
        }
    }
    fn set_swdef_normal(&mut self) -> Result<(), Self::Error> {
        let swdef = self.region(0x9f000)?;
        if swdef
            .read_u32(SWDEF_MODE)
            .map_err(|e| format!("read SWDEF: {e:?}"))?
            == u32::MAX
        {
            return Err("SWDEF returned all ones".into());
        }
        swdef
            .write_u32(SWDEF_MODE, 0)
            .map_err(|e| format!("write SWDEF: {e:?}"))?;
        readback(&swdef, SWDEF_MODE, 0, "SWDEF mode")
    }
    fn final_readback(&mut self) -> Result<(), Self::Error> {
        let mac = self.region(0x10000)?;
        let wfdma = self.region(0xd4000)?;
        readback(
            &mac,
            PCIE_MAC_INT_ENABLE,
            0xff,
            "final MAC interrupt enable",
        )?;
        let global = wfdma
            .read_u32(WFDMA_GLO_CFG)
            .map_err(|e| format!("final WFDMA read: {e:?}"))?;
        let expected = self.enabled_wfdma.ok_or("enabled WFDMA value absent")?;
        if global != expected {
            return Err(format!(
                "final WFDMA global {global:#010x}, expected {expected:#010x}"
            ));
        }
        readback(
            &wfdma,
            HOST_INT_ENABLE,
            McuRxIrqTopology::firmware().mask(),
            "final host interrupt enable",
        )
    }
    fn disable_interrupt(&mut self) -> Result<(), Self::Error> {
        if let Some(interrupt) = self.resources.interrupt.as_mut() {
            interrupt
                .disable()
                .map_err(|error| format!("disable installed MSI vector 0: {error:?}"))?;
            drop(self.resources.interrupt.take());
        } else if self.interrupt_install_attempted {
            self.resources
                .device
                .disable_interrupt_vector(0)
                .map_err(|error| format!("revoke possibly installed MSI vector 0: {error:?}"))?;
        }
        self.containment.irq_installed = false;
        self.interrupt_install_attempted = false;
        Ok(())
    }
    fn disable_bus_master(&mut self) -> Result<(), Self::Error> {
        self.pci.disable_bus_master().map(|_| ())
    }
    fn verify_bus_master_disabled(&mut self) -> Result<(), Self::Error> {
        let command = self.pci.verify_bus_master_disabled()?;
        self.containment.bme_disabled_command = Some(command);
        self.containment.bus_master_enabled = false;
        Ok(())
    }
}

pub(super) fn activate<B: Backend>(
    resources: &mut OwnedHardwareResources<B>,
    pci: &mut impl ActivationPci,
    acquisition: &mut AcquisitionLedger,
    containment: &mut ContainmentLedger,
) -> Result<mt7921_core::ActivationState, ActivationFailure<String>> {
    activate_transport(&mut HardwareActivationOps {
        resources,
        pci,
        acquisition,
        containment,
        enabled_wfdma: None,
        interrupt_install_attempted: false,
    })
}

pub(super) fn quiesce<B: Backend>(
    resources: &mut OwnedHardwareResources<B>,
    pci: &mut impl ActivationPci,
    acquisition: &mut AcquisitionLedger,
    containment: &mut ContainmentLedger,
    state: &mut mt7921_core::ActivationState,
) -> Vec<mt7921_core::QuiesceError<String>> {
    mt7921_core::transport_quiesce(
        &mut HardwareActivationOps {
            resources,
            pci,
            acquisition,
            containment,
            enabled_wfdma: None,
            interrupt_install_attempted: state.interrupt
                != mt7921_core::InterruptInstallState::NotAttempted,
        },
        state,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use drv_hardware_backends::DeterministicBackend;

    struct FakePci {
        command: u16,
        calls: Vec<&'static str>,
    }

    impl ActivationPci for FakePci {
        fn disable_intx(&mut self) -> Result<u16, String> {
            self.calls.push("disable-intx");
            self.command |= 1 << 10;
            Ok(self.command)
        }
        fn enable_bus_master(&mut self) -> Result<u16, String> {
            self.calls.push("enable-bme");
            self.command |= 1 << 2;
            Ok(self.command)
        }
        fn verify_bus_master_enabled(&mut self) -> Result<u16, String> {
            self.calls.push("verify-bme-on");
            (self.command & 4 != 0)
                .then_some(self.command)
                .ok_or("BME off".into())
        }
        fn disable_bus_master(&mut self) -> Result<u16, String> {
            self.calls.push("disable-bme");
            self.command &= !(1 << 2);
            Ok(self.command)
        }
        fn verify_bus_master_disabled(&mut self) -> Result<u16, String> {
            self.calls.push("verify-bme-off");
            (self.command & 4 == 0)
                .then_some(self.command)
                .ok_or("BME on".into())
        }
    }

    fn containment() -> ContainmentLedger {
        ContainmentLedger {
            vfio_attached: true,
            bar_mapped: true,
            dma_mapped: true,
            irq_installed: false,
            bus_master_enabled: false,
            bme_disabled_command: None,
            reset_generation: None,
            post_reset_registers: None,
            post_reset_pci: None,
        }
    }

    #[test]
    fn physical_adapter_installs_irq_only_after_mask_reset_and_rings_then_quiesces() {
        let (device, operations) = DeterministicBackend::recording_mt7921_activation_device();
        let (mut resources, mut acquisition) = OwnedHardwareResources::acquire(device).unwrap();
        assert!(resources.interrupt.is_none());
        assert!(
            !acquisition
                .acquired()
                .contains(&HardwareResource::Interrupt)
        );
        let mut pci = FakePci {
            command: 0x2,
            calls: Vec::new(),
        };
        let mut containment = containment();
        let mut state =
            activate(&mut resources, &mut pci, &mut acquisition, &mut containment).unwrap();
        assert_eq!(
            state.interrupt,
            mt7921_core::InterruptInstallState::InstalledAndQuiet
        );
        assert_eq!(
            acquisition.acquired().last(),
            Some(&HardwareResource::Interrupt)
        );
        assert!(containment.irq_installed);
        assert!(containment.bus_master_enabled);
        let operations = operations.borrow();
        let host_mask = operations
            .iter()
            .position(|operation| {
                matches!(
                    operation,
                    drv_hardware_backends::Operation::WriteU32 {
                        offset: 0xd4204,
                        value: 0,
                        ..
                    }
                )
            })
            .unwrap();
        let route = operations.iter().rposition(|operation| matches!(operation,
            drv_hardware_backends::Operation::WriteU32 { offset, .. } if (0xd4300..=0xd454c).contains(offset)
        )).unwrap();
        assert!(host_mask < route);
        drop(operations);

        assert!(
            quiesce(
                &mut resources,
                &mut pci,
                &mut acquisition,
                &mut containment,
                &mut state,
            )
            .is_empty()
        );
        assert!(resources.interrupt.is_none());
        assert!(!containment.irq_installed);
        assert!(!containment.bus_master_enabled);
        assert_eq!(pci.calls.last(), Some(&"verify-bme-off"));
    }

    #[test]
    fn irq_disable_failure_retains_handle_ledger_and_ambiguity_for_retry() {
        let (device, _, failures) =
            DeterministicBackend::recording_mt7921_activation_device_with_failures();
        let (mut resources, mut acquisition) = OwnedHardwareResources::acquire(device).unwrap();
        let mut pci = FakePci {
            command: 0x2,
            calls: Vec::new(),
        };
        let mut containment = containment();
        let mut state =
            activate(&mut resources, &mut pci, &mut acquisition, &mut containment).unwrap();
        failures.fail_next_interrupt_disable();
        let errors = quiesce(
            &mut resources,
            &mut pci,
            &mut acquisition,
            &mut containment,
            &mut state,
        );
        assert!(
            errors
                .iter()
                .any(|error| error.step == mt7921_core::QuiesceStep::DisableInterrupt)
        );
        assert!(resources.interrupt.is_some());
        assert!(containment.irq_installed);
        assert_eq!(
            state.interrupt,
            mt7921_core::InterruptInstallState::InstalledAndQuiet
        );
        assert!(
            quiesce(
                &mut resources,
                &mut pci,
                &mut acquisition,
                &mut containment,
                &mut state,
            )
            .is_empty()
        );
        assert!(resources.interrupt.is_none());
    }

    #[test]
    fn ambiguous_irq_install_uses_vector_revoke_and_retains_revoke_failure() {
        let (device, _, failures) =
            DeterministicBackend::recording_mt7921_activation_device_with_failures();
        let (mut resources, mut acquisition) = OwnedHardwareResources::acquire(device).unwrap();
        let mut pci = FakePci {
            command: 0x2,
            calls: Vec::new(),
        };
        let mut containment = containment();
        failures.fail_next_interrupt_open();
        failures.fail_interrupt_disable_attempts(2);
        let failure =
            activate(&mut resources, &mut pci, &mut acquisition, &mut containment).unwrap_err();
        assert_eq!(
            failure.primary.stage,
            mt7921_core::ActivationStage::InstallInterrupt
        );
        assert!(
            failure
                .cleanup
                .iter()
                .any(|error| error.step == mt7921_core::QuiesceStep::DisableInterrupt)
        );
        assert_eq!(
            failure.state.interrupt,
            mt7921_core::InterruptInstallState::PossiblyInstalled
        );
        assert!(
            !acquisition
                .acquired()
                .contains(&HardwareResource::Interrupt)
        );
        assert!(!containment.irq_installed);
        assert_eq!(
            resources.device.reset(),
            Err(drv_hardware::Error::DeviceFault)
        );
        assert!(resources.device.reset().is_ok());
    }
}
