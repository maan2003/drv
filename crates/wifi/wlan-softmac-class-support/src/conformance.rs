// SPDX-License-Identifier: GPL-2.0-only

//! Reusable deterministic checks for client SoftMAC implementations.

use crate::{ClientRuntimeDriver, WlanSoftmac, WlanSoftmacLifecycle, WlanSoftmacUpcalls};
use fidl_fuchsia_wlan_ieee80211::{ChannelBandwidth, ChannelNumber};
use fidl_fuchsia_wlan_softmac::{
    WlanRxInfo, WlanSoftmacBaseSetChannelRequest, WlanSoftmacBaseStartPassiveScanRequest,
    WlanTxResult,
};
use futures::FutureExt;
use std::sync::{Arc, Mutex};

/// Fresh authority for a conformance test, paired with its revocation action.
pub fn operation_context(deadline: std::time::Instant) -> (crate::OperationContext, impl FnOnce()) {
    let context = crate::OperationContext::new(deadline);
    let revocation = context.clone();
    (context, move || revocation.revoke())
}

const DRIVE_BUDGET: usize = 8;

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ConformanceEvent {
    Started,
    Identity([u8; 6]),
    SupportQueried,
    ChannelSet,
    ScanStarted(u64),
    DriverProgress,
    ScanComplete { status: zx::Status, scan_id: u64 },
    Stopped,
}

struct Recorder(Arc<Mutex<Vec<ConformanceEvent>>>);

impl WlanSoftmacUpcalls for Recorder {
    fn recv(&mut self, _: Vec<u8>, _: WlanRxInfo) {}
    fn report_tx_result(&mut self, _: WlanTxResult) {}
    fn notify_scan_complete(&mut self, status: zx::Status, scan_id: u64) {
        self.0
            .lock()
            .unwrap()
            .push(ConformanceEvent::ScanComplete { status, scan_id });
    }
}

fn drive_operation<D: ClientRuntimeDriver, T>(
    device: &mut D,
    completion: impl std::future::Future<Output = Result<T, zx::Status>>,
) -> Result<T, zx::Status> {
    let mut completion = std::pin::pin!(completion);
    for _ in 0..DRIVE_BUDGET {
        if let Some(result) = completion.as_mut().now_or_never() {
            return result;
        }
        device.drive()?;
    }
    completion
        .as_mut()
        .now_or_never()
        .unwrap_or(Err(zx::Status::TIMED_OUT))
}

/// Exercise one deterministic lifecycle/query/channel/passive-scan run.
///
/// The normalized event stream is intentionally chip-independent. A backend
/// may perform arbitrary private descriptor and firmware work inside each
/// bounded ClientRuntimeDriver::drive call.
pub fn run_client_conformance<D>(
    mut device: D,
    channel: ChannelNumber,
) -> Result<Vec<ConformanceEvent>, zx::Status>
where
    D: WlanSoftmac + WlanSoftmacLifecycle + ClientRuntimeDriver,
{
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(1);
    let events = Arc::new(Mutex::new(Vec::new()));
    device.start(Box::new(Recorder(events.clone())))?;
    events.lock().unwrap().push(ConformanceEvent::Started);

    let identity = device.query()?.sta_addr.ok_or(zx::Status::BAD_STATE)?;
    events
        .lock()
        .unwrap()
        .push(ConformanceEvent::Identity(identity));
    device.query_discovery_support()?;
    device.query_mac_sublayer_support()?;
    device.query_security_support()?;
    device.query_spectrum_management_support()?;
    events
        .lock()
        .unwrap()
        .push(ConformanceEvent::SupportQueried);

    let completion = device.set_channel(
        crate::OperationContext::new(deadline),
        WlanSoftmacBaseSetChannelRequest {
            primary: Some(channel),
            bandwidth: Some(ChannelBandwidth::Cbw20),
            vht_secondary_80_channel: Some(ChannelNumber {
                number: 0,
                ..channel
            }),
        },
    );
    drive_operation(&mut device, completion)?;
    events.lock().unwrap().push(ConformanceEvent::ChannelSet);

    let completion = device.start_passive_scan(
        crate::OperationContext::new(deadline),
        WlanSoftmacBaseStartPassiveScanRequest {
            channels: Some(vec![channel]),
            min_channel_time: Some(10),
            max_channel_time: Some(20),
            min_home_time: Some(0),
        },
    );
    let scan_id = drive_operation(&mut device, completion)?
        .scan_id
        .ok_or(zx::Status::BAD_STATE)?;
    events
        .lock()
        .unwrap()
        .push(ConformanceEvent::ScanStarted(scan_id));

    let mut progressed = false;
    for _ in 0..DRIVE_BUDGET {
        progressed |= device.drive()?;
        if events.lock().unwrap().iter().any(|event| {
            matches!(
                event,
                ConformanceEvent::ScanComplete {
                    status: zx::Status::OK,
                    scan_id: completed
                } if *completed == scan_id
            )
        }) {
            if progressed {
                events
                    .lock()
                    .unwrap()
                    .push(ConformanceEvent::DriverProgress);
            }
            device.stop()?;
            events.lock().unwrap().push(ConformanceEvent::Stopped);
            return Ok(events.lock().unwrap().clone());
        }
    }
    Err(zx::Status::TIMED_OUT)
}

