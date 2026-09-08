// SPDX-License-Identifier: GPL-2.0-only

//! Opaque production ownership of the MT7921 client runtime.

use crate::Mt7921SoftmacAdapter;
use crate::client_device::{
    Mt7921ClientDevice, Mt7921ClientEffects, PinnedClientRuntime, PinnedConnectError,
};
use crate::ethernet::HostEthernetDevice;
use fidl_fuchsia_wlan_common as fidl_common;
use fidl_fuchsia_wlan_mlme as fidl_mlme;
use fidl_fuchsia_wlan_sme as fidl_sme;
use std::future::Future;
use std::pin::Pin;
use std::time::Instant;

trait RuntimeOwner {
    fn take_ethernet_device(&mut self) -> Option<HostEthernetDevice>;
    fn connect<'a>(
        &'a mut self,
        request: fidl_sme::ConnectRequest,
        deadline: Instant,
    ) -> Pin<Box<dyn Future<Output = Result<(), PinnedConnectError>> + 'a>>;
    fn drive_once(
        &mut self,
    ) -> Pin<Box<dyn Future<Output = Result<bool, PinnedConnectError>> + '_>>;
    fn stop(&mut self) -> Result<(), zx::Status>;
}

impl<E, T> RuntimeOwner for PinnedClientRuntime<E, T>
where
    E: Mt7921ClientEffects,
    T: crate::Mt7921PassiveTransport,
{
    fn take_ethernet_device(&mut self) -> Option<HostEthernetDevice> {
        self.take_ethernet_device()
    }

    fn connect<'a>(
        &'a mut self,
        request: fidl_sme::ConnectRequest,
        deadline: Instant,
    ) -> Pin<Box<dyn Future<Output = Result<(), PinnedConnectError>> + 'a>> {
        Box::pin(self.connect(request, deadline))
    }

    fn drive_once(
        &mut self,
    ) -> Pin<Box<dyn Future<Output = Result<bool, PinnedConnectError>> + '_>> {
        Box::pin(self.pump_associated_once())
    }

    fn stop(&mut self) -> Result<(), zx::Status> {
        self.stop()
    }
}

/// Library-owned production client containing the physical MT7921 effects,
/// SoftMAC device, and pinned MLME/SME/RSN runtime.
///
/// Its concrete internals are deliberately hidden from service composition.
/// Ethernet capabilities become available only after controlled-port UP and
/// each capability may be taken once. `&mut self` serializes connect and drive
/// operations, so a second connect cannot run concurrently.
pub struct Mt7921ProductionClient<'hardware> {
    public_mac: [u8; 6],
    runtime: Box<dyn RuntimeOwner + 'hardware>,
}

impl<'hardware> Mt7921ProductionClient<'hardware> {
    #[allow(clippy::too_many_arguments)]
    pub async fn new<E, T>(
        device: Mt7921ClientDevice<E, Mt7921SoftmacAdapter<T>>,
        device_info: fidl_mlme::DeviceInfo,
        security: fidl_common::SecuritySupport,
        spectrum: fidl_common::SpectrumManagementSupport,
        inspector: fuchsia_inspect::Inspector,
        ethernet_queue_capacity: usize,
    ) -> Result<Self, anyhow::Error>
    where
        E: Mt7921ClientEffects + 'hardware,
        T: crate::Mt7921PassiveTransport + 'hardware,
    {
        let device = device.into_production_owner().map_err(|status| {
            anyhow::anyhow!("MT7921 effects/transport owner is still shared: {status}")
        })?;
        let public_mac = device_info.sta_addr;
        let runtime = PinnedClientRuntime::new_with_ethernet_capacity(
            device,
            {
                let mut config = wlan_sme::client::ClientConfig::default();
                config.wpa3_supported = true;
                config
            },
            device_info,
            security,
            spectrum,
            inspector,
            ethernet_queue_capacity,
        )
        .await?;
        Ok(Self {
            public_mac,
            runtime: Box::new(runtime),
        })
    }

    pub fn public_mac(&self) -> [u8; 6] {
        self.public_mac
    }

    pub fn take_ethernet_device(&mut self) -> Option<HostEthernetDevice> {
        self.runtime.take_ethernet_device()
    }

    /// Drive exactly one caller-selected connection attempt until its terminal
    /// result or `deadline`; this method never backs off or retries internally.
    /// [`PinnedConnectError::Failed`] is returned only after certified cleanup
    /// and is reusable on this same client. Cleanup failure is returned
    /// immediately as a terminal driver or containment error; timeout and all
    /// other driver or containment failures are terminal as well.
    pub async fn connect(
        &mut self,
        request: fidl_sme::ConnectRequest,
        deadline: Instant,
    ) -> Result<(), PinnedConnectError> {
        self.runtime.connect(request, deadline).await
    }

    pub async fn drive_once(&mut self) -> Result<bool, PinnedConnectError> {
        self.runtime.drive_once().await
    }

    pub fn stop(&mut self) -> Result<(), zx::Status> {
        self.runtime.stop()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct FakeOwner {
        calls: Vec<&'static str>,
    }

    impl RuntimeOwner for FakeOwner {
        fn take_ethernet_device(&mut self) -> Option<HostEthernetDevice> {
            self.calls.push("take");
            None
        }
        fn connect<'a>(
            &'a mut self,
            _: fidl_sme::ConnectRequest,
            _: Instant,
        ) -> Pin<Box<dyn Future<Output = Result<(), PinnedConnectError>> + 'a>> {
            self.calls.push("connect");
            Box::pin(async { Ok(()) })
        }
        fn drive_once(
            &mut self,
        ) -> Pin<Box<dyn Future<Output = Result<bool, PinnedConnectError>> + '_>> {
            self.calls.push("drive");
            Box::pin(async { Ok(true) })
        }
        fn stop(&mut self) -> Result<(), zx::Status> {
            self.calls.push("stop");
            Ok(())
        }
    }

    #[test]
    fn opaque_owner_exposes_only_bounded_runtime_operations() {
        let mut client = Mt7921ProductionClient {
            public_mac: [2, 1, 2, 3, 4, 5],
            runtime: Box::new(FakeOwner { calls: vec![] }),
        };
        assert_eq!(client.public_mac(), [2, 1, 2, 3, 4, 5]);
        assert!(client.take_ethernet_device().is_none());
        assert!(futures::executor::block_on(client.drive_once()).unwrap());
        client.stop().unwrap();
    }
}
