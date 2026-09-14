// SPDX-License-Identifier: MIT OR Apache-2.0

//! Chip-generic owner and owned-operation contracts for a client SoftMAC device.
//!
//! The request and response values are the host bindings generated from the
//! project's pinned Fuchsia FIDL schemas. The lifecycle and callback traits
//! are paired by [`runtime::ClientRuntime`], the sole owner of a started
//! device and its MLME/SME state.

#[cfg(any(test, feature = "conformance"))]
pub mod conformance;
mod driver;
pub mod ethernet;
pub mod runtime;

pub use fidl_fuchsia_wlan_common::{
    MacSublayerSupport, SecuritySupport, SpectrumManagementSupport,
};
pub use fidl_fuchsia_wlan_driver::JoinBssRequest;
pub use fidl_fuchsia_wlan_softmac::{
    DiscoverySupport, WlanAssociationConfig, WlanKeyConfiguration, WlanRxInfo,
    WlanSoftmacBaseCancelScanRequest, WlanSoftmacBaseClearAssociationRequest,
    WlanSoftmacBaseSetChannelRequest, WlanSoftmacBaseStartActiveScanResponse,
    WlanSoftmacBaseStartPassiveScanRequest, WlanSoftmacBaseStartPassiveScanResponse,
    WlanSoftmacBaseUpdateWmmParametersRequest, WlanSoftmacQueryResponse,
    WlanSoftmacStartActiveScanRequest, WlanTxInfoFlags, WlanTxResult,
};

/// Immutable publication authority for one bounded operation.
///
/// All deadlines use `std::time::Instant` (Linux CLOCK_MONOTONIC). Drivers
/// check with an injected reading of that clock immediately before publishing,
/// without an intervening await. Expiration or revocation never releases
/// already-published DMA or correlation state.
#[derive(Clone)]
pub struct OperationContext {
    epoch: OperationEpoch,
    deadline: std::time::Instant,
}

impl OperationContext {
    pub(crate) fn new(deadline: std::time::Instant) -> Self {
        Self {
            epoch: OperationEpoch::new(),
            deadline,
        }
    }

    pub(crate) fn for_deadline(&self, deadline: std::time::Instant) -> Self {
        Self {
            epoch: self.epoch.clone(),
            deadline,
        }
    }

    pub fn deadline(&self) -> std::time::Instant {
        self.deadline
    }

    pub fn check(&self, now: std::time::Instant) -> Result<(), zx::Status> {
        if !self.epoch.is_live() {
            Err(zx::Status::CANCELED)
        } else if now >= self.deadline {
            Err(zx::Status::TIMED_OUT)
        } else {
            Ok(())
        }
    }

    pub(crate) fn revoke(&self) {
        self.epoch.revoke();
    }

    pub(crate) fn is_live(&self) -> bool {
        self.epoch.is_live()
    }
}

/// Lifetime identity is independent of each operation's deadline. A successful
/// connection may authorize new work after its original connect budget ends.
#[derive(Clone)]
pub(crate) struct OperationEpoch(std::sync::Arc<std::sync::atomic::AtomicBool>);

impl OperationEpoch {
    pub(crate) fn new() -> Self {
        Self(std::sync::Arc::new(std::sync::atomic::AtomicBool::new(
            true,
        )))
    }
    pub(crate) fn revoke(&self) {
        self.0.store(false, std::sync::atomic::Ordering::Release);
    }
    pub(crate) fn is_live(&self) -> bool {
        self.0.load(std::sync::atomic::Ordering::Acquire)
    }
}

/// Device-to-host callbacks installed by [`WlanSoftmacLifecycle::start`].
pub trait WlanSoftmacUpcalls: Send {
    fn recv(&mut self, bytes: Vec<u8>, info: WlanRxInfo);
    fn report_tx_result(&mut self, result: WlanTxResult);
    fn notify_scan_complete(&mut self, status: zx::Status, scan_id: u64);
}

/// Run-scoped ownership paired with the SoftMAC operation contract.
pub trait WlanSoftmacLifecycle {
    fn start(&mut self, upcalls: Box<dyn WlanSoftmacUpcalls>) -> Result<(), zx::Status>;
    fn stop(&mut self) -> Result<(), zx::Status>;
}

/// Synchronous, policy-free hardware work driven at the host runtime's
/// deterministic device slot.
pub trait ClientRuntimeDriver {
    fn drive(&mut self) -> Result<bool, zx::Status>;
    fn set_link_up(&mut self, up: bool) -> Result<(), zx::Status>;
    /// Make a completed, unsuccessful connection attempt safe to retry.
    ///
    /// `Ok(())` guarantees that device-side attempt authority was revoked
    /// first, association keys and data admission are cleared, and no callback
    /// from the old attempt can be produced after this method returns. Drivers
    /// that cannot establish all of those properties retain terminal reset
    /// behavior.
    fn finish_failed_connect_attempt(&mut self) -> Result<(), zx::Status> {
        Err(zx::Status::NOT_SUPPORTED)
    }
    fn reset(&mut self) -> Result<(), zx::Status>;
}

