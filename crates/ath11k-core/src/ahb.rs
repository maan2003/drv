extern crate alloc;

use alloc::vec::Vec;
use ath11k_platform_backend::{Backend, Device, Error, Interrupt};

const WINDOW_START: u32 = 0x0008_0000;
const WINDOW_MASK: u32 = 0x0007_ffff;
const UMAC_OFFSET: u32 = 0x00a0_0000;
const CE0_SRC_OFFSET: u32 = 0x01b8_0000;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RegisterWindow {
    Direct,
    Dp,
    CopyEngine,
}
impl RegisterWindow {
    pub const fn for_offset(offset: u32) -> Self {
        if (offset ^ UMAC_OFFSET) < WINDOW_MASK {
            Self::Dp
        } else if (offset ^ CE0_SRC_OFFSET) < WINDOW_MASK {
            Self::CopyEngine
        } else {
            Self::Direct
        }
    }
    pub const fn mapped_offset(self, offset: u32) -> u32 {
        let start = match self {
            Self::Direct => 0,
            Self::Dp => WINDOW_START,
            Self::CopyEngine => 2 * WINDOW_START,
        };
        start + (offset & WINDOW_MASK)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MsiUser {
    CopyEngine,
    DataPath,
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct InterruptRoute {
    pub vector: u8,
    pub user: MsiUser,
    pub irq: Wcn6750Irq,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Wcn6750Irq {
    CopyEngine(u8),
    DataPathExternalGroup(u8),
}

const fn ce_route(vector: u8, engine: u8) -> InterruptRoute {
    InterruptRoute {
        vector,
        user: MsiUser::CopyEngine,
        irq: Wcn6750Irq::CopyEngine(engine),
    }
}

const fn dp_route(group: u8) -> InterruptRoute {
    InterruptRoute {
        vector: 10 + group,
        user: MsiUser::DataPath,
        irq: Wcn6750Irq::DataPathExternalGroup(group),
    }
}

// pcic.c gives WCN6750 CE vectors [0, 10) and DP vectors [10, 28).  Only
// sources selected by the CE attributes and WCN6750 ring mask are opened.
pub const WCN6750_CE_INTERRUPT_ROUTES: [InterruptRoute; 7] = [
    ce_route(0, 0),
    ce_route(1, 1),
    ce_route(2, 2),
    ce_route(3, 3),
    ce_route(4, 5),
    ce_route(5, 7),
    ce_route(6, 8),
];
pub const WCN6750_DP_INTERRUPT_ROUTES: [InterruptRoute; 9] = [
    dp_route(0),
    dp_route(1),
    dp_route(2),
    dp_route(4),
    dp_route(6),
    dp_route(7),
    dp_route(8),
    dp_route(9),
    dp_route(10),
];
pub const WCN6750_INTERRUPT_ROUTES: [InterruptRoute; 16] = [
    WCN6750_CE_INTERRUPT_ROUTES[0],
    WCN6750_CE_INTERRUPT_ROUTES[1],
    WCN6750_CE_INTERRUPT_ROUTES[2],
    WCN6750_CE_INTERRUPT_ROUTES[3],
    WCN6750_CE_INTERRUPT_ROUTES[4],
    WCN6750_CE_INTERRUPT_ROUTES[5],
    WCN6750_CE_INTERRUPT_ROUTES[6],
    WCN6750_DP_INTERRUPT_ROUTES[0],
    WCN6750_DP_INTERRUPT_ROUTES[1],
    WCN6750_DP_INTERRUPT_ROUTES[2],
    WCN6750_DP_INTERRUPT_ROUTES[3],
    WCN6750_DP_INTERRUPT_ROUTES[4],
    WCN6750_DP_INTERRUPT_ROUTES[5],
    WCN6750_DP_INTERRUPT_ROUTES[6],
    WCN6750_DP_INTERRUPT_ROUTES[7],
    WCN6750_DP_INTERRUPT_ROUTES[8],
];

pub struct Wcn6750Interrupts<B: Backend> {
    ce: Wcn6750CeWaiter<B>,
    dp: Wcn6750DpInterrupts<B>,
}

impl<B: Backend> Wcn6750Interrupts<B> {
    /// Configure the request-IRQ equivalent. CE routes are live immediately;
    /// external DP routes retain their device capability until `enable`.
    pub fn configure(device: Device<B>) -> Result<Self, Error> {
        let mut interrupts = Vec::with_capacity(WCN6750_CE_INTERRUPT_ROUTES.len());
        for route in WCN6750_CE_INTERRUPT_ROUTES {
            interrupts.push(device.open_interrupt(u32::from(route.vector))?);
        }
        Ok(Self {
            ce: Wcn6750CeWaiter {
                routes: &WCN6750_CE_INTERRUPT_ROUTES,
                interrupts,
            },
            dp: Wcn6750DpInterrupts {
                device,
                routes: &WCN6750_DP_INTERRUPT_ROUTES,
                interrupts: None,
            },
        })
    }

    pub fn split(self) -> (Wcn6750CeWaiter<B>, Wcn6750DpInterrupts<B>) {
        (self.ce, self.dp)
    }
}

pub struct Wcn6750CeWaiter<B: Backend> {
    routes: &'static [InterruptRoute],
    interrupts: Vec<Interrupt<B>>,
}

impl<B: Backend> Wcn6750CeWaiter<B> {
    pub fn routes(&self) -> &'static [InterruptRoute] {
        self.routes
    }

    pub fn wait_any(&mut self, deadline_ns: u64) -> Result<Vec<Wcn6750Irq>, Error> {
        let handles = self.interrupts.iter().collect::<Vec<_>>();
        let ready = Interrupt::wait_any(&handles, deadline_ns)?;
        Ok(ready
            .events()
            .iter()
            .filter_map(|event| route_for_vector(event.vector, self.routes))
            .collect())
    }
}

impl<B: Backend> ath11k_ce::CeCompletionWait for Wcn6750CeWaiter<B> {
    fn wait_for_ce(&mut self, deadline_ns: u64) -> Result<bool, ath11k_ce::CeError> {
        Ok(!self.wait_any(deadline_ns)?.is_empty())
    }
}

pub struct Wcn6750DpInterrupts<B: Backend> {
    device: Device<B>,
    routes: &'static [InterruptRoute],
    interrupts: Option<Vec<Interrupt<B>>>,
}

impl<B: Backend> Wcn6750DpInterrupts<B> {
    pub fn is_enabled(&self) -> bool {
        self.interrupts.is_some()
    }

    pub fn routes(&self) -> &'static [InterruptRoute] {
        self.routes
    }

    /// Open every route before publishing the enabled state. On failure the
    /// temporary handles drop, leaving the previous disabled state intact.
    pub fn enable(&mut self) -> Result<(), Error> {
        if self.is_enabled() {
            return Ok(());
        }
        let mut interrupts = Vec::with_capacity(self.routes.len());
        for route in self.routes {
            interrupts.push(self.device.open_interrupt(u32::from(route.vector))?);
        }
        self.interrupts = Some(interrupts);
        Ok(())
    }

    pub fn disable(&mut self) {
        self.interrupts = None;
    }

    pub fn wait_any(&mut self, deadline_ns: u64) -> Result<Vec<Wcn6750Irq>, Error> {
        let interrupts = self.interrupts.as_ref().ok_or(Error::Invalid)?;
        let handles = interrupts.iter().collect::<Vec<_>>();
        let ready = Interrupt::wait_any(&handles, deadline_ns)?;
        Ok(ready
            .events()
            .iter()
            .filter_map(|event| route_for_vector(event.vector, self.routes))
            .collect())
    }
}

fn route_for_vector(vector: u32, routes: &[InterruptRoute]) -> Option<Wcn6750Irq> {
    routes
        .iter()
        .find(|route| u32::from(route.vector) == vector)
        .map(|route| route.irq)
}

/// Hardware-facing body of `ath11k_ahb_ce_interrupt_handler` after the
/// platform backend has delivered the interrupt notification.
pub fn service_ce_interrupt<B: ath11k_platform_backend::Backend>(
    pipes: &mut ath11k_ce::CePipes<B>,
    mmio: &ath11k_platform_backend::MmioRegion<B>,
    remote_read_pointers: &mut ath11k_platform_backend::CoherentDma<
        B,
        ath11k_platform_backend::Bidirectional,
    >,
    engine: usize,
) -> Result<ath11k_ce::CeServiceBatch<B>, ath11k_ce::CeError> {
    pipes.per_engine_service(mmio, remote_read_pointers, engine)
}

/// Hardware-facing NAPI poll body of `ath11k_ahb_ext_grp_napi_poll`.
pub fn service_dp_external_group<
    B: ath11k_platform_backend::Backend,
    R: ath11k_dp::DpRingOps<B>,
>(
    data_path: &mut ath11k_dp::tx::ClientDataPath<B, R>,
    budget: usize,
) -> Result<ath11k_dp::tx::ServiceResult, ath11k_dp::DpError> {
    data_path.ath11k_dp_service_srng(budget)
}

pub enum Wcn6750InterruptService<B: Backend> {
    CopyEngine(ath11k_ce::CeServiceBatch<B>),
    DataPath(ath11k_dp::tx::ServiceResult),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Wcn6750InterruptServiceError {
    CopyEngine(ath11k_ce::CeError),
    DataPath(ath11k_dp::DpError),
}

/// Dispatch a typed route to its hardware-facing service body. Scheduling and
/// NAPI completion policy intentionally remain above this layer.
pub fn dispatch_wcn6750_interrupt<B: Backend, R: ath11k_hal::Rings<B> + ath11k_dp::DpRingOps<B>>(
    irq: Wcn6750Irq,
    pipes: &mut ath11k_ce::CePipes<B>,
    mmio: &ath11k_platform_backend::MmioRegion<B>,
    remote_read_pointers: &mut ath11k_platform_backend::CoherentDma<
        B,
        ath11k_platform_backend::Bidirectional,
    >,
    data_path: &mut ath11k_dp::tx::ClientDataPath<B, R>,
    budget: usize,
) -> Result<Wcn6750InterruptService<B>, Wcn6750InterruptServiceError> {
    match irq {
        Wcn6750Irq::CopyEngine(engine) => {
            service_ce_interrupt(pipes, mmio, remote_read_pointers, usize::from(engine))
                .map(Wcn6750InterruptService::CopyEngine)
                .map_err(Wcn6750InterruptServiceError::CopyEngine)
        }
        Wcn6750Irq::DataPathExternalGroup(_) => service_dp_external_group(data_path, budget)
            .map(Wcn6750InterruptService::DataPath)
            .map_err(Wcn6750InterruptServiceError::DataPath),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::{rc::Rc, vec};
    use ath11k_ce::CeCompletionWait;
    use ath11k_platform_backend::{DmaConstraints, DmaDirection, IrqEvent};
    use core::{cell::RefCell, ops::Range};

    #[derive(Default)]
    struct IrqState {
        fail_open: Option<u32>,
        opened: Vec<u32>,
        released: Vec<u32>,
        ready: Vec<u32>,
        wait_sets: Vec<Vec<u32>>,
    }

    struct IrqBackend(Rc<RefCell<IrqState>>);

    impl Backend for IrqBackend {
        type Region = ();
        type Dma = ();
        type Interrupt = u32;

        fn generation(&self) -> u64 {
            1
        }
        fn open_region(&mut self, _: u8) -> Result<(), Error> {
            Err(Error::Invalid)
        }
        fn region_len(&self, _: &()) -> usize {
            0
        }
        fn read_u32(&mut self, _: &(), _: usize) -> Result<u32, Error> {
            Err(Error::Invalid)
        }
        fn write_u32(&mut self, _: &(), _: usize, _: u32) -> Result<(), Error> {
            Err(Error::Invalid)
        }
        fn write_dma_address(
            &mut self,
            _: &(),
            _: usize,
            _: Option<usize>,
            _: &(),
            _: usize,
        ) -> Result<(), Error> {
            Err(Error::Invalid)
        }
        fn dma_device_address(&self, _: &(), _: usize) -> Result<u64, Error> {
            Err(Error::Invalid)
        }
        fn alloc_dma(&mut self, _: usize, _: usize, _: DmaDirection, _: bool) -> Result<(), Error> {
            Err(Error::Invalid)
        }
        fn alloc_dma_constrained(
            &mut self,
            _: usize,
            _: DmaConstraints,
            _: DmaDirection,
            _: bool,
        ) -> Result<(), Error> {
            Err(Error::Invalid)
        }
        fn dma_read(&mut self, _: &(), _: Range<usize>, _: &mut [u8]) -> Result<(), Error> {
            Err(Error::Invalid)
        }
        fn dma_write(&mut self, _: &(), _: Range<usize>, _: &[u8]) -> Result<(), Error> {
            Err(Error::Invalid)
        }
        fn sync_for_cpu(&mut self, _: &(), _: Range<usize>) -> Result<(), Error> {
            Err(Error::Invalid)
        }
        fn sync_for_device(&mut self, _: &(), _: Range<usize>) -> Result<(), Error> {
            Err(Error::Invalid)
        }
        fn open_interrupt(&mut self, vector: u32) -> Result<u32, Error> {
            let mut state = self.0.borrow_mut();
            if state.fail_open == Some(vector) {
                return Err(Error::DeviceFault);
            }
            state.opened.push(vector);
            Ok(vector)
        }
        fn wait_interrupt(&mut self, _: &u32, _: u64) -> Result<Option<IrqEvent>, Error> {
            Err(Error::Invalid)
        }
        fn wait_any(
            &mut self,
            interrupts: &[&u32],
            deadline_ns: u64,
        ) -> Result<Vec<IrqEvent>, Error> {
            let mut state = self.0.borrow_mut();
            state
                .wait_sets
                .push(interrupts.iter().map(|interrupt| **interrupt).collect());
            Ok(state
                .ready
                .iter()
                .filter(|vector| interrupts.iter().any(|interrupt| **interrupt == **vector))
                .map(|vector| IrqEvent {
                    vector: *vector,
                    count: 1,
                    at_ns: deadline_ns,
                })
                .collect())
        }
        fn reset(&mut self) -> Result<u64, Error> {
            Ok(1)
        }
        fn release_region(&mut self, _: ()) {}
        fn release_dma(&mut self, _: ()) {}
        fn release_interrupt(&mut self, interrupt: u32) {
            self.0.borrow_mut().released.push(interrupt);
        }
    }

    fn irq_device(state: &Rc<RefCell<IrqState>>) -> Device<IrqBackend> {
        Device::from_backend(IrqBackend(state.clone()))
    }

    #[test]
    fn configure_and_dp_enable_are_transactional_and_disable_drops_routes() {
        let failed = Rc::new(RefCell::new(IrqState {
            fail_open: Some(3),
            ..IrqState::default()
        }));
        assert_eq!(
            Wcn6750Interrupts::configure(irq_device(&failed)).err(),
            Some(Error::DeviceFault)
        );
        assert_eq!(failed.borrow().released, vec![0, 1, 2]);

        let state = Rc::new(RefCell::new(IrqState::default()));
        let (_, mut dp) = Wcn6750Interrupts::configure(irq_device(&state))
            .unwrap()
            .split();
        assert_eq!(dp.wait_any(1), Err(Error::Invalid));
        state.borrow_mut().fail_open = Some(16);
        assert_eq!(dp.enable(), Err(Error::DeviceFault));
        assert!(!dp.is_enabled());
        assert_eq!(&state.borrow().released[7..], &[10, 11, 12, 14]);

        state.borrow_mut().fail_open = None;
        dp.enable().unwrap();
        assert!(dp.is_enabled());
        state.borrow_mut().ready = vec![11, 18];
        assert_eq!(
            dp.wait_any(99).unwrap(),
            vec![
                Wcn6750Irq::DataPathExternalGroup(1),
                Wcn6750Irq::DataPathExternalGroup(8)
            ]
        );
        dp.disable();
        assert!(!dp.is_enabled());
        assert_eq!(dp.wait_any(100), Err(Error::Invalid));
        for vector in [10, 11, 12, 14, 16, 17, 18, 19, 20] {
            assert!(state.borrow().released.contains(&vector));
        }
    }

    #[test]
    fn ce_waiter_uses_one_wait_any_and_preserves_typed_ready_routes() {
        let state = Rc::new(RefCell::new(IrqState::default()));
        let (mut ce, _) = Wcn6750Interrupts::configure(irq_device(&state))
            .unwrap()
            .split();
        state.borrow_mut().ready = vec![1, 5];
        assert_eq!(
            ce.wait_any(42).unwrap(),
            vec![Wcn6750Irq::CopyEngine(1), Wcn6750Irq::CopyEngine(7)]
        );
        assert_eq!(state.borrow().wait_sets, vec![vec![0, 1, 2, 3, 4, 5, 6]]);
        state.borrow_mut().ready = vec![0];
        assert!(CeCompletionWait::wait_for_ce(&mut ce, 43).unwrap());
        assert_eq!(state.borrow().wait_sets.len(), 2);
    }
}
