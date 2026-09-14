//! Deterministic host for exercising the driver component boundary.

use std::collections::HashMap;
use std::path::Path;

use wasmtime::component::{Component, HasSelf, Linker, Resource};
use wasmtime::{Config, Engine, Store};

wasmtime::component::bindgen!({
    path: "../../wit",
    world: "driver",
});

use drv::hardware::broker::{
    Device, DmaAccess, DmaArena, Error, Host, HostDevice, HostDmaArena, HostInterrupt, HostRegion,
    Interrupt, IrqEvent, Region,
};
use exports::drv::hardware::probe::Report;

const DEVICE_REP: u32 = 1;
const MAX_ARENAS: usize = 4;
const MAX_ARENA_BYTES: u32 = 4096;
const FIRST_IOVA: u64 = 0x1000_0000;
const INTERRUPT_AT_NS: u64 = 10;

type BrokerResult<T> = std::result::Result<T, Error>;

struct Arena {
    generation: u64,
    iova: u64,
    bytes: Vec<u8>,
    _access: DmaAccess,
}

struct InterruptState {
    generation: u64,
    vector: u32,
    delivered: bool,
}

/// A deterministic resource broker with no ambient host capabilities.
pub struct DeterministicHost {
    generation: u64,
    now_ns: u64,
    next_iova: u64,
    next_region_rep: u32,
    next_arena_rep: u32,
    next_interrupt_rep: u32,
    regions: HashMap<u32, u64>,
    arenas: HashMap<u32, Arena>,
    interrupts: HashMap<u32, InterruptState>,
    dma_iova_register: u64,
    dma_length_register: u32,
    interrupt_pending: bool,
}

impl Default for DeterministicHost {
    fn default() -> Self {
        Self {
            generation: 1,
            now_ns: 0,
            next_iova: FIRST_IOVA,
            next_region_rep: 1,
            next_arena_rep: 1,
            next_interrupt_rep: 1,
            regions: HashMap::new(),
            arenas: HashMap::new(),
            interrupts: HashMap::new(),
            dma_iova_register: 0,
            dma_length_register: 0,
            interrupt_pending: false,
        }
    }
}

impl DeterministicHost {
    fn device_is_current(&self, device: &Resource<Device>) -> bool {
        device.rep() == DEVICE_REP
    }

    fn arena(&self, arena: &Resource<DmaArena>) -> BrokerResult<&Arena> {
        self.arenas
            .get(&arena.rep())
            .filter(|arena| arena.generation == self.generation)
            .ok_or(Error::StaleHandle)
    }

    fn arena_mut(&mut self, arena: &Resource<DmaArena>) -> BrokerResult<&mut Arena> {
        let generation = self.generation;
        self.arenas
            .get_mut(&arena.rep())
            .filter(|arena| arena.generation == generation)
            .ok_or(Error::StaleHandle)
    }

    fn validate_region(&self, region: &Resource<Region>) -> BrokerResult<()> {
        self.regions
            .get(&region.rep())
            .filter(|generation| **generation == self.generation)
            .map(|_| ())
            .ok_or(Error::StaleHandle)
    }

    fn execute_doorbell(&mut self) -> BrokerResult<()> {
        if self.dma_length_register == 0 {
            return Err(Error::Invalid);
        }
        let dma_end = self
            .dma_iova_register
            .checked_add(self.dma_length_register as u64)
            .ok_or(Error::DeviceFault)?;
        let generation = self.generation;
        let dma_iova = self.dma_iova_register;
        let arena = self
            .arenas
            .values_mut()
            .find(|arena| {
                arena.generation == generation
                    && dma_iova >= arena.iova
                    && dma_end <= arena.iova + arena.bytes.len() as u64
            })
            .ok_or(Error::DeviceFault)?;
        let offset = (dma_iova - arena.iova) as u32;
        let range = Self::range(offset, self.dma_length_register, arena.bytes.len())?;
        arena.bytes[range].reverse();
        self.interrupt_pending = true;
        Ok(())
    }

    fn range(offset: u32, length: u32, total: usize) -> BrokerResult<std::ops::Range<usize>> {
        let start = offset as usize;
        let end = start
            .checked_add(length as usize)
            .filter(|end| *end <= total)
            .ok_or(Error::OutOfBounds)?;
        Ok(start..end)
    }
}

impl Host for DeterministicHost {
    fn get_device(&mut self) -> Resource<Device> {
        Resource::new_own(DEVICE_REP)
    }