/// Client-only calls from the host MLME into a SoftMAC device.
///
/// Mutations validate and admit work during the call, then return an owned
/// completion future that does not borrow the device. Await it only after
/// releasing the driver lock. Completion means the defined effect is complete,
/// not merely queued; scan-start completion is distinct from scan-end upcalls.
///
/// The driver retains all in-flight resources. Dropping a completion future
/// abandons its result without cancelling published work or releasing DMA.
/// Futures need not be Send: the Linux host uses its owning LocalSet.
/// Queries return installed facts; queue_tx reports bounded queue admission.
/// Policy, transport, lifecycle, and callbacks remain outside this contract.
pub trait WlanSoftmac {
    fn query(&mut self) -> Result<WlanSoftmacQueryResponse, zx::Status>;
    fn query_discovery_support(&mut self) -> Result<DiscoverySupport, zx::Status>;
    fn query_mac_sublayer_support(&mut self) -> Result<MacSublayerSupport, zx::Status>;
    fn query_security_support(&mut self) -> Result<SecuritySupport, zx::Status>;
    fn query_spectrum_management_support(
        &mut self,
    ) -> Result<SpectrumManagementSupport, zx::Status>;

    fn set_channel(
        &mut self,
        request: WlanSoftmacBaseSetChannelRequest,
    ) -> impl std::future::Future<Output = Result<(), zx::Status>> + 'static;
    fn join_bss(
        &mut self,
        request: JoinBssRequest,
    ) -> impl std::future::Future<Output = Result<(), zx::Status>> + 'static;
    fn install_key(
        &mut self,
        configuration: WlanKeyConfiguration,
    ) -> impl std::future::Future<Output = Result<(), zx::Status>> + 'static;
    fn notify_association_complete(
        &mut self,
        configuration: WlanAssociationConfig,
    ) -> impl std::future::Future<Output = Result<(), zx::Status>> + 'static;
    fn clear_association(
        &mut self,
        request: WlanSoftmacBaseClearAssociationRequest,
    ) -> impl std::future::Future<Output = Result<(), zx::Status>> + 'static;

    fn start_passive_scan(
        &mut self,
        request: WlanSoftmacBaseStartPassiveScanRequest,
    ) -> impl std::future::Future<
        Output = Result<WlanSoftmacBaseStartPassiveScanResponse, zx::Status>,
    > + 'static;
    fn start_active_scan(
        &mut self,
        request: WlanSoftmacStartActiveScanRequest,
    ) -> impl std::future::Future<
        Output = Result<WlanSoftmacBaseStartActiveScanResponse, zx::Status>,
    > + 'static;
    fn cancel_scan(
        &mut self,
        request: WlanSoftmacBaseCancelScanRequest,
    ) -> impl std::future::Future<Output = Result<(), zx::Status>> + 'static;
    fn update_wmm_parameters(
        &mut self,
        request: WlanSoftmacBaseUpdateWmmParametersRequest,
    ) -> impl std::future::Future<Output = Result<(), zx::Status>> + 'static;
    fn queue_tx(&mut self, bytes: &[u8], flags: WlanTxInfoFlags) -> Result<(), zx::Status>;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn context_deadline_is_absolute_and_revocation_spans_later_work() {
        let now = std::time::Instant::now();
        let deadline = now + std::time::Duration::from_secs(1);
        let (context, revoke) = crate::conformance::operation_context(deadline);
        assert_eq!(context.deadline(), deadline);
        assert_eq!(context.check(now), Ok(()));
        assert_eq!(context.check(deadline), Err(zx::Status::TIMED_OUT));
        // Association lifetime and the original connect budget are distinct.
        let associated = context.for_deadline(deadline + std::time::Duration::from_secs(1));
        assert_eq!(associated.check(deadline), Ok(()));
        revoke();
        assert_eq!(context.check(now), Err(zx::Status::CANCELED));
        assert_eq!(associated.check(deadline), Err(zx::Status::CANCELED));
        assert!(!associated.is_live());
    }

    #[derive(Default)]
    struct Fake {
        calls: Vec<&'static str>,
        tx: Option<(Vec<u8>, WlanTxInfoFlags)>,
        upcalls: Option<Box<dyn WlanSoftmacUpcalls>>,
    }

    impl WlanSoftmacLifecycle for Fake {
        fn start(&mut self, upcalls: Box<dyn WlanSoftmacUpcalls>) -> Result<(), zx::Status> {
            self.calls.push("start");
            self.upcalls = Some(upcalls);
            Ok(())
        }

        fn stop(&mut self) -> Result<(), zx::Status> {
            self.calls.push("stop");
            self.upcalls = None;
            Ok(())
        }
    }

