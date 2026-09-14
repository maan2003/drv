// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::client::types;
#[cfg(feature = "host-selection")]
use crate::fidl_fuchsia_wlan_policy as fidl_policy;
use crate::telemetry::ScanEventInspectData;
#[cfg(not(feature = "host-selection"))]
use fidl_fuchsia_wlan_policy as fidl_policy;
use itertools::Itertools;
use log::debug;
use std::collections::HashMap;

/// Converts sme::ScanResult to our internal BSS type, then adds it to a map.
/// Only keeps the first unique instance of a BSSID
pub fn bss_to_network_map(
    scan_result_list: Vec<wlan_common::scan::ScanResult>,
    target_ssids: &[types::Ssid],
    scan_event_inspect_data: &mut ScanEventInspectData,
) -> HashMap<types::NetworkIdentifierDetailed, Vec<types::Bss>> {
    let mut bss_by_network: HashMap<types::NetworkIdentifierDetailed, Vec<types::Bss>> =
        HashMap::new();
    for scan_result in scan_result_list.into_iter() {
        let security_type: types::SecurityTypeDetailed =
            scan_result.bss_description.protection().into();
        if security_type == types::SecurityTypeDetailed::Unknown {
            // Log a space-efficient version of the IEs.
            let readable_ie =
                scan_result.bss_description.ies().iter().map(|n| n.to_string()).join(",");
            debug!("Encountered unknown protection, ies: [{:?}]", readable_ie.clone());
            scan_event_inspect_data.unknown_protection_ies.push(readable_ie);
        };
        let entry = bss_by_network
            .entry(types::NetworkIdentifierDetailed {
                ssid: scan_result.bss_description.ssid.clone(),
                security_type,
            })
            .or_default();

        // Check if this BSSID is already in the hashmap
        if !entry.iter().any(|existing_bss| existing_bss.bssid == scan_result.bss_description.bssid)
        {
            entry.push(types::Bss {
                bssid: scan_result.bss_description.bssid,
                signal: types::Signal {
                    rssi_dbm: scan_result.bss_description.rssi_dbm,
                    snr_db: scan_result.bss_description.snr_db,
                },
                channel: scan_result.bss_description.channel,
                timestamp: scan_result.timestamp,
                // TODO(123709): if target_ssids contains the wildcard, this need to be "Unknown"
                observation: if target_ssids.contains(&scan_result.bss_description.ssid) {
                    types::ScanObservation::Active
                } else {
                    types::ScanObservation::Passive
                },
                compatibility: scan_result.compatibility,
                bss_description: wlan_common::sequestered::Sequestered::from(
                    fidl_fuchsia_wlan_ieee80211::BssDescription::from(scan_result.bss_description),
                ),
            });
        };
    }
    bss_by_network
}

pub fn network_map_to_scan_result(
    mut bss_by_network: HashMap<types::NetworkIdentifierDetailed, Vec<types::Bss>>,
) -> Vec<types::ScanResult> {
    let mut scan_results: Vec<types::ScanResult> = bss_by_network
        .drain()
        .map(|(types::NetworkIdentifierDetailed { ssid, security_type }, bss_entries)| {
            let compatibility = if bss_entries.iter().any(|bss| bss.is_compatible()) {
                fidl_policy::Compatibility::Supported
            } else {
                fidl_policy::Compatibility::DisallowedNotSupported
            };
            types::ScanResult {
                ssid,
                security_type_detailed: security_type,
                entries: bss_entries,
                compatibility,
            }
        })
        .collect();

    scan_results.sort_by(|a, b| a.ssid.cmp(&b.ssid));
    scan_results
}
