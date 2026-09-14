// SPDX-License-Identifier: GPL-2.0-only

//! Direct protocol/driver boundary. Firmware bootstrap and containment are
//! implemented by the owning driver. Radio operations not ported from the
//! retired lab owner fail explicitly; no compatibility transport is retained.

use crate::{Mt7921Driver, SessionLifecycle};
use wlan_softmac_host::*;

impl WlanSoftmacLifecycle for Mt7921Driver {
    fn start(&mut self, _upcalls: Box<dyn WlanSoftmacUpcalls>) -> Result<(), zx::Status> {
        if self.session.lifecycle != SessionLifecycle::FirmwareInitialized {
            return Err(zx::Status::BAD_STATE);
        }
        self.session.lifecycle = SessionLifecycle::ProtocolStarted;
        Ok(())
    }

    fn stop(&mut self) -> Result<(), zx::Status> {
        self.session
            .contain()
            .map(|_| ())
            .map_err(|_| zx::Status::IO)
    }
}

impl ClientRuntimeDriver for Mt7921Driver {
    fn drive(&mut self) -> Result<bool, zx::Status> {
        if self.session.lifecycle != SessionLifecycle::ProtocolStarted {
            return Err(zx::Status::BAD_STATE);
        }
        // No radio operation is currently admitted and no RX callback can be
        // produced. Firmware events stay private to the hardware owner.
        Ok(false)
    }

    fn set_link_up(&mut self, up: bool) -> Result<(), zx::Status> {
        if up {
            Err(zx::Status::NOT_SUPPORTED)
        } else {
            Ok(())
        }
    }

    fn reset(&mut self) -> Result<(), zx::Status> {
        self.session
            .contain()
            .map(|_| ())
            .map_err(|_| zx::Status::IO)
    }
}

impl WlanSoftmac for Mt7921Driver {
    fn query(&mut self) -> Result<WlanSoftmacQueryResponse, zx::Status> {
        let mac = self
            .firmware
            .nic_capability
            .mac_address
            .ok_or(zx::Status::IO_DATA_INTEGRITY)?;
        Ok(WlanSoftmacQueryResponse {
            sta_addr: Some(mac),
            factory_addr: Some(mac),
            // No channels are advertised until their operations are owned here.
            band_caps: Some(Vec::new()),
            ..Default::default()
        })
    }

    fn query_discovery_support(&mut self) -> Result<DiscoverySupport, zx::Status> {
        Ok(Default::default())
    }
    fn query_mac_sublayer_support(&mut self) -> Result<MacSublayerSupport, zx::Status> {
        Ok(Default::default())
    }
    fn query_security_support(&mut self) -> Result<SecuritySupport, zx::Status> {
        Ok(Default::default())
    }
    fn query_spectrum_management_support(
        &mut self,
    ) -> Result<SpectrumManagementSupport, zx::Status> {
        Ok(Default::default())
    }
    fn set_channel(
        &mut self,
        _: WlanSoftmacBaseSetChannelRequest,
    ) -> impl std::future::Future<Output = Result<(), zx::Status>> + 'static {
        std::future::ready(Err(zx::Status::NOT_SUPPORTED))
    }
    fn join_bss(
        &mut self,
        _: JoinBssRequest,
    ) -> impl std::future::Future<Output = Result<(), zx::Status>> + 'static {
        std::future::ready(Err(zx::Status::NOT_SUPPORTED))
    }
    fn install_key(
        &mut self,
        _: WlanKeyConfiguration,
    ) -> impl std::future::Future<Output = Result<(), zx::Status>> + 'static {
        std::future::ready(Err(zx::Status::NOT_SUPPORTED))
    }
    fn notify_association_complete(
        &mut self,
        _: WlanAssociationConfig,
    ) -> impl std::future::Future<Output = Result<(), zx::Status>> + 'static {
        std::future::ready(Err(zx::Status::NOT_SUPPORTED))
    }
    fn clear_association(
        &mut self,
        _: WlanSoftmacBaseClearAssociationRequest,
    ) -> impl std::future::Future<Output = Result<(), zx::Status>> + 'static {
        std::future::ready({
            // No join/key/TX operation can currently create association state.
            Ok(())
        })
    }
    fn start_passive_scan(
        &mut self,
        _: WlanSoftmacBaseStartPassiveScanRequest,
    ) -> impl std::future::Future<
        Output = Result<WlanSoftmacBaseStartPassiveScanResponse, zx::Status>,
    > + 'static {
        std::future::ready(Err(zx::Status::NOT_SUPPORTED))
    }
    fn start_active_scan(
        &mut self,
        _: WlanSoftmacStartActiveScanRequest,
    ) -> impl std::future::Future<
        Output = Result<WlanSoftmacBaseStartActiveScanResponse, zx::Status>,
    > + 'static {
        std::future::ready(Err(zx::Status::NOT_SUPPORTED))
    }
    fn cancel_scan(
        &mut self,
        _: WlanSoftmacBaseCancelScanRequest,
    ) -> impl std::future::Future<Output = Result<(), zx::Status>> + 'static {
        std::future::ready({
            // Neither scan entrypoint admits work.
            Ok(())
        })
    }
    fn update_wmm_parameters(
        &mut self,
        _: WlanSoftmacBaseUpdateWmmParametersRequest,
    ) -> impl std::future::Future<Output = Result<(), zx::Status>> + 'static {
        std::future::ready(Err(zx::Status::NOT_SUPPORTED))
    }
    fn queue_tx(&mut self, _: &[u8], _: WlanTxInfoFlags) -> Result<(), zx::Status> {
        Err(zx::Status::NOT_SUPPORTED)
    }
}