    fn now(&mut self) -> u64 {
        self.now_ns
    }
}

impl HostDevice for DeterministicHost {
    fn generation(&mut self, device: Resource<Device>) -> u64 {
        assert!(self.device_is_current(&device));
        self.generation
    }

    fn open_region(
        &mut self,
        device: Resource<Device>,
        index: u8,
    ) -> BrokerResult<Resource<Region>> {
        if !self.device_is_current(&device) {
            return Err(Error::StaleHandle);
        }
        if index != 0 {
            return Err(Error::Invalid);
        }
        let rep = self.next_region_rep;
        self.next_region_rep = rep.checked_add(1).ok_or(Error::Limit)?;
        self.regions.insert(rep, self.generation);
        Ok(Resource::new_own(rep))
    }

    fn alloc_dma(
        &mut self,
        device: Resource<Device>,
        size: u32,
        alignment: u32,
        access: DmaAccess,
    ) -> BrokerResult<Resource<DmaArena>> {
        if !self.device_is_current(&device) {
            return Err(Error::StaleHandle);
        }
        if size == 0 || size > MAX_ARENA_BYTES || self.arenas.len() >= MAX_ARENAS {
            return Err(Error::Limit);
        }
        if alignment == 0 || !alignment.is_power_of_two() || alignment > 4096 {
            return Err(Error::Invalid);
        }

        let alignment = alignment as u64;
        let iova = self
            .next_iova
            .checked_add(alignment - 1)
            .map(|value| value & !(alignment - 1))
            .ok_or(Error::Limit)?;
        self.next_iova = iova.checked_add(size as u64).ok_or(Error::Limit)?;

        let rep = self.next_arena_rep;
        self.next_arena_rep = rep.checked_add(1).ok_or(Error::Limit)?;
        self.arenas.insert(
            rep,
            Arena {
                generation: self.generation,
                iova,
                bytes: vec![0; size as usize],
                _access: access,
            },
        );
        Ok(Resource::new_own(rep))
    }

    fn open_interrupt(
        &mut self,
        device: Resource<Device>,
        vector: u32,
    ) -> BrokerResult<Resource<Interrupt>> {
        if !self.device_is_current(&device) {
            return Err(Error::StaleHandle);
        }
        if vector != 0 {
            return Err(Error::Invalid);
        }
        let rep = self.next_interrupt_rep;
        self.next_interrupt_rep = rep.checked_add(1).ok_or(Error::Limit)?;
        self.interrupts.insert(
            rep,
            InterruptState {
                generation: self.generation,
                vector,
                delivered: false,
            },
        );
        Ok(Resource::new_own(rep))
    }

    fn reset(&mut self, device: Resource<Device>) -> BrokerResult<u64> {
        if !self.device_is_current(&device) {
            return Err(Error::StaleHandle);
        }
        self.generation = self.generation.checked_add(1).ok_or(Error::DeviceFault)?;
        self.regions.clear();
        self.arenas.clear();
        self.interrupts.clear();
        self.dma_iova_register = 0;
        self.dma_length_register = 0;
        self.interrupt_pending = false;
        Ok(self.generation)
    }

    fn drop(&mut self, _device: Resource<Device>) -> wasmtime::Result<()> {
        Ok(())
    }
}

impl HostRegion for DeterministicHost {
    fn read_u32(&mut self, region: Resource<Region>, offset: u32) -> BrokerResult<u32> {
        self.validate_region(&region)?;
        match offset {
            0 => Ok(self.dma_iova_register as u32),
            4 => Ok(self.dma_length_register),
            8 => Ok(0),
            12 => Ok(u32::from(self.interrupt_pending)),
            _ => Err(Error::OutOfBounds),
        }
    }

    fn write_u32(&mut self, region: Resource<Region>, offset: u32, value: u32) -> BrokerResult<()> {
        self.validate_region(&region)?;
        match offset {
            0 => self.dma_iova_register = value as u64,
            4 => self.dma_length_register = value,
            8 if value == 1 => return self.execute_doorbell(),
            8 => return Err(Error::Invalid),
            _ => return Err(Error::OutOfBounds),
        }
        Ok(())
    }

    fn drop(&mut self, region: Resource<Region>) -> wasmtime::Result<()> {
        self.regions.remove(&region.rep());
        Ok(())
    }
}

impl HostDmaArena for DeterministicHost {
    fn length(&mut self, arena: Resource<DmaArena>) -> BrokerResult<u32> {
        Ok(self.arena(&arena)?.bytes.len() as u32)
    }

