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
}

// pcic.c's WCN6750 entry: 28 total vectors, CE [0, 10), DP [10, 28).
pub const WCN6750_INTERRUPT_ROUTES: [InterruptRoute; 28] = [
    InterruptRoute {
        vector: 0,
        user: MsiUser::CopyEngine,
    },
    InterruptRoute {
        vector: 1,
        user: MsiUser::CopyEngine,
    },
    InterruptRoute {
        vector: 2,
        user: MsiUser::CopyEngine,
    },
    InterruptRoute {
        vector: 3,
        user: MsiUser::CopyEngine,
    },
    InterruptRoute {
        vector: 4,
        user: MsiUser::CopyEngine,
    },
    InterruptRoute {
        vector: 5,
        user: MsiUser::CopyEngine,
    },
    InterruptRoute {
        vector: 6,
        user: MsiUser::CopyEngine,
    },
    InterruptRoute {
        vector: 7,
        user: MsiUser::CopyEngine,
    },
    InterruptRoute {
        vector: 8,
        user: MsiUser::CopyEngine,
    },
    InterruptRoute {
        vector: 9,
        user: MsiUser::CopyEngine,
    },
    InterruptRoute {
        vector: 10,
        user: MsiUser::DataPath,
    },
    InterruptRoute {
        vector: 11,
        user: MsiUser::DataPath,
    },
    InterruptRoute {
        vector: 12,
        user: MsiUser::DataPath,
    },
    InterruptRoute {
        vector: 13,
        user: MsiUser::DataPath,
    },
    InterruptRoute {
        vector: 14,
        user: MsiUser::DataPath,
    },
    InterruptRoute {
        vector: 15,
        user: MsiUser::DataPath,
    },
    InterruptRoute {
        vector: 16,
        user: MsiUser::DataPath,
    },
    InterruptRoute {
        vector: 17,
        user: MsiUser::DataPath,
    },
    InterruptRoute {
        vector: 18,
        user: MsiUser::DataPath,
    },
    InterruptRoute {
        vector: 19,
        user: MsiUser::DataPath,
    },
    InterruptRoute {
        vector: 20,
        user: MsiUser::DataPath,
    },
    InterruptRoute {
        vector: 21,
        user: MsiUser::DataPath,
    },
    InterruptRoute {
        vector: 22,
        user: MsiUser::DataPath,
    },
    InterruptRoute {
        vector: 23,
        user: MsiUser::DataPath,
    },
    InterruptRoute {
        vector: 24,
        user: MsiUser::DataPath,
    },
    InterruptRoute {
        vector: 25,
        user: MsiUser::DataPath,
    },
    InterruptRoute {
        vector: 26,
        user: MsiUser::DataPath,
    },
    InterruptRoute {
        vector: 27,
        user: MsiUser::DataPath,
    },
];

/// Hardware-facing body of `ath11k_ahb_ce_interrupt_handler` after the host
/// interrupt source has been masked by the platform backend.
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
pub fn service_dp_external_group<B: ath11k_platform_backend::Backend, R: ath11k_hal::Rings<B>>(
    data_path: &mut ath11k_dp::tx::ClientDataPath<B, R>,
    budget: usize,
) -> Result<ath11k_dp::tx::ServiceResult, ath11k_dp::DpError> {
    data_path.ath11k_dp_service_srng(budget)
}
