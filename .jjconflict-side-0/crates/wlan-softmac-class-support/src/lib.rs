// SPDX-License-Identifier: GPL-2.0-only

//! Hardware-neutral support for the project's pinned Fuchsia WLAN SoftMAC
//! boundary. Authentication, association protocol, radio programming,
//! descriptors, and device policy remain with their existing owners.

use fidl_fuchsia_wlan_ieee80211::{ChannelBandwidth, ChannelNumber};
use fidl_fuchsia_wlan_softmac::{WlanRxInfo, WlanSoftmacBaseSetChannelRequest, WlanTxInfoFlags};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ChannelDefinition {
    primary: ChannelNumber,
    bandwidth: ChannelBandwidth,
    secondary80: Option<ChannelNumber>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ChannelDefinitionError {
    MissingPrimary,
    MissingBandwidth,
    InvalidPrimary,
    InvalidSecondary80,
}

impl ChannelDefinition {
    pub fn new(
        primary: ChannelNumber,
        bandwidth: ChannelBandwidth,
        secondary80: Option<ChannelNumber>,
    ) -> Result<Self, ChannelDefinitionError> {
        if primary.number == 0 {
            return Err(ChannelDefinitionError::InvalidPrimary);
        }
        match (bandwidth, secondary80) {
            (ChannelBandwidth::Cbw80P80, Some(secondary))
                if secondary.band == primary.band && secondary.number != 0 => {}
            (ChannelBandwidth::Cbw80P80, _) => {
                return Err(ChannelDefinitionError::InvalidSecondary80);
            }
            (_, None) => {}
            (_, Some(secondary)) if secondary.band == primary.band && secondary.number == 0 => {}
            _ => return Err(ChannelDefinitionError::InvalidSecondary80),
        }
        Ok(Self {
            primary,
            bandwidth,
            secondary80,
        })
    }

    pub fn from_request(
        request: &WlanSoftmacBaseSetChannelRequest,
    ) -> Result<Self, ChannelDefinitionError> {
        Self::new(
            request
                .primary
                .ok_or(ChannelDefinitionError::MissingPrimary)?,
            request
                .bandwidth
                .ok_or(ChannelDefinitionError::MissingBandwidth)?,
            request.vht_secondary_80_channel,
        )
    }

    pub fn request(self) -> WlanSoftmacBaseSetChannelRequest {
        WlanSoftmacBaseSetChannelRequest {
            primary: Some(self.primary),
            bandwidth: Some(self.bandwidth),
            vht_secondary_80_channel: self.secondary80,
        }
    }
    pub fn primary(self) -> ChannelNumber {
        self.primary
    }
    pub fn bandwidth(self) -> ChannelBandwidth {
        self.bandwidth
    }
    pub fn secondary80(self) -> Option<ChannelNumber> {
        self.secondary80
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PublicWlanIdentity([u8; 6]);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum IdentityError {
    Missing,
    Invalid,
    Mismatch,
}

impl PublicWlanIdentity {
    pub fn new(address: [u8; 6]) -> Result<Self, IdentityError> {
        if address == [0; 6] || address[0] & 1 != 0 {
            return Err(IdentityError::Invalid);
        }
        Ok(Self(address))
    }
    pub fn require(expected: Self, actual: Option<[u8; 6]>) -> Result<Self, IdentityError> {
        let actual = Self::new(actual.ok_or(IdentityError::Missing)?)?;
        (actual == expected)
            .then_some(actual)
            .ok_or(IdentityError::Mismatch)
    }
    pub fn bytes(self) -> [u8; 6] {
        self.0
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FrameClass {
    Management,
    Control,
    Data,
    Extension,
}

#[derive(Clone, Copy)]
pub struct RxCarrierMetadata {
    pub class: FrameClass,
    pub preassociation: bool,
    pub status: WlanRxInfo,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct TxCarrierMetadata {
    pub class: FrameClass,
    pub preassociation: bool,
    pub flags: WlanTxInfoFlags,
}

pub fn frame_class(bytes: &[u8]) -> Option<FrameClass> {
    let frame_control = u16::from_le_bytes([*bytes.first()?, *bytes.get(1)?]);
    Some(match (frame_control >> 2) & 0x3 {
        0 => FrameClass::Management,
        1 => FrameClass::Control,
        2 => FrameClass::Data,
        _ => FrameClass::Extension,
    })
}

pub fn rx_carrier(bytes: &[u8], status: WlanRxInfo, associated: bool) -> Option<RxCarrierMetadata> {
    let class = frame_class(bytes)?;
    Some(RxCarrierMetadata {
        class,
        preassociation: !associated && class == FrameClass::Management,
        status,
    })
}

pub fn tx_carrier(
    bytes: &[u8],
    flags: WlanTxInfoFlags,
    associated: bool,
) -> Option<TxCarrierMetadata> {
    let class = frame_class(bytes)?;
    Some(TxCarrierMetadata {
        class,
        preassociation: !associated && class == FrameClass::Management,
        flags,
    })
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct LifecycleAuthorization {
    scan_authorized: bool,
    live: bool,
}

impl LifecycleAuthorization {
    pub fn new(scan_authorized: bool) -> Self {
        Self {
            scan_authorized,
            live: true,
        }
    }
    pub fn authorize_scan(&mut self) {
        if self.live {
            self.scan_authorized = true;
        }
    }
    pub fn invalidate_scan(&mut self) {
        self.scan_authorized = false;
    }
    pub fn invalidate_lifecycle(&mut self) {
        self.live = false;
        self.scan_authorized = false;
    }
    pub fn permits_tx(self) -> bool {
        self.live && self.scan_authorized
    }
    pub fn is_live(self) -> bool {
        self.live
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use fidl_fuchsia_wlan_ieee80211::WlanBand;
    use fidl_fuchsia_wlan_softmac::{WlanRxInfoFlags, WlanRxInfoValid};
    fn channel(band: WlanBand, number: u8) -> ChannelNumber {
        ChannelNumber { band, number }
    }

    #[test]
    fn malformed_channel_shapes_are_rejected() {
        let primary = channel(WlanBand::FiveGhz, 36);
        assert_eq!(
            ChannelDefinition::new(channel(WlanBand::FiveGhz, 0), ChannelBandwidth::Cbw20, None),
            Err(ChannelDefinitionError::InvalidPrimary)
        );
        assert_eq!(
            ChannelDefinition::new(primary, ChannelBandwidth::Cbw80P80, None),
            Err(ChannelDefinitionError::InvalidSecondary80)
        );
        assert_eq!(
            ChannelDefinition::new(
                primary,
                ChannelBandwidth::Cbw80P80,
                Some(channel(WlanBand::TwoGhz, 42))
            ),
            Err(ChannelDefinitionError::InvalidSecondary80)
        );
        assert_eq!(
            ChannelDefinition::new(
                primary,
                ChannelBandwidth::Cbw20,
                Some(channel(WlanBand::FiveGhz, 42))
            ),
            Err(ChannelDefinitionError::InvalidSecondary80)
        );
    }

    #[test]
    fn identity_mismatch_is_rejected() {
        let expected = PublicWlanIdentity::new([2, 1, 2, 3, 4, 5]).unwrap();
        assert_eq!(
            PublicWlanIdentity::require(expected, Some([2, 1, 2, 3, 4, 6])),
            Err(IdentityError::Mismatch)
        );
        assert_eq!(
            PublicWlanIdentity::new([0xff; 6]),
            Err(IdentityError::Invalid)
        );
    }

    #[test]
    fn preassociation_management_carrier_preserves_status() {
        let status = WlanRxInfo {
            rx_flags: WlanRxInfoFlags::empty(),
            valid_fields: WlanRxInfoValid::RSSI,
            phy: fidl_fuchsia_wlan_ieee80211::WlanPhyType::Ofdm,
            data_rate: 0,
            primary: channel(WlanBand::TwoGhz, 1),
            bandwidth: ChannelBandwidth::Cbw20,
            vht_secondary_80_channel: channel(WlanBand::TwoGhz, 0),
            mcs: 0,
            rssi_dbm: -42,
            snr_dbh: 0,
        };
        let carrier = rx_carrier(&[0x80, 0], status, false).unwrap();
        assert_eq!(carrier.class, FrameClass::Management);
        assert!(carrier.preassociation);
        assert_eq!(carrier.status.rssi_dbm, -42);
    }

    #[test]
    fn lifecycle_invalidation_is_fail_closed() {
        let mut lifecycle = LifecycleAuthorization::new(true);
        lifecycle.invalidate_lifecycle();
        lifecycle.authorize_scan();
        assert!(!lifecycle.permits_tx());
        assert!(!lifecycle.is_live());
    }
}