    fn iova(&mut self, arena: Resource<DmaArena>) -> BrokerResult<u64> {
        Ok(self.arena(&arena)?.iova)
    }

    fn read(
        &mut self,
        arena: Resource<DmaArena>,
        offset: u32,
        length: u32,
    ) -> BrokerResult<Vec<u8>> {
        let arena = self.arena(&arena)?;
        let range = Self::range(offset, length, arena.bytes.len())?;
        Ok(arena.bytes[range].to_vec())
    }

    fn write(
        &mut self,
        arena: Resource<DmaArena>,
        offset: u32,
        bytes: Vec<u8>,
    ) -> BrokerResult<()> {
        let arena = self.arena_mut(&arena)?;
        let range = Self::range(
            offset,
            bytes.len().try_into().map_err(|_| Error::Limit)?,
            arena.bytes.len(),
        )?;
        arena.bytes[range].copy_from_slice(&bytes);
        Ok(())
    }

    fn drop(&mut self, arena: Resource<DmaArena>) -> wasmtime::Result<()> {
        self.arenas.remove(&arena.rep());
        Ok(())
    }
}

impl HostInterrupt for DeterministicHost {
    fn wait_until(
        &mut self,
        interrupt: Resource<Interrupt>,
        deadline_ns: u64,
    ) -> BrokerResult<Option<IrqEvent>> {
        let generation = self.generation;
        let state = self
            .interrupts
            .get_mut(&interrupt.rep())
            .filter(|state| state.generation == generation)
            .ok_or(Error::StaleHandle)?;

        if self.interrupt_pending && !state.delivered && deadline_ns >= INTERRUPT_AT_NS {
            self.now_ns = INTERRUPT_AT_NS;
            state.delivered = true;
            self.interrupt_pending = false;
            return Ok(Some(IrqEvent {
                vector: state.vector,
                count: 1,
                at_ns: INTERRUPT_AT_NS,
            }));
        }
        self.now_ns = self.now_ns.max(deadline_ns);
        Ok(None)
    }

    fn drop(&mut self, interrupt: Resource<Interrupt>) -> wasmtime::Result<()> {
        self.interrupts.remove(&interrupt.rep());
        Ok(())
    }
}

/// Runs a no-WASI driver component with bounded Wasm execution.
pub fn run_component(path: impl AsRef<Path>) -> wasmtime::Result<Report> {
    let mut config = Config::new();
    config.wasm_component_model(true).consume_fuel(true);
    let engine = Engine::new(&config)?;
    let component = Component::from_file(&engine, path)
        .map_err(|error| error.context("load probe component"))?;
    let mut linker = Linker::<DeterministicHost>::new(&engine);
    Driver::add_to_linker::<_, HasSelf<DeterministicHost>>(
        &mut linker,
        |state: &mut DeterministicHost| state,
    )?;

    let mut store = Store::new(&engine, DeterministicHost::default());
    store.set_fuel(1_000_000)?;
    let driver = Driver::instantiate(&mut store, &component, &linker)?;
    driver
        .drv_hardware_probe()
        .call_run(&mut store)?
        .map_err(|error| wasmtime::Error::msg(format!("probe returned {error:?}")))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn component_observes_bounds_interrupt_and_revocation() {
        // End-to-end evidence for REQ-hardware-independent-testing and REQ-isolation.
        let path = std::env::var("DRV_PROBE_COMPONENT").expect("scripts/test sets component path");
        let report = run_component(path).unwrap();

        assert_eq!(report.generation_before, 1);
        assert_eq!(report.generation_after, 2);
        assert_eq!(report.iova, FIRST_IOVA);
        assert_eq!(report.interrupt_count, 1);
        assert_eq!(report.interrupt_at_ns, INTERRUPT_AT_NS);
        assert!(report.dma_round_trip);
        assert!(report.device_dma_observed);
        assert!(report.bounds_rejected);
        assert!(report.bar_bounds_rejected);
        assert!(report.stale_rejected);
        assert!(report.region_stale_rejected);
    }

    #[test]
    fn model_rejects_invalid_dma_requests() {
        let mut host = DeterministicHost::default();
        let device = Host::get_device(&mut host);
        assert!(matches!(
            HostDevice::alloc_dma(&mut host, device, 16, 3, DmaAccess::Bidirectional),
            Err(Error::Invalid)
        ));
    }
}
