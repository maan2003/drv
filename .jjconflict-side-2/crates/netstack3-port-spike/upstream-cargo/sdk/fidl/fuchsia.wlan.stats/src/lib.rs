// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! Host value bindings generated from the pinned `fuchsia.wlan.stats` schema.

use fidl_fuchsia_wlan_ieee80211::{ChannelBandwidth, ChannelNumber};

pub const MAX_NOISE_FLOOR_SAMPLES: u8 = 255;
pub const MAX_RX_RATE_INDEX_SAMPLES: u8 = 196;
pub const MAX_RSSI_SAMPLES: u8 = 255;
pub const MAX_SNR_SAMPLES: u16 = 256;
pub const MAX_DRIVER_SPECIFIC_COUNTERS: u32 = 127;
pub const MAX_DRIVER_SPECIFIC_GAUGES: u32 = 127;
pub const STAT_NAME_MAX_LENGTH: u8 = 127;
pub const GAUGE_STATISTIC_MAX_LENGTH: u8 = 5;
pub const MAX_HISTOGRAMS_PER_TYPE: u8 = 8;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u8)]
pub enum AntennaFreq {
    Antenna2G = 1,
    Antenna5G = 2,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct AntennaId {
    pub freq: AntennaFreq,
    pub index: u8,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u8)]
pub enum HistScope {
    Station = 1,
    PerAntenna = 2,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct HistBucket {
    pub bucket_index: u16,
    pub num_samples: u64,
}

macro_rules! histogram {
    ($name:ident, $samples:ident) => {
        #[derive(Clone, Debug, Eq, PartialEq)]
        pub struct $name {
            pub hist_scope: HistScope,
            pub antenna_id: Option<Box<AntennaId>>,
            pub $samples: Vec<HistBucket>,
            pub invalid_samples: u64,
        }
    };
}

histogram!(NoiseFloorHistogram, noise_floor_samples);
histogram!(RxRateIndexHistogram, rx_rate_index_samples);
histogram!(RssiHistogram, rssi_samples);
histogram!(SnrHistogram, snr_samples);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct UnnamedCounter {
    pub id: u16,
    pub count: u64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct UnnamedGauge {
    pub id: u16,
    pub value: i64,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct IfaceStats {
    pub connection_stats: Option<ConnectionStats>,
    pub driver_specific_counters: Option<Vec<UnnamedCounter>>,
    pub driver_specific_gauges: Option<Vec<UnnamedGauge>>,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct ConnectionStats {
    pub connection_id: Option<u8>,
    pub driver_specific_counters: Option<Vec<UnnamedCounter>>,
    pub driver_specific_gauges: Option<Vec<UnnamedGauge>>,
    pub rx_unicast_total: Option<u64>,
    pub rx_unicast_drop: Option<u64>,
    pub rx_multicast: Option<u64>,
    pub tx_total: Option<u64>,
    pub tx_drop: Option<u64>,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct InspectCounterConfig {
    pub counter_id: Option<u16>,
    pub counter_name: Option<String>,
}

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
#[repr(transparent)]
pub struct GaugeStatistic(u8);

#[allow(non_upper_case_globals)]
impl GaugeStatistic {
    pub const Min: Self = Self(1);
    pub const Max: Self = Self(2);
    pub const Sum: Self = Self(3);
    pub const Last: Self = Self(4);
    pub const Mean: Self = Self(5);

    pub const fn from_primitive(value: u8) -> Option<Self> {
        if value >= Self::Min.0 && value <= Self::Mean.0 {
            Some(Self(value))
        } else {
            None
        }
    }

    pub const fn from_primitive_allow_unknown(value: u8) -> Self {
        Self(value)
    }

    pub const fn into_primitive(self) -> u8 {
        self.0
    }
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct InspectGaugeConfig {
    pub gauge_id: Option<u16>,
    pub gauge_name: Option<String>,
    pub statistics: Option<Vec<GaugeStatistic>>,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct TelemetrySupport {
    pub inspect_counter_configs: Option<Vec<InspectCounterConfig>>,
    pub inspect_gauge_configs: Option<Vec<InspectGaugeConfig>>,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct IfaceHistogramStats {
    pub noise_floor_histograms: Option<Vec<NoiseFloorHistogram>>,
    pub rssi_histograms: Option<Vec<RssiHistogram>>,
    pub rx_rate_index_histograms: Option<Vec<RxRateIndexHistogram>>,
    pub snr_histograms: Option<Vec<SnrHistogram>>,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct SignalReport {
    pub connection_signal_report: Option<ConnectionSignalReport>,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct ConnectionSignalReport {
    pub primary: Option<ChannelNumber>,
    pub tx_rate_500kbps: Option<u32>,
    pub rssi_dbm: Option<i8>,
    pub snr_db: Option<i8>,
    pub bandwidth: Option<ChannelBandwidth>,
    pub vht_secondary_80_channel: Option<ChannelNumber>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn schema_discriminants_and_bounds_match() {
        assert_eq!(AntennaFreq::Antenna2G as u8, 1);
        assert_eq!(AntennaFreq::Antenna5G as u8, 2);
        assert_eq!(HistScope::PerAntenna as u8, 2);
        assert_eq!(GaugeStatistic::Mean.into_primitive(), 5);
        assert_eq!(MAX_SNR_SAMPLES, 256);
        assert_eq!(MAX_HISTOGRAMS_PER_TYPE, 8);
    }

    #[test]
    fn flexible_gauge_statistics_retain_unknown_values() {
        assert_eq!(GaugeStatistic::from_primitive(3), Some(GaugeStatistic::Sum));
        assert_eq!(GaugeStatistic::from_primitive(9), None);
        assert_eq!(
            GaugeStatistic::from_primitive_allow_unknown(9).into_primitive(),
            9
        );
    }

    #[test]
    fn tables_default_to_absent_fields() {
        assert_eq!(SignalReport::default().connection_signal_report, None);
        assert_eq!(TelemetrySupport::default().inspect_counter_configs, None);
        assert_eq!(IfaceStats::default().connection_stats, None);
    }

    #[test]
    fn histogram_preserves_nullable_box_and_sparse_samples() {
        let histogram = RssiHistogram {
            hist_scope: HistScope::PerAntenna,
            antenna_id: Some(Box::new(AntennaId {
                freq: AntennaFreq::Antenna5G,
                index: 1,
            })),
            rssi_samples: vec![HistBucket {
                bucket_index: 225,
                num_samples: 50,
            }],
            invalid_samples: 2,
        };
        assert_eq!(histogram.antenna_id.unwrap().index, 1);
        assert_eq!(histogram.rssi_samples[0].num_samples, 50);
    }
}
