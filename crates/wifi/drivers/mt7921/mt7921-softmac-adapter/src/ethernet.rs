// SPDX-License-Identifier: GPL-2.0-only

//! Compatibility surface for the chip-independent host Ethernet owner.

pub use wlan_softmac_host::ethernet::*;
pub type Mt7921EthernetDevice = HostEthernetDevice;
pub const MT7921_ETHERNET_MTU: u16 = SOFTMAC_ETHERNET_MTU;

#[cfg(test)]
use netstack3_port_spike::{EthernetDeviceEvent, EthernetEventSource, EthernetFrame};
#[cfg(test)]
use rand::SeedableRng as _;

/// The sole production associated-data owner. Both directions pass through
/// the same pinned `ClientMlme`: TX enters its associated Ethernet handler and
/// RX enters its raw MAC handler through the MT7921 runner. Consequently this
/// type cannot be constructed around `OpenClientMlme` or an independently
/// maintained association state.
#[cfg(test)]
pub struct PinnedAssociatedDataPump<'a, E, T> {
    mlme: &'a mut wlan_mlme::client::ClientMlme<
        crate::client_device::Mt7921ClientDevice<E, crate::Mt7921SoftmacAdapter<T>>,
    >,
    runner: &'a crate::client_device::Mt7921ScanRunner<E, T>,
}

#[cfg(test)]
#[derive(Debug)]
pub enum PinnedDataPumpError {
    Tx(anyhow::Error),
    Rx(zx::Status),
}

#[cfg(test)]
impl std::fmt::Display for PinnedDataPumpError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Tx(error) => write!(formatter, "pinned client Ethernet TX failed: {error}"),
            Self::Rx(status) => write!(formatter, "pinned client MAC RX failed: {status}"),
        }
    }
}

#[cfg(test)]
impl std::error::Error for PinnedDataPumpError {}

#[cfg(test)]
impl<'a, E, T> PinnedAssociatedDataPump<'a, E, T>
where
    E: crate::client_device::Mt7921ClientEffects,
    T: crate::Mt7921PassiveTransport,
{
    pub fn new(
        mlme: &'a mut wlan_mlme::client::ClientMlme<
            crate::client_device::Mt7921ClientDevice<E, crate::Mt7921SoftmacAdapter<T>>,
        >,
        runner: &'a crate::client_device::Mt7921ScanRunner<E, T>,
    ) -> Self {
        Self { mlme, runner }
    }
}

#[cfg(test)]
impl<E, T> AssociatedSoftmacTx for PinnedAssociatedDataPump<'_, E, T>
where
    E: crate::client_device::Mt7921ClientEffects,
    T: crate::Mt7921PassiveTransport,
{
    type Error = PinnedDataPumpError;

    fn transmit_ethernet(&mut self, frame: &[u8]) -> Result<(), Self::Error> {
        wlan_mlme::MlmeImpl::handle_eth_frame_tx(self.mlme, frame, fuchsia_trace::Id::new())
            .map_err(PinnedDataPumpError::Tx)
    }
}

#[cfg(test)]
impl<E, T> AssociatedDataPump for PinnedAssociatedDataPump<'_, E, T>
where
    E: crate::client_device::Mt7921ClientEffects,
    T: crate::Mt7921PassiveTransport,
{
    fn pump_transmit(&mut self) -> Result<bool, EthernetTxPumpError<Self::Error>> {
        // Pop the owned frame under the backend lock, then release it before
        // entering ClientMlme: DeviceOps TX re-enters that same backend.
        let frame = self
            .runner
            .take_ethernet_transmit()
            .map_err(|status| match status {
                zx::Status::CANCELED => EthernetTxPumpError::Closed,
                zx::Status::BAD_STATE => EthernetTxPumpError::LinkDown,
                status => EthernetTxPumpError::Target(PinnedDataPumpError::Rx(status)),
            })?;
        let Some(frame) = frame else { return Ok(false) };
        self.transmit_ethernet(frame.as_bytes())
            .map_err(EthernetTxPumpError::Target)?;
        Ok(true)
    }

    fn pump_receive(&mut self, deadline: std::time::Instant) -> Result<bool, Self::Error> {
        if std::time::Instant::now() >= deadline {
            return Err(PinnedDataPumpError::Rx(zx::Status::TIMED_OUT));
        }
        futures::executor::block_on(self.runner.pump_client_rx(self.mlme))
            .map_err(PinnedDataPumpError::Rx)
    }
}

#[cfg(test)]
#[path = "associated_runtime_test.rs"]
mod associated_runtime_test;
