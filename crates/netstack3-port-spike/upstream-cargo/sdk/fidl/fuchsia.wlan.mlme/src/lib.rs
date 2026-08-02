// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! Host subset generated from the pinned `fuchsia.wlan.mlme` schema.

use fidl_fuchsia_wlan_common::WlanMacRole;
use fidl_fuchsia_wlan_ieee80211::{
    ChannelNumber, HtCapabilities, MacAddr, VhtCapabilities, WlanBand,
};

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BandCapability {
    pub band: WlanBand,
    pub basic_rates: Vec<u8>,
    pub ht_cap: Option<Box<HtCapabilities>>,
    pub vht_cap: Option<Box<VhtCapabilities>>,
    pub primary_channels: Vec<ChannelNumber>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DeviceInfo {
    pub sta_addr: MacAddr,
    pub factory_addr: MacAddr,
    pub role: WlanMacRole,
    pub bands: Vec<BandCapability>,
    pub softmac_hardware_capability: u32,
    pub qos_capable: bool,
}

#[cfg(test)]
mod tests {
    use super::*;
    use fidl_fuchsia_wlan_ieee80211::HT_CAP_LEN;

    #[test]
    fn band_capability_preserves_schema_shapes() {
        let capability = BandCapability {
            band: WlanBand::FiveGhz,
            basic_rates: vec![0x0c, 0x12],
            ht_cap: Some(Box::new(HtCapabilities {
                bytes: [0; HT_CAP_LEN as usize],
            })),
            vht_cap: None,
            primary_channels: vec![ChannelNumber {
                band: WlanBand::FiveGhz,
                number: 36,
            }],
        };
        assert_eq!(capability.ht_cap.unwrap().bytes.len(), HT_CAP_LEN as usize);
        assert_eq!(capability.primary_channels[0].number, 36);
    }

    #[test]
    fn device_info_uses_common_and_ieee80211_schema_types() {
        let info = DeviceInfo {
            sta_addr: [1; 6],
            factory_addr: [2; 6],
            role: WlanMacRole::Client,
            bands: vec![],
            softmac_hardware_capability: 0x1020,
            qos_capable: true,
        };
        assert_eq!(info.role, WlanMacRole::Client);
        assert_eq!(info.factory_addr.len(), 6);
    }
}
