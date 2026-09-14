// Copyright 2018 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! Host value bindings generated from the pinned `fuchsia.wlan.minstrel` schema.

use fidl_fuchsia_wlan_ieee80211::MacAddr;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Peers {
    pub addrs: Vec<MacAddr>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct StatsEntry {
    pub tx_vector_idx: u16,
    pub tx_vec_desc: String,
    pub success_cur: u64,
    pub attempts_cur: u64,
    pub probability: f32,
    pub cur_tp: f32,
    pub success_total: u64,
    pub attempts_total: u64,
    pub probes_total: u64,
    pub probe_cycles_skipped: u8,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Peer {
    pub addr: MacAddr,
    pub max_tp: u16,
    pub max_probability: u16,
    pub basic_highest: u16,
    pub basic_max_probability: u16,
    pub probes: u64,
    pub entries: Vec<StatsEntry>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn peer_list_preserves_mac_addresses() {
        let peers = Peers {
            addrs: vec![[1, 2, 3, 4, 5, 6], [6, 5, 4, 3, 2, 1]],
        };
        assert_eq!(peers.addrs[1], [6, 5, 4, 3, 2, 1]);
    }

    #[test]
    fn peer_preserves_rate_control_statistics() {
        let entry = StatsEntry {
            tx_vector_idx: 129,
            tx_vec_desc: "VHT MCS 1 80MHz".to_string(),
            success_cur: 7,
            attempts_cur: 9,
            probability: 7.0 / 9.0,
            cur_tp: 42.5,
            success_total: 70,
            attempts_total: 90,
            probes_total: 5,
            probe_cycles_skipped: 2,
        };
        let peer = Peer {
            addr: [1, 2, 3, 4, 5, 6],
            max_tp: 129,
            max_probability: 128,
            basic_highest: 3,
            basic_max_probability: 2,
            probes: 11,
            entries: vec![entry],
        };
        assert_eq!(peer.entries[0].tx_vector_idx, peer.max_tp);
        assert_eq!(peer.entries[0].attempts_total, 90);
        assert_eq!(peer.probes, 11);
    }
}
