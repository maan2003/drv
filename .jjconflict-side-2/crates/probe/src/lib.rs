mod bindings;

use bindings::drv::hardware::broker::{self, DmaAccess, Error};
use bindings::exports::drv::hardware::probe::{Guest, Report};

struct Component;

impl Guest for Component {
    fn run() -> Result<Report, Error> {
        let device = broker::get_device();
        let generation_before = device.generation();
        let arena = device.alloc_dma(16, 8, DmaAccess::Bidirectional)?;
        let iova = arena.iova()?;
        if arena.length()? != 16 || iova % 8 != 0 {
            return Err(Error::DeviceFault);
        }

        let expected = [0x44, 0x52, 0x56, 0x21];
        arena.write(4, &expected)?;
        let dma_round_trip = arena.read(4, expected.len() as u32)? == expected;
        let bounds_rejected = matches!(arena.write(15, &[1, 2]), Err(Error::OutOfBounds));

        let region = device.open_region(0)?;
        region.write_u32(0, (iova + 4) as u32)?;
        region.write_u32(4, expected.len() as u32)?;
        let bar_bounds_rejected = matches!(region.write_u32(2, 0), Err(Error::OutOfBounds));
        let interrupt = device.open_interrupt(0)?;
        region.write_u32(8, 1)?;
        if region.read_u32(12)? != 1 {
            return Err(Error::DeviceFault);
        }
        let event = interrupt
            .wait_until(broker::now() + 100)?
            .ok_or(Error::Cancelled)?;
        if region.read_u32(12)? != 0 {
            return Err(Error::DeviceFault);
        }
        let device_dma_observed =
            arena.read(4, expected.len() as u32)? == expected.into_iter().rev().collect::<Vec<_>>();

        let generation_after = device.reset()?;
        let stale_rejected = matches!(arena.read(0, 1), Err(Error::StaleHandle));
        let region_stale_rejected = matches!(region.read_u32(0), Err(Error::StaleHandle));

        Ok(Report {
            generation_before,
            generation_after,
            iova,
            interrupt_count: event.count,
            interrupt_at_ns: event.at_ns,
            dma_round_trip,
            device_dma_observed,
            bounds_rejected,
            bar_bounds_rejected,
            stale_rejected,
            region_stale_rejected,
        })
    }
}

bindings::export!(Component with_types_in bindings);
