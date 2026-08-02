// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! Host subset generated from the pinned `fuchsia.wlan.softmac` schema.

use fidl_fuchsia_wlan_ieee80211::{ChannelBandwidth, WlanPhyType};

bitflags::bitflags! {
    #[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
    pub struct WlanTxInfoFlags: u32 {
        const PROTECTED = 0x1;
        const FAVOR_RELIABILITY = 0x2;
        const QOS = 0x4;
    }

    #[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
    pub struct WlanTxInfoValid: u32 {
        const DATA_RATE = 0x1;
        const TX_VECTOR_IDX = 0x2;
        const PHY = 0x4;
        const CHANNEL_BANDWIDTH = 0x8;
        const MCS = 0x10;
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct WlanTxInfo {
    pub tx_flags: u32,
    pub valid_fields: u32,
    pub tx_vector_idx: u16,
    pub phy: WlanPhyType,
    pub bandwidth: ChannelBandwidth,
    pub mcs: u8,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn schema_bits_match_pinned_fidl() {
        assert_eq!(WlanTxInfoFlags::all().bits(), 0x7);
        assert_eq!(WlanTxInfoValid::all().bits(), 0x1f);
        assert_eq!(
            (WlanTxInfoValid::TX_VECTOR_IDX
                | WlanTxInfoValid::PHY
                | WlanTxInfoValid::CHANNEL_BANDWIDTH
                | WlanTxInfoValid::MCS)
                .bits(),
            0x1e
        );
    }

    #[test]
    fn flexible_bits_retain_unknown_values() {
        assert_eq!(WlanTxInfoFlags::from_bits_retain(0x80).bits(), 0x80);
    }

    #[test]
    fn tx_info_uses_ieee80211_schema_types() {
        let info = WlanTxInfo {
            tx_flags: WlanTxInfoFlags::FAVOR_RELIABILITY.bits(),
            valid_fields: WlanTxInfoValid::PHY.bits(),
            tx_vector_idx: 7,
            phy: WlanPhyType::Ht,
            bandwidth: ChannelBandwidth::Cbw40,
            mcs: 3,
        };
        assert_eq!(info.phy, WlanPhyType::Ht);
        assert_eq!(info.bandwidth, ChannelBandwidth::Cbw40);
    }
}