    impl WlanSoftmac for Fake {
        fn query(&mut self) -> Result<WlanSoftmacQueryResponse, zx::Status> {
            self.calls.push("query");
            Ok(Default::default())
        }
        fn query_discovery_support(&mut self) -> Result<DiscoverySupport, zx::Status> {
            self.calls.push("discovery");
            Ok(Default::default())
        }
        fn query_mac_sublayer_support(&mut self) -> Result<MacSublayerSupport, zx::Status> {
            self.calls.push("mac_sublayer");
            Ok(Default::default())
        }
        fn query_security_support(&mut self) -> Result<SecuritySupport, zx::Status> {
            self.calls.push("security");
            Ok(Default::default())
        }
        fn query_spectrum_management_support(
            &mut self,
        ) -> Result<SpectrumManagementSupport, zx::Status> {
            self.calls.push("spectrum_management");
            Ok(Default::default())
        }
        fn set_channel(
            &mut self,
            _: WlanSoftmacBaseSetChannelRequest,
        ) -> impl std::future::Future<Output = Result<(), zx::Status>> + 'static {
            std::future::ready({
                self.calls.push("set_channel");
                Ok(())
            })
        }
        fn join_bss(
            &mut self,
            _: JoinBssRequest,
        ) -> impl std::future::Future<Output = Result<(), zx::Status>> + 'static {
            std::future::ready({
                self.calls.push("join_bss");
                Ok(())
            })
        }
        fn install_key(
            &mut self,
            _: WlanKeyConfiguration,
        ) -> impl std::future::Future<Output = Result<(), zx::Status>> + 'static {
            std::future::ready({
                self.calls.push("install_key");
                Ok(())
            })
        }
        fn notify_association_complete(
            &mut self,
            _: WlanAssociationConfig,
        ) -> impl std::future::Future<Output = Result<(), zx::Status>> + 'static {
            std::future::ready({
                self.calls.push("association_complete");
                Ok(())
            })
        }
        fn clear_association(
            &mut self,
            _: WlanSoftmacBaseClearAssociationRequest,
        ) -> impl std::future::Future<Output = Result<(), zx::Status>> + 'static {
            std::future::ready({
                self.calls.push("clear_association");
                Ok(())
            })
        }
        fn start_passive_scan(
            &mut self,
            _: WlanSoftmacBaseStartPassiveScanRequest,
        ) -> impl std::future::Future<
            Output = Result<WlanSoftmacBaseStartPassiveScanResponse, zx::Status>,
        > + 'static {
            std::future::ready({
                self.calls.push("passive_scan");
                Ok(Default::default())
            })
        }
        fn start_active_scan(
            &mut self,
            _: WlanSoftmacStartActiveScanRequest,
        ) -> impl std::future::Future<
            Output = Result<WlanSoftmacBaseStartActiveScanResponse, zx::Status>,
        > + 'static {
            std::future::ready({
                self.calls.push("active_scan");
                Ok(Default::default())
            })
        }
        fn cancel_scan(
            &mut self,
            _: WlanSoftmacBaseCancelScanRequest,
        ) -> impl std::future::Future<Output = Result<(), zx::Status>> + 'static {
            std::future::ready({
                self.calls.push("cancel_scan");
                Ok(())
            })
        }
        fn update_wmm_parameters(
            &mut self,
            _: WlanSoftmacBaseUpdateWmmParametersRequest,
        ) -> impl std::future::Future<Output = Result<(), zx::Status>> + 'static {
            std::future::ready({
                self.calls.push("update_wmm");
                Ok(())
            })
        }
        fn queue_tx(&mut self, bytes: &[u8], flags: WlanTxInfoFlags) -> Result<(), zx::Status> {
            self.calls.push("queue_tx");
            self.tx = Some((bytes.to_vec(), flags));
            Ok(())
        }
    }

    async fn forward_every_downcall<D: WlanSoftmac>(device: &mut D) -> Result<(), zx::Status> {
        device.query()?;
        device.query_discovery_support()?;
        device.query_mac_sublayer_support()?;
        device.query_security_support()?;
        device.query_spectrum_management_support()?;
        device.set_channel(Default::default()).await?;
        device.join_bss(Default::default()).await?;
        device.install_key(Default::default()).await?;
        device
            .notify_association_complete(Default::default())
            .await?;
        device.clear_association(Default::default()).await?;
        device.start_passive_scan(Default::default()).await?;
        device.start_active_scan(Default::default()).await?;
        device.cancel_scan(Default::default()).await?;
        device.update_wmm_parameters(Default::default()).await?;
        device.queue_tx(&[1, 2, 3], WlanTxInfoFlags::PROTECTED)
    }

    #[test]
    fn fake_compiles_and_forwards_the_complete_downcall_surface() {
        let mut fake = Fake::default();
        futures::executor::block_on(forward_every_downcall(&mut fake)).unwrap();

        assert_eq!(
            fake.calls,
            [
                "query",
                "discovery",
                "mac_sublayer",
                "security",
                "spectrum_management",
                "set_channel",
                "join_bss",
                "install_key",
                "association_complete",
                "clear_association",
                "passive_scan",
                "active_scan",
                "cancel_scan",
                "update_wmm",
                "queue_tx",
            ]
        );
        assert_eq!(fake.tx, Some((vec![1, 2, 3], WlanTxInfoFlags::PROTECTED)));
    }
}
