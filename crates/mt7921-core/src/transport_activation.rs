//! Resource-free policy for activating and quiescing the MT7921 PCI transport.
//!
//! The operations are intentionally semantic.  BAR layout, DMA ownership, PCI
//! configuration access, and interrupt resources belong to the caller.

use alloc::vec::Vec;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ActivationStage {
    PrepareDescriptors,
    MaskAndAcknowledgeInterrupts,
    AcquireConnOwnership,
    ResetWfsys,
    QuiesceWfdma,
    ConfigureDmashdl,
    RouteRings,
    InstallInterrupt,
    ConfigurePrefetch,
    EnableBusMaster,
    EnableMacInterrupt,
    EnableWfdma,
    EnableHostInterrupt,
    AcquireTopOwnership,
    DisableL0s,
    SetSwdefNormal,
    FinalReadback,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum InterruptInstallState {
    NotAttempted,
    PossiblyInstalled,
    InstalledAndQuiet,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum BusMasterState {
    Disabled,
    PossiblyEnabled,
    Enabled,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ActivationState {
    pub mutation_attempted: bool,
    pub interrupt: InterruptInstallState,
    pub bus_master: BusMasterState,
}

impl ActivationState {
    pub const fn initial() -> Self {
        Self {
            mutation_attempted: false,
            interrupt: InterruptInstallState::NotAttempted,
            bus_master: BusMasterState::Disabled,
        }
    }
}

impl Default for ActivationState {
    fn default() -> Self {
        Self::initial()
    }
}

/// Physical operations required by the MT7921 transport policy.
///
/// Implementations own all resource-specific details.  In particular, an
/// implementation must make every `verify_*` operation an exact readback and
/// must bound `disable_wfdma_and_wait_idle` rather than waiting forever.
pub trait TransportActivationOps {
    type Error;

    fn prepare_descriptors(&mut self) -> Result<(), Self::Error>;
    fn mask_and_verify_mac_interrupts(&mut self) -> Result<(), Self::Error>;
    fn mask_ack_and_verify_host_interrupts(&mut self) -> Result<(), Self::Error>;
    fn acquire_conn_ownership(&mut self) -> Result<(), Self::Error>;
    fn reset_wfsys(&mut self) -> Result<(), Self::Error>;
    fn disable_wfdma_and_wait_idle(&mut self) -> Result<(), Self::Error>;
    fn configure_dmashdl(&mut self) -> Result<(), Self::Error>;
    fn route_rings(&mut self) -> Result<(), Self::Error>;
    fn install_interrupt(&mut self) -> Result<(), Self::Error>;
    fn verify_interrupt_quiet(&mut self) -> Result<(), Self::Error>;
    fn configure_prefetch(&mut self) -> Result<(), Self::Error>;
    fn enable_bus_master(&mut self) -> Result<(), Self::Error>;
    fn verify_bus_master_enabled(&mut self) -> Result<(), Self::Error>;
    fn enable_mac_interrupt(&mut self) -> Result<(), Self::Error>;
    fn enable_wfdma(&mut self) -> Result<(), Self::Error>;
    fn enable_host_interrupt(&mut self) -> Result<(), Self::Error>;
    fn acquire_top_ownership(&mut self) -> Result<(), Self::Error>;
    fn disable_l0s(&mut self) -> Result<(), Self::Error>;
    fn set_swdef_normal(&mut self) -> Result<(), Self::Error>;
    fn final_readback(&mut self) -> Result<(), Self::Error>;

    fn disable_interrupt(&mut self) -> Result<(), Self::Error>;
    fn disable_bus_master(&mut self) -> Result<(), Self::Error>;
    fn verify_bus_master_disabled(&mut self) -> Result<(), Self::Error>;
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum QuiesceStep {
    MaskMacInterrupts,
    MaskAndAcknowledgeHostInterrupts,
    DisableWfdmaAndWaitIdle,
    DisableInterrupt,
    DisableBusMaster,
    VerifyBusMasterDisabled,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct QuiesceError<E> {
    pub step: QuiesceStep,
    pub source: E,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ActivationError<E> {
    pub stage: ActivationStage,
    pub source: E,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ActivationFailure<E> {
    pub primary: ActivationError<E>,
    pub cleanup: Vec<QuiesceError<E>>,
    pub state: ActivationState,
}

/// Attempt every transport-safety action, retaining every failure.
///
/// Calls are unconditional: an earlier operation can have taken effect even
/// when it returned an error, and cleanup must not trust the activation ledger.
pub fn transport_quiesce<T: TransportActivationOps>(
    ops: &mut T,
    state: &mut ActivationState,
) -> Vec<QuiesceError<T::Error>> {
    let mut errors = Vec::new();
    macro_rules! attempt {
        ($step:expr, $operation:expr) => {
            if let Err(source) = $operation {
                errors.push(QuiesceError {
                    step: $step,
                    source,
                });
            }
        };
    }

    attempt!(
        QuiesceStep::MaskMacInterrupts,
        ops.mask_and_verify_mac_interrupts()
    );
    attempt!(
        QuiesceStep::MaskAndAcknowledgeHostInterrupts,
        ops.mask_ack_and_verify_host_interrupts()
    );
    attempt!(
        QuiesceStep::DisableWfdmaAndWaitIdle,
        ops.disable_wfdma_and_wait_idle()
    );
    match ops.disable_interrupt() {
        Ok(()) => state.interrupt = InterruptInstallState::NotAttempted,
        Err(source) => errors.push(QuiesceError {
            step: QuiesceStep::DisableInterrupt,
            source,
        }),
    }
    // Clearing BME is itself ambiguous until the independent exact proof.
    state.bus_master = BusMasterState::PossiblyEnabled;
    attempt!(QuiesceStep::DisableBusMaster, ops.disable_bus_master());
    match ops.verify_bus_master_disabled() {
        Ok(()) => state.bus_master = BusMasterState::Disabled,
        Err(source) => errors.push(QuiesceError {
            step: QuiesceStep::VerifyBusMasterDisabled,
            source,
        }),
    }
    errors
}

/// Activate the transport in the Linux-derived containment order.
///
/// Every failure after the first possibly-mutating call funnels through one
/// attempt-all quiesce pass.  The failure retains both the primary error and
/// all cleanup errors, together with the conservative resulting state.
pub fn activate_transport<T: TransportActivationOps>(
    ops: &mut T,
) -> Result<ActivationState, ActivationFailure<T::Error>> {
    let mut state = ActivationState::initial();

    macro_rules! stage {
        ($stage:expr, $operation:expr) => {{
            state.mutation_attempted = true;
            if let Err(source) = $operation {
                let primary = ActivationError {
                    stage: $stage,
                    source,
                };
                let cleanup = transport_quiesce(ops, &mut state);
                return Err(ActivationFailure {
                    primary,
                    cleanup,
                    state,
                });
            }
        }};
    }

    stage!(
        ActivationStage::PrepareDescriptors,
        ops.prepare_descriptors()
    );
    stage!(
        ActivationStage::MaskAndAcknowledgeInterrupts,
        ops.mask_and_verify_mac_interrupts()
    );
    stage!(
        ActivationStage::MaskAndAcknowledgeInterrupts,
        ops.mask_ack_and_verify_host_interrupts()
    );
    stage!(
        ActivationStage::AcquireConnOwnership,
        ops.acquire_conn_ownership()
    );
    stage!(ActivationStage::ResetWfsys, ops.reset_wfsys());
    stage!(
        ActivationStage::QuiesceWfdma,
        ops.disable_wfdma_and_wait_idle()
    );
    stage!(ActivationStage::ConfigureDmashdl, ops.configure_dmashdl());
    stage!(ActivationStage::RouteRings, ops.route_rings());

    state.interrupt = InterruptInstallState::PossiblyInstalled;
    stage!(ActivationStage::InstallInterrupt, ops.install_interrupt());
    stage!(
        ActivationStage::InstallInterrupt,
        ops.verify_interrupt_quiet()
    );
    state.interrupt = InterruptInstallState::InstalledAndQuiet;

    stage!(ActivationStage::ConfigurePrefetch, ops.configure_prefetch());

    state.bus_master = BusMasterState::PossiblyEnabled;
    stage!(ActivationStage::EnableBusMaster, ops.enable_bus_master());
    stage!(
        ActivationStage::EnableBusMaster,
        ops.verify_bus_master_enabled()
    );
    state.bus_master = BusMasterState::Enabled;

    stage!(
        ActivationStage::EnableMacInterrupt,
        ops.enable_mac_interrupt()
    );
    stage!(ActivationStage::EnableWfdma, ops.enable_wfdma());
    stage!(
        ActivationStage::EnableHostInterrupt,
        ops.enable_host_interrupt()
    );
    stage!(
        ActivationStage::AcquireTopOwnership,
        ops.acquire_top_ownership()
    );
    stage!(ActivationStage::DisableL0s, ops.disable_l0s());
    stage!(ActivationStage::SetSwdefNormal, ops.set_swdef_normal());
    stage!(ActivationStage::FinalReadback, ops.final_readback());
    Ok(state)
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::vec;

    const ACTIVATION_CALLS: [&str; 20] = [
        "prepare_descriptors",
        "mask_mac",
        "mask_ack_host",
        "conn_ownership",
        "reset_wfsys",
        "disable_wfdma_idle",
        "dmashdl",
        "route_rings",
        "install_interrupt",
        "verify_interrupt_quiet",
        "prefetch",
        "enable_bme",
        "verify_bme_enabled",
        "enable_mac",
        "enable_wfdma",
        "enable_host",
        "top_ownership",
        "disable_l0s",
        "swdef_normal",
        "final_readback",
    ];

    const ACTIVATION_STAGES: [ActivationStage; 20] = [
        ActivationStage::PrepareDescriptors,
        ActivationStage::MaskAndAcknowledgeInterrupts,
        ActivationStage::MaskAndAcknowledgeInterrupts,
        ActivationStage::AcquireConnOwnership,
        ActivationStage::ResetWfsys,
        ActivationStage::QuiesceWfdma,
        ActivationStage::ConfigureDmashdl,
        ActivationStage::RouteRings,
        ActivationStage::InstallInterrupt,
        ActivationStage::InstallInterrupt,
        ActivationStage::ConfigurePrefetch,
        ActivationStage::EnableBusMaster,
        ActivationStage::EnableBusMaster,
        ActivationStage::EnableMacInterrupt,
        ActivationStage::EnableWfdma,
        ActivationStage::EnableHostInterrupt,
        ActivationStage::AcquireTopOwnership,
        ActivationStage::DisableL0s,
        ActivationStage::SetSwdefNormal,
        ActivationStage::FinalReadback,
    ];

    const CLEANUP_CALLS: [&str; 6] = [
        "mask_mac",
        "mask_ack_host",
        "disable_wfdma_idle",
        "disable_interrupt",
        "disable_bme",
        "verify_bme_disabled",
    ];

    const MAC: u8 = 1 << 0;
    const HOST: u8 = 1 << 1;
    const WFDMA: u8 = 1 << 2;
    const IRQ: u8 = 1 << 3;
    const BME_CLEAR: u8 = 1 << 4;
    const BME_PROOF: u8 = 1 << 5;
    const ALL_CLEANUP: u8 = MAC | HOST | WFDMA | IRQ | BME_CLEAR | BME_PROOF;

    struct FakeOps {
        calls: Vec<&'static str>,
        fail_activation_call: Option<usize>,
        activation_calls: usize,
        primary_failed: bool,
        cleanup_failures: u8,
    }

    impl FakeOps {
        fn new(fail_activation_call: Option<usize>, cleanup_failures: u8) -> Self {
            Self {
                calls: Vec::new(),
                fail_activation_call,
                activation_calls: 0,
                primary_failed: fail_activation_call.is_none(),
                cleanup_failures,
            }
        }

        fn activation(&mut self, name: &'static str) -> Result<(), &'static str> {
            self.calls.push(name);
            let index = self.activation_calls;
            self.activation_calls += 1;
            if self.fail_activation_call == Some(index) {
                self.primary_failed = true;
                Err(name)
            } else {
                Ok(())
            }
        }

        fn shared(&mut self, name: &'static str, cleanup_bit: u8) -> Result<(), &'static str> {
            if self.primary_failed {
                self.calls.push(name);
                if self.cleanup_failures & cleanup_bit != 0 {
                    Err(name)
                } else {
                    Ok(())
                }
            } else {
                self.activation(name)
            }
        }
    }

    impl TransportActivationOps for FakeOps {
        type Error = &'static str;

        fn prepare_descriptors(&mut self) -> Result<(), Self::Error> {
            self.activation("prepare_descriptors")
        }
        fn mask_and_verify_mac_interrupts(&mut self) -> Result<(), Self::Error> {
            self.shared("mask_mac", MAC)
        }
        fn mask_ack_and_verify_host_interrupts(&mut self) -> Result<(), Self::Error> {
            self.shared("mask_ack_host", HOST)
        }
        fn acquire_conn_ownership(&mut self) -> Result<(), Self::Error> {
            self.activation("conn_ownership")
        }
        fn reset_wfsys(&mut self) -> Result<(), Self::Error> {
            self.activation("reset_wfsys")
        }
        fn disable_wfdma_and_wait_idle(&mut self) -> Result<(), Self::Error> {
            self.shared("disable_wfdma_idle", WFDMA)
        }
        fn configure_dmashdl(&mut self) -> Result<(), Self::Error> {
            self.activation("dmashdl")
        }
        fn route_rings(&mut self) -> Result<(), Self::Error> {
            self.activation("route_rings")
        }
        fn install_interrupt(&mut self) -> Result<(), Self::Error> {
            self.activation("install_interrupt")
        }
        fn verify_interrupt_quiet(&mut self) -> Result<(), Self::Error> {
            self.activation("verify_interrupt_quiet")
        }
        fn configure_prefetch(&mut self) -> Result<(), Self::Error> {
            self.activation("prefetch")
        }
        fn enable_bus_master(&mut self) -> Result<(), Self::Error> {
            self.activation("enable_bme")
        }
        fn verify_bus_master_enabled(&mut self) -> Result<(), Self::Error> {
            self.activation("verify_bme_enabled")
        }
        fn enable_mac_interrupt(&mut self) -> Result<(), Self::Error> {
            self.activation("enable_mac")
        }
        fn enable_wfdma(&mut self) -> Result<(), Self::Error> {
            self.activation("enable_wfdma")
        }
        fn enable_host_interrupt(&mut self) -> Result<(), Self::Error> {
            self.activation("enable_host")
        }
        fn acquire_top_ownership(&mut self) -> Result<(), Self::Error> {
            self.activation("top_ownership")
        }
        fn disable_l0s(&mut self) -> Result<(), Self::Error> {
            self.activation("disable_l0s")
        }
        fn set_swdef_normal(&mut self) -> Result<(), Self::Error> {
            self.activation("swdef_normal")
        }
        fn final_readback(&mut self) -> Result<(), Self::Error> {
            self.activation("final_readback")
        }
        fn disable_interrupt(&mut self) -> Result<(), Self::Error> {
            self.shared("disable_interrupt", IRQ)
        }
        fn disable_bus_master(&mut self) -> Result<(), Self::Error> {
            self.shared("disable_bme", BME_CLEAR)
        }
        fn verify_bus_master_disabled(&mut self) -> Result<(), Self::Error> {
            self.shared("verify_bme_disabled", BME_PROOF)
        }
    }

    #[test]
    fn successful_activation_has_exact_order_and_does_not_quiesce() {
        let mut ops = FakeOps {
            calls: Vec::new(),
            fail_activation_call: None,
            activation_calls: 0,
            primary_failed: false,
            cleanup_failures: 0,
        };
        let state = activate_transport(&mut ops).unwrap();
        assert_eq!(ops.calls, ACTIVATION_CALLS);
        assert_eq!(
            state,
            ActivationState {
                mutation_attempted: true,
                interrupt: InterruptInstallState::InstalledAndQuiet,
                bus_master: BusMasterState::Enabled,
            }
        );
    }

    #[test]
    fn every_post_mutation_failure_runs_one_complete_quiesce() {
        for failed_at in 0..ACTIVATION_CALLS.len() {
            let mut ops = FakeOps::new(Some(failed_at), 0);
            let failure = activate_transport(&mut ops).unwrap_err();
            assert_eq!(failure.primary.stage, ACTIVATION_STAGES[failed_at]);
            assert_eq!(failure.primary.source, ACTIVATION_CALLS[failed_at]);
            assert!(failure.cleanup.is_empty());
            assert!(failure.state.mutation_attempted);
            assert_eq!(failure.state.interrupt, InterruptInstallState::NotAttempted);
            assert_eq!(failure.state.bus_master, BusMasterState::Disabled);
            assert_eq!(
                &ops.calls[ops.calls.len() - CLEANUP_CALLS.len()..],
                &CLEANUP_CALLS
            );
            assert_eq!(ops.calls.len(), failed_at + 1 + CLEANUP_CALLS.len());
        }
    }

    #[test]
    fn ambiguous_irq_install_is_retained_when_disable_also_fails() {
        let mut ops = FakeOps::new(Some(8), IRQ);
        let failure = activate_transport(&mut ops).unwrap_err();
        assert_eq!(failure.primary.source, "install_interrupt");
        assert_eq!(
            failure.cleanup,
            vec![QuiesceError {
                step: QuiesceStep::DisableInterrupt,
                source: "disable_interrupt",
            }]
        );
        assert_eq!(
            failure.state.interrupt,
            InterruptInstallState::PossiblyInstalled
        );
    }

    #[test]
    fn ambiguous_bme_enable_retains_primary_clear_and_proof_errors() {
        let mut ops = FakeOps::new(Some(11), BME_CLEAR | BME_PROOF);
        let failure = activate_transport(&mut ops).unwrap_err();
        assert_eq!(failure.primary.source, "enable_bme");
        assert_eq!(
            failure.cleanup,
            vec![
                QuiesceError {
                    step: QuiesceStep::DisableBusMaster,
                    source: "disable_bme",
                },
                QuiesceError {
                    step: QuiesceStep::VerifyBusMasterDisabled,
                    source: "verify_bme_disabled",
                },
            ]
        );
        assert_eq!(failure.state.bus_master, BusMasterState::PossiblyEnabled);
    }

    #[test]
    fn primary_and_every_cleanup_error_are_retained_in_cleanup_order() {
        let mut ops = FakeOps::new(Some(19), ALL_CLEANUP);
        let failure = activate_transport(&mut ops).unwrap_err();
        assert_eq!(failure.primary.source, "final_readback");
        assert_eq!(
            failure
                .cleanup
                .iter()
                .map(|error| error.step)
                .collect::<Vec<_>>(),
            vec![
                QuiesceStep::MaskMacInterrupts,
                QuiesceStep::MaskAndAcknowledgeHostInterrupts,
                QuiesceStep::DisableWfdmaAndWaitIdle,
                QuiesceStep::DisableInterrupt,
                QuiesceStep::DisableBusMaster,
                QuiesceStep::VerifyBusMasterDisabled,
            ]
        );
        assert_eq!(
            failure.state.interrupt,
            InterruptInstallState::InstalledAndQuiet
        );
        assert_eq!(failure.state.bus_master, BusMasterState::PossiblyEnabled);
        assert_eq!(
            &ops.calls[ops.calls.len() - CLEANUP_CALLS.len()..],
            &CLEANUP_CALLS
        );
    }
}
