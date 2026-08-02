// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! Host subset generated from the pinned `fuchsia.wlan.common` schema.

pub const WLAN_TX_VECTOR_IDX_INVALID: u16 = 0;

macro_rules! flexible_enum {
    ($name:ident, $raw:ty, {$($variant:ident = $value:expr),+ $(,)?}) => {
        #[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
        #[repr(transparent)]
        pub struct $name($raw);

        #[allow(non_upper_case_globals)]
        impl $name {
            $(pub const $variant: Self = Self($value);)+

            pub const fn from_primitive(value: $raw) -> Self {
                Self(value)
            }

            pub const fn into_primitive(self) -> $raw {
                self.0
            }

            pub const fn unknown() -> Self {
                Self(<$raw>::MAX)
            }
        }
    };
}

flexible_enum!(WlanMacRole, u32, {
    Client = 1,
    Ap = 2,
    Mesh = 3,
});

flexible_enum!(DataPlaneType, u8, {
    EthernetDevice = 1,
    GenericNetworkDevice = 2,
});

flexible_enum!(MacImplementationType, u8, {
    Softmac = 1,
    Fullmac = 2,
});

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct RateSelectionOffloadExtension {
    pub supported: Option<bool>,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct DataPlaneExtension {
    pub data_plane_type: Option<DataPlaneType>,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct DeviceExtension {
    pub is_synthetic: Option<bool>,
    pub mac_implementation_type: Option<MacImplementationType>,
    pub tx_status_report_supported: Option<bool>,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct MacSublayerSupport {
    pub rate_selection_offload: Option<RateSelectionOffloadExtension>,
    pub data_plane: Option<DataPlaneExtension>,
    pub device: Option<DeviceExtension>,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct SaeFeature {
    pub driver_handler_supported: Option<bool>,
    pub sme_handler_supported: Option<bool>,
    pub hash_to_element_supported: Option<bool>,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct MfpFeature {
    pub supported: Option<bool>,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct OweFeature {
    pub supported: Option<bool>,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct SecuritySupport {
    pub sae: Option<SaeFeature>,
    pub mfp: Option<MfpFeature>,
    pub owe: Option<OweFeature>,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct DfsFeature {
    pub supported: Option<bool>,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct SpectrumManagementSupport {
    pub dfs: Option<DfsFeature>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn schema_values_match_pinned_fidl() {
        assert_eq!(WLAN_TX_VECTOR_IDX_INVALID, 0);
        assert_eq!(WlanMacRole::Mesh.into_primitive(), 3);
        assert_eq!(DataPlaneType::GenericNetworkDevice.into_primitive(), 2);
        assert_eq!(MacImplementationType::Fullmac.into_primitive(), 2);
    }

    #[test]
    fn flexible_unknown_and_table_defaults_match_binding_contract() {
        assert_eq!(WlanMacRole::from_primitive(44).into_primitive(), 44);
        assert_eq!(DataPlaneType::unknown().into_primitive(), u8::MAX);
        assert_eq!(
            SecuritySupport::default(),
            SecuritySupport {
                sae: None,
                mfp: None,
                owe: None
            }
        );
        assert_eq!(MacSublayerSupport::default().device, None);
    }
}
