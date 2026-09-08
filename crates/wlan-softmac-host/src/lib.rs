// SPDX-License-Identifier: MIT OR Apache-2.0

//! Chip-generic owner and synchronous contracts for a client SoftMAC device.
//!
//! The request and response values are the host bindings generated from the
//! project's pinned Fuchsia FIDL schemas. The lifecycle and callback traits
//! are paired by [`runtime::ClientRuntime`], the sole owner of a started
//! device and its MLME/SME state.

#[cfg(any(test, feature = "conformance"))]
pub mod conformance;
pub mod ethernet;
pub mod netstack_child;
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

/// Device-to-host callbacks installed by [`WlanSoftmacLifecycle::start`].
pub trait WlanSoftmacUpcalls: Send {
    fn recv(&mut self, bytes: Vec<u8>, info: WlanRxInfo);
    fn report_tx_result(&mut self, result: WlanTxResult);
    fn notify_scan_complete(&mut self, status: zx::Status, scan_id: u64);
}

/// Run-scoped ownership paired with the synchronous SoftMAC downcalls.
pub trait WlanSoftmacLifecycle {
    fn start(&mut self, upcalls: Box<dyn WlanSoftmacUpcalls>) -> Result<(), zx::Status>;
    fn stop(&mut self) -> Result<(), zx::Status>;
}

/// Synchronous, policy-free hardware work driven at the host runtime's
/// deterministic device slot.
pub trait ClientRuntimeDriver {
    fn drive(&mut self) -> Result<bool, zx::Status>;
    fn set_link_up(&mut self, up: bool) -> Result<(), zx::Status>;
    fn reset(&mut self) -> Result<(), zx::Status>;
}

/// Synchronous client-only calls from the host MLME into a SoftMAC device.
///
/// Implementations complete each operation before returning. Policy,
/// transport, lifecycle, and callbacks remain outside this contract.
pub trait WlanSoftmac {
    fn query(&mut self) -> Result<WlanSoftmacQueryResponse, zx::Status>;
    fn query_discovery_support(&mut self) -> Result<DiscoverySupport, zx::Status>;
    fn query_mac_sublayer_support(&mut self) -> Result<MacSublayerSupport, zx::Status>;
    fn query_security_support(&mut self) -> Result<SecuritySupport, zx::Status>;
    fn query_spectrum_management_support(
        &mut self,
    ) -> Result<SpectrumManagementSupport, zx::Status>;

    fn set_channel(&mut self, request: WlanSoftmacBaseSetChannelRequest) -> Result<(), zx::Status>;
    fn join_bss(&mut self, request: JoinBssRequest) -> Result<(), zx::Status>;
    fn install_key(&mut self, configuration: WlanKeyConfiguration) -> Result<(), zx::Status>;
    fn notify_association_complete(
        &mut self,
        configuration: WlanAssociationConfig,
    ) -> Result<(), zx::Status>;
    fn clear_association(
        &mut self,
        request: WlanSoftmacBaseClearAssociationRequest,
    ) -> Result<(), zx::Status>;

    fn start_passive_scan(
        &mut self,
        request: WlanSoftmacBaseStartPassiveScanRequest,
    ) -> Result<WlanSoftmacBaseStartPassiveScanResponse, zx::Status>;
    fn start_active_scan(
        &mut self,
        request: WlanSoftmacStartActiveScanRequest,
    ) -> Result<WlanSoftmacBaseStartActiveScanResponse, zx::Status>;
    fn cancel_scan(&mut self, request: WlanSoftmacBaseCancelScanRequest) -> Result<(), zx::Status>;
    fn update_wmm_parameters(
        &mut self,
        request: WlanSoftmacBaseUpdateWmmParametersRequest,
    ) -> Result<(), zx::Status>;
    fn queue_tx(&mut self, bytes: &[u8], flags: WlanTxInfoFlags) -> Result<(), zx::Status>;
}

#[cfg(test)]
mod tests {
    use super::*;

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
        fn set_channel(&mut self, _: WlanSoftmacBaseSetChannelRequest) -> Result<(), zx::Status> {
            self.calls.push("set_channel");
            Ok(())
        }
        fn join_bss(&mut self, _: JoinBssRequest) -> Result<(), zx::Status> {
            self.calls.push("join_bss");
            Ok(())
        }
        fn install_key(&mut self, _: WlanKeyConfiguration) -> Result<(), zx::Status> {
            self.calls.push("install_key");
            Ok(())
        }
        fn notify_association_complete(
            &mut self,
            _: WlanAssociationConfig,
        ) -> Result<(), zx::Status> {
            self.calls.push("association_complete");
            Ok(())
        }
        fn clear_association(
            &mut self,
            _: WlanSoftmacBaseClearAssociationRequest,
        ) -> Result<(), zx::Status> {
            self.calls.push("clear_association");
            Ok(())
        }
        fn start_passive_scan(
            &mut self,
            _: WlanSoftmacBaseStartPassiveScanRequest,
        ) -> Result<WlanSoftmacBaseStartPassiveScanResponse, zx::Status> {
            self.calls.push("passive_scan");
            Ok(Default::default())
        }
        fn start_active_scan(
            &mut self,
            _: WlanSoftmacStartActiveScanRequest,
        ) -> Result<WlanSoftmacBaseStartActiveScanResponse, zx::Status> {
            self.calls.push("active_scan");
            Ok(Default::default())
        }
        fn cancel_scan(&mut self, _: WlanSoftmacBaseCancelScanRequest) -> Result<(), zx::Status> {
            self.calls.push("cancel_scan");
            Ok(())
        }
        fn update_wmm_parameters(
            &mut self,
            _: WlanSoftmacBaseUpdateWmmParametersRequest,
        ) -> Result<(), zx::Status> {
            self.calls.push("update_wmm");
            Ok(())
        }
        fn queue_tx(&mut self, bytes: &[u8], flags: WlanTxInfoFlags) -> Result<(), zx::Status> {
            self.calls.push("queue_tx");
            self.tx = Some((bytes.to_vec(), flags));
            Ok(())
        }
    }

    fn forward_every_downcall(device: &mut dyn WlanSoftmac) -> Result<(), zx::Status> {
        device.query()?;
        device.query_discovery_support()?;
        device.query_mac_sublayer_support()?;
        device.query_security_support()?;
        device.query_spectrum_management_support()?;
        device.set_channel(Default::default())?;
        device.join_bss(Default::default())?;
        device.install_key(Default::default())?;
        device.notify_association_complete(Default::default())?;
        device.clear_association(Default::default())?;
        device.start_passive_scan(Default::default())?;
        device.start_active_scan(Default::default())?;
        device.cancel_scan(Default::default())?;
        device.update_wmm_parameters(Default::default())?;
        device.queue_tx(&[1, 2, 3], WlanTxInfoFlags::PROTECTED)
    }

    #[test]
    fn fake_compiles_and_forwards_the_complete_downcall_surface() {
        let mut fake = Fake::default();
        forward_every_downcall(&mut fake).unwrap();

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