/// Canonical normalized transcript for the deterministic client run.
pub fn expected_client_conformance(identity: [u8; 6], scan_id: u64) -> Vec<ConformanceEvent> {
    vec![
        ConformanceEvent::Started,
        ConformanceEvent::Identity(identity),
        ConformanceEvent::SupportQueried,
        ConformanceEvent::ChannelSet,
        ConformanceEvent::ScanStarted(scan_id),
        ConformanceEvent::ScanComplete {
            status: zx::Status::OK,
            scan_id,
        },
        ConformanceEvent::DriverProgress,
        ConformanceEvent::Stopped,
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        DiscoverySupport, JoinBssRequest, MacSublayerSupport, SecuritySupport,
        SpectrumManagementSupport, WlanAssociationConfig, WlanKeyConfiguration,
        WlanSoftmacBaseCancelScanRequest, WlanSoftmacBaseClearAssociationRequest,
        WlanSoftmacBaseStartActiveScanResponse, WlanSoftmacBaseUpdateWmmParametersRequest,
        WlanSoftmacQueryResponse, WlanSoftmacStartActiveScanRequest, WlanTxInfoFlags,
    };
    use fidl_fuchsia_wlan_ieee80211::WlanBand;

    const CLIENT: [u8; 6] = [2, 0, 0, 0, 0, 1];

    #[derive(Default)]
    struct Fake {
        upcalls: Option<Box<dyn WlanSoftmacUpcalls>>,
        scan_id: Option<u64>,
        drive_stage: u8,
    }

    impl WlanSoftmacLifecycle for Fake {
        fn start(&mut self, upcalls: Box<dyn WlanSoftmacUpcalls>) -> Result<(), zx::Status> {
            self.upcalls = Some(upcalls);
            Ok(())
        }
        fn stop(&mut self) -> Result<(), zx::Status> {
            self.upcalls = None;
            Ok(())
        }
    }

    impl ClientRuntimeDriver for Fake {
        fn drive(&mut self) -> Result<bool, zx::Status> {
            self.drive_stage += 1;
            if self.drive_stage == 2 {
                self.upcalls
                    .as_mut()
                    .unwrap()
                    .notify_scan_complete(zx::Status::OK, self.scan_id.unwrap());
            }
            Ok(self.drive_stage <= 2)
        }
        fn set_link_up(&mut self, _: bool) -> Result<(), zx::Status> {
            Ok(())
        }
        fn reset(&mut self) -> Result<(), zx::Status> {
            Ok(())
        }
    }

    impl WlanSoftmac for Fake {
        fn query(&mut self) -> Result<WlanSoftmacQueryResponse, zx::Status> {
            Ok(WlanSoftmacQueryResponse {
                sta_addr: Some(CLIENT),
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
            _context: crate::OperationContext,
            _: WlanSoftmacBaseSetChannelRequest,
        ) -> impl std::future::Future<Output = Result<(), zx::Status>> + 'static {
            std::future::ready(Ok(()))
        }
        fn join_bss(
            &mut self,
            _context: crate::OperationContext,
            _: JoinBssRequest,
        ) -> impl std::future::Future<Output = Result<(), zx::Status>> + 'static {
            std::future::ready(Ok(()))
        }
        fn install_key(
            &mut self,
            _: WlanKeyConfiguration,
        ) -> impl std::future::Future<Output = Result<(), zx::Status>> + 'static {
            std::future::ready(Ok(()))
        }
        fn notify_association_complete(
            &mut self,
            _: WlanAssociationConfig,
        ) -> impl std::future::Future<Output = Result<(), zx::Status>> + 'static {
            std::future::ready(Ok(()))
        }
        fn clear_association(
            &mut self,
            _: WlanSoftmacBaseClearAssociationRequest,
        ) -> impl std::future::Future<Output = Result<(), zx::Status>> + 'static {
            std::future::ready(Ok(()))
        }
        fn start_passive_scan(
            &mut self,
            _context: crate::OperationContext,
            _: WlanSoftmacBaseStartPassiveScanRequest,
        ) -> impl std::future::Future<
            Output = Result<crate::WlanSoftmacBaseStartPassiveScanResponse, zx::Status>,
        > + 'static {
            std::future::ready({
                self.scan_id = Some(1);
                Ok(crate::WlanSoftmacBaseStartPassiveScanResponse { scan_id: Some(1) })
            })
        }
        fn start_active_scan(
            &mut self,
            _context: crate::OperationContext,
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
            std::future::ready(Ok(()))
        }
        fn update_wmm_parameters(
            &mut self,
            _: WlanSoftmacBaseUpdateWmmParametersRequest,
        ) -> impl std::future::Future<Output = Result<(), zx::Status>> + 'static {
            std::future::ready(Ok(()))
        }
        fn queue_tx(
            &mut self,
            context: crate::OperationContext,
            _: &[u8],
            _: WlanTxInfoFlags,
        ) -> Result<(), zx::Status> {
            context.check(std::time::Instant::now())?;
            Ok(())
        }
    }

    #[test]
    fn fake_device_passes_the_generic_client_contract() {
        let channel = ChannelNumber {
            band: WlanBand::TwoGhz,
            number: 6,
        };
        assert_eq!(
            run_client_conformance(Fake::default(), channel),
            Ok(expected_client_conformance(CLIENT, 1))
        );
    }
}
