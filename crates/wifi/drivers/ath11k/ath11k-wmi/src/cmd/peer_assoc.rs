//! Peer association command encoding.

#[cfg(feature = "proptest")]
use super::CommandStrategy;
use super::{EncodeCommand, TlvWriter, trace_branch, trace_field};
use crate::tags::{
    WMI_PEER_ASSOC_CMDID, WMI_TAG_ARRAY_STRUCT, WMI_TAG_HE_RATE_SET,
    WMI_TAG_PEER_ASSOC_COMPLETE_CMD, WMI_TAG_VHT_RATE_SET,
};
use crate::trace::TraceSink;
use crate::{Command, WmiError};
use alloc::vec::Vec;

const WMI_PEER_AUTH: u32 = 0x0000_0001;
const WMI_PEER_QOS: u32 = 0x0000_0002;
const WMI_PEER_NEED_PTK_4_WAY: u32 = 0x0000_0004;
const WMI_PEER_NEED_GTK_2_WAY: u32 = 0x0000_0010;
const WMI_PEER_HE: u32 = 0x0000_0400;
const WMI_PEER_APSD: u32 = 0x0000_0800;
const WMI_PEER_HT: u32 = 0x0000_1000;
const WMI_PEER_40MHZ: u32 = 0x0000_2000;
const WMI_PEER_STBC: u32 = 0x0000_8000;
const WMI_PEER_LDPC: u32 = 0x0001_0000;
const WMI_PEER_DYN_MIMOPS: u32 = 0x0002_0000;
const WMI_PEER_STATIC_MIMOPS: u32 = 0x0004_0000;
const WMI_PEER_SPATIAL_MUX: u32 = 0x0020_0000;
const WMI_PEER_TWT_REQ: u32 = 0x0040_0000;
const WMI_PEER_TWT_RESP: u32 = 0x0080_0000;
const WMI_PEER_VHT: u32 = 0x0200_0000;
const WMI_PEER_80MHZ: u32 = 0x0400_0000;
const WMI_PEER_PMF: u32 = 0x0800_0000;
const WMI_PEER_160MHZ: u32 = 0x4000_0000;

/// Host representation of `struct ath11k_ppe_threshold`.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct PeerPpeThreshold {
    pub numss_m1: u32,
    pub ru_bit_mask: u32,
    pub ppet16_ppet8_ru3_ru0: [u32; 8],
}

/// One element of the peer HE rate-set arrays.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct PeerHeRateSet {
    pub rx_mcs_set: u32,
    pub tx_mcs_set: u32,
}

/// Rust counterpart of the fields consumed from `struct peer_assoc_params`.
///
/// Count-and-array C members are represented as vectors, so safe Rust cannot
/// express the out-of-bounds count values accepted by the unchecked C caller.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct PeerAssocParams {
    pub vdev_id: u32,
    pub peer_new_assoc: u32,
    pub peer_associd: u32,
    pub peer_mac: [u8; 6],
    pub peer_rate_caps: u32,
    pub peer_caps: u32,
    pub peer_listen_intval: u32,
    pub peer_ht_caps: u32,
    pub peer_max_mpdu: u32,
    pub peer_mpdu_density: u32,
    pub peer_vht_caps: u32,
    pub peer_phymode: u32,
    pub peer_nss: u32,
    pub peer_bw_rxnss_override: u32,
    pub peer_legacy_rates: Vec<u8>,
    pub peer_ht_rates: Vec<u8>,
    pub vht_capable: bool,
    pub rx_max_rate: u32,
    pub rx_mcs_set: u32,
    pub tx_max_rate: u32,
    pub tx_mcs_set: u32,
    pub peer_he_mcs: Vec<PeerHeRateSet>,
    pub min_data_rate: u8,
    pub peer_he_cap_macinfo: [u32; 2],
    pub peer_he_cap_macinfo_internal: u32,
    pub peer_he_caps_6ghz: u32,
    pub peer_he_ops: u32,
    pub peer_he_cap_phyinfo: [u32; 3],
    pub peer_ppet: PeerPpeThreshold,
    pub is_pmf_enabled: bool,
    pub is_wme_set: bool,
    pub qos_flag: bool,
    pub apsd_flag: bool,
    pub ht_flag: bool,
    pub bw_40: bool,
    pub bw_80: bool,
    pub bw_160: bool,
    pub stbc_flag: bool,
    pub ldpc_flag: bool,
    pub static_mimops_flag: bool,
    pub dynamic_mimops_flag: bool,
    pub spatial_mux_flag: bool,
    pub vht_flag: bool,
    pub he_flag: bool,
    pub twt_requester: bool,
    pub twt_responder: bool,
    pub auth_flag: bool,
    pub need_ptk_4_way: bool,
    pub need_gtk_2_way: bool,
    pub safe_mode_enabled: bool,
    pub is_assoc: bool,
}

/// The two sources used by `ath11k_wmi_send_peer_assoc_cmd`.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct PeerAssoc {
    pub params: PeerAssocParams,
    pub hw_crypto_disabled: bool,
}

/// Port of `ath11k_wmi_copy_peer_flags`.
pub fn copy_peer_flags(param: &PeerAssocParams, hw_crypto_disabled: bool) -> u32 {
    let mut flags = 0;

    if param.is_wme_set {
        for (set, flag) in [
            (param.qos_flag, WMI_PEER_QOS),
            (param.apsd_flag, WMI_PEER_APSD),
            (param.ht_flag, WMI_PEER_HT),
            (param.bw_40, WMI_PEER_40MHZ),
            (param.bw_80, WMI_PEER_80MHZ),
            (param.bw_160, WMI_PEER_160MHZ),
            (param.stbc_flag, WMI_PEER_STBC),
            (param.ldpc_flag, WMI_PEER_LDPC),
            (param.static_mimops_flag, WMI_PEER_STATIC_MIMOPS),
            (param.dynamic_mimops_flag, WMI_PEER_DYN_MIMOPS),
            (param.spatial_mux_flag, WMI_PEER_SPATIAL_MUX),
            (param.vht_flag, WMI_PEER_VHT),
            (param.he_flag, WMI_PEER_HE),
            (param.twt_requester, WMI_PEER_TWT_REQ),
            (param.twt_responder, WMI_PEER_TWT_RESP),
        ] {
            if set {
                flags |= flag;
            }
        }
    }

    if param.auth_flag {
        flags |= WMI_PEER_AUTH;
    }
    if param.need_ptk_4_way {
        flags |= WMI_PEER_NEED_PTK_4_WAY;
        if !hw_crypto_disabled && param.is_assoc {
            flags &= !WMI_PEER_AUTH;
        }
    }
    if param.need_gtk_2_way {
        flags |= WMI_PEER_NEED_GTK_2_WAY;
    }
    if param.safe_mode_enabled {
        flags &= !(WMI_PEER_NEED_PTK_4_WAY | WMI_PEER_NEED_GTK_2_WAY);
    }
    if param.is_pmf_enabled {
        flags |= WMI_PEER_PMF;
    }
    if param.peer_ht_rates.is_empty() {
        flags &= !WMI_PEER_HT;
    }

    flags
}

impl EncodeCommand for PeerAssoc {
    fn trace_fields(&self, sink: &mut dyn TraceSink) {
        let p = &self.params;
        trace_field(sink, "PeerAssoc.params.vdev_id", p.vdev_id);
        trace_field(sink, "PeerAssoc.params.peer_new_assoc", p.peer_new_assoc);
        trace_field(sink, "PeerAssoc.params.peer_associd", p.peer_associd);
        for value in p.peer_mac {
            trace_field(sink, "PeerAssoc.params.peer_mac[]", value);
        }
        trace_field(sink, "PeerAssoc.params.peer_rate_caps", p.peer_rate_caps);
        trace_field(sink, "PeerAssoc.params.peer_caps", p.peer_caps);
        trace_field(
            sink,
            "PeerAssoc.params.peer_listen_intval",
            p.peer_listen_intval,
        );
        trace_field(sink, "PeerAssoc.params.peer_ht_caps", p.peer_ht_caps);
        trace_field(sink, "PeerAssoc.params.peer_max_mpdu", p.peer_max_mpdu);
        trace_field(
            sink,
            "PeerAssoc.params.peer_mpdu_density",
            p.peer_mpdu_density,
        );
        trace_field(sink, "PeerAssoc.params.peer_vht_caps", p.peer_vht_caps);
        trace_field(sink, "PeerAssoc.params.peer_phymode", p.peer_phymode);
        trace_field(sink, "PeerAssoc.params.peer_nss", p.peer_nss);
        trace_field(
            sink,
            "PeerAssoc.params.peer_bw_rxnss_override",
            p.peer_bw_rxnss_override,
        );
        trace_field(
            sink,
            "PeerAssoc.params.peer_legacy_rates.len",
            p.peer_legacy_rates.len() as u64,
        );
        for &value in &p.peer_legacy_rates {
            trace_field(sink, "PeerAssoc.params.peer_legacy_rates[]", value);
        }
        trace_field(
            sink,
            "PeerAssoc.params.peer_ht_rates.len",
            p.peer_ht_rates.len() as u64,
        );
        for &value in &p.peer_ht_rates {
            trace_field(sink, "PeerAssoc.params.peer_ht_rates[]", value);
        }
        trace_branch(sink, "PeerAssoc.params.vht_capable", p.vht_capable);
        trace_field(sink, "PeerAssoc.params.rx_max_rate", p.rx_max_rate);
        trace_field(sink, "PeerAssoc.params.rx_mcs_set", p.rx_mcs_set);
        trace_field(sink, "PeerAssoc.params.tx_max_rate", p.tx_max_rate);
        trace_field(sink, "PeerAssoc.params.tx_mcs_set", p.tx_mcs_set);
        trace_field(
            sink,
            "PeerAssoc.params.peer_he_mcs.len",
            p.peer_he_mcs.len() as u64,
        );
        for rate in &p.peer_he_mcs {
            trace_field(
                sink,
                "PeerAssoc.params.peer_he_mcs[].rx_mcs_set",
                rate.rx_mcs_set,
            );
            trace_field(
                sink,
                "PeerAssoc.params.peer_he_mcs[].tx_mcs_set",
                rate.tx_mcs_set,
            );
        }
        trace_field(sink, "PeerAssoc.params.min_data_rate", p.min_data_rate);
        for value in p.peer_he_cap_macinfo {
            trace_field(sink, "PeerAssoc.params.peer_he_cap_macinfo[]", value);
        }
        trace_field(
            sink,
            "PeerAssoc.params.peer_he_cap_macinfo_internal",
            p.peer_he_cap_macinfo_internal,
        );
        trace_field(
            sink,
            "PeerAssoc.params.peer_he_caps_6ghz",
            p.peer_he_caps_6ghz,
        );
        trace_field(sink, "PeerAssoc.params.peer_he_ops", p.peer_he_ops);
        for value in p.peer_he_cap_phyinfo {
            trace_field(sink, "PeerAssoc.params.peer_he_cap_phyinfo[]", value);
        }
        trace_field(
            sink,
            "PeerAssoc.params.peer_ppet.numss_m1",
            p.peer_ppet.numss_m1,
        );
        trace_field(
            sink,
            "PeerAssoc.params.peer_ppet.ru_bit_mask",
            p.peer_ppet.ru_bit_mask,
        );
        for value in p.peer_ppet.ppet16_ppet8_ru3_ru0 {
            trace_field(
                sink,
                "PeerAssoc.params.peer_ppet.ppet16_ppet8_ru3_ru0[]",
                value,
            );
        }
        for (name, taken) in [
            ("PeerAssoc.params.is_pmf_enabled", p.is_pmf_enabled),
            ("PeerAssoc.params.is_wme_set", p.is_wme_set),
            ("PeerAssoc.params.qos_flag", p.qos_flag),
            ("PeerAssoc.params.apsd_flag", p.apsd_flag),
            ("PeerAssoc.params.ht_flag", p.ht_flag),
            ("PeerAssoc.params.bw_40", p.bw_40),
            ("PeerAssoc.params.bw_80", p.bw_80),
            ("PeerAssoc.params.bw_160", p.bw_160),
            ("PeerAssoc.params.stbc_flag", p.stbc_flag),
            ("PeerAssoc.params.ldpc_flag", p.ldpc_flag),
            ("PeerAssoc.params.static_mimops_flag", p.static_mimops_flag),
            (
                "PeerAssoc.params.dynamic_mimops_flag",
                p.dynamic_mimops_flag,
            ),
            ("PeerAssoc.params.spatial_mux_flag", p.spatial_mux_flag),
            ("PeerAssoc.params.vht_flag", p.vht_flag),
            ("PeerAssoc.params.he_flag", p.he_flag),
            ("PeerAssoc.params.twt_requester", p.twt_requester),
            ("PeerAssoc.params.twt_responder", p.twt_responder),
            ("PeerAssoc.params.auth_flag", p.auth_flag),
            ("PeerAssoc.params.need_ptk_4_way", p.need_ptk_4_way),
            ("PeerAssoc.params.need_gtk_2_way", p.need_gtk_2_way),
            ("PeerAssoc.params.safe_mode_enabled", p.safe_mode_enabled),
            ("PeerAssoc.params.is_assoc", p.is_assoc),
            ("PeerAssoc.hw_crypto_disabled", self.hw_crypto_disabled),
        ] {
            trace_branch(sink, name, taken);
        }
    }

    fn encode_command(&self) -> Result<Command, WmiError> {
        let p = &self.params;
        if p.peer_legacy_rates.len() > 128 || p.peer_ht_rates.len() > 128 || p.peer_he_mcs.len() > 3
        {
            return Err(WmiError::Malformed);
        }
        let mut w = TlvWriter::default();
        w.tlv(WMI_TAG_PEER_ASSOC_COMPLETE_CMD, |w| {
            w.mac(&p.peer_mac);
            for value in [
                p.vdev_id,
                p.peer_new_assoc,
                p.peer_associd,
                copy_peer_flags(p, self.hw_crypto_disabled),
                p.peer_caps,
                p.peer_listen_intval,
                p.peer_ht_caps,
                p.peer_max_mpdu,
                p.peer_mpdu_density,
                p.peer_rate_caps,
                p.peer_nss,
                p.peer_vht_caps,
                p.peer_phymode,
                0,
                0,
                p.peer_legacy_rates.len() as u32,
                p.peer_ht_rates.len() as u32,
                p.peer_bw_rxnss_override,
                p.peer_ppet.numss_m1,
                p.peer_ppet.ru_bit_mask,
            ] {
                w.u32(value);
            }
            for value in p.peer_ppet.ppet16_ppet8_ru3_ru0 {
                w.u32(value);
            }
            w.u32(p.peer_he_cap_macinfo[0]);
            w.u32(p.peer_he_ops);
            for value in p.peer_he_cap_phyinfo {
                w.u32(value);
            }
            w.u32(p.peer_he_mcs.len() as u32);
            w.u32(p.peer_he_cap_macinfo[1]);
            w.u32(p.peer_he_cap_macinfo_internal);
            w.u32(u32::from(p.min_data_rate));
            w.u32(p.peer_he_caps_6ghz);
        })?;
        w.byte_array(&p.peer_legacy_rates)?;
        w.byte_array(&p.peer_ht_rates)?;
        w.tlv(WMI_TAG_VHT_RATE_SET, |w| {
            if p.vht_capable {
                // These directions intentionally match the firmware-facing C
                // assignment, including its RX/TX naming reversal.
                w.u32(p.tx_max_rate);
                w.u32(p.tx_mcs_set);
                w.u32(p.rx_max_rate);
                w.u32(p.rx_mcs_set);
            } else {
                w.zeros(16);
            }
            w.u32(0);
        })?;
        w.tlv(WMI_TAG_ARRAY_STRUCT, |w| {
            for rate in &p.peer_he_mcs {
                // A HE rate-set TLV is fixed-size, so this cannot fail.
                let _ = w.tlv(WMI_TAG_HE_RATE_SET, |w| {
                    w.u32(rate.rx_mcs_set);
                    w.u32(rate.tx_mcs_set);
                });
            }
        })?;
        w.finish(WMI_PEER_ASSOC_CMDID)
    }
}

#[cfg(feature = "proptest")]
impl CommandStrategy for PeerAssoc {
    fn strategy() -> proptest::strategy::BoxedStrategy<Self> {
        use proptest::prelude::{Strategy, any};

        let ppe = (any::<u32>(), any::<u32>(), any::<[u32; 8]>()).prop_map(
            |(numss_m1, ru_bit_mask, ppet16_ppet8_ru3_ru0)| PeerPpeThreshold {
                numss_m1,
                ru_bit_mask,
                ppet16_ppet8_ru3_ru0,
            },
        );
        let he_rates = proptest::collection::vec(
            (any::<u32>(), any::<u32>()).prop_map(|(rx_mcs_set, tx_mcs_set)| PeerHeRateSet {
                rx_mcs_set,
                tx_mcs_set,
            }),
            0..=3,
        );

        (
            (
                any::<u32>(),
                any::<u32>(),
                any::<u32>(),
                any::<[u8; 6]>(),
                any::<u32>(),
                any::<u32>(),
                any::<u32>(),
            ),
            (
                any::<u32>(),
                any::<u32>(),
                any::<u32>(),
                any::<u32>(),
                any::<u32>(),
                any::<u32>(),
                any::<u32>(),
            ),
            (
                proptest::collection::vec(any::<u8>(), 0..=128),
                proptest::collection::vec(any::<u8>(), 0..=128),
                any::<bool>(),
                any::<u32>(),
                any::<u32>(),
                any::<u32>(),
                any::<u32>(),
                he_rates,
            ),
            (
                any::<u8>(),
                any::<[u32; 2]>(),
                any::<u32>(),
                any::<u32>(),
                any::<u32>(),
                any::<[u32; 3]>(),
                ppe,
            ),
            (
                any::<bool>(),
                any::<bool>(),
                any::<bool>(),
                any::<bool>(),
                any::<bool>(),
                any::<bool>(),
                any::<bool>(),
                any::<bool>(),
                any::<bool>(),
                any::<bool>(),
                any::<bool>(),
            ),
            (
                any::<bool>(),
                any::<bool>(),
                any::<bool>(),
                any::<bool>(),
                any::<bool>(),
                any::<bool>(),
                any::<bool>(),
                any::<bool>(),
                any::<bool>(),
                any::<bool>(),
                any::<bool>(),
            ),
            any::<bool>(),
        )
            .prop_map(
                |(
                    (
                        vdev_id,
                        peer_new_assoc,
                        peer_associd,
                        peer_mac,
                        peer_rate_caps,
                        peer_caps,
                        peer_listen_intval,
                    ),
                    (
                        peer_ht_caps,
                        peer_max_mpdu,
                        peer_mpdu_density,
                        peer_vht_caps,
                        peer_phymode,
                        peer_nss,
                        peer_bw_rxnss_override,
                    ),
                    (
                        peer_legacy_rates,
                        peer_ht_rates,
                        vht_capable,
                        rx_max_rate,
                        rx_mcs_set,
                        tx_max_rate,
                        tx_mcs_set,
                        peer_he_mcs,
                    ),
                    (
                        min_data_rate,
                        peer_he_cap_macinfo,
                        peer_he_cap_macinfo_internal,
                        peer_he_caps_6ghz,
                        peer_he_ops,
                        peer_he_cap_phyinfo,
                        peer_ppet,
                    ),
                    (
                        is_pmf_enabled,
                        is_wme_set,
                        qos_flag,
                        apsd_flag,
                        ht_flag,
                        bw_40,
                        bw_80,
                        bw_160,
                        stbc_flag,
                        ldpc_flag,
                        static_mimops_flag,
                    ),
                    (
                        dynamic_mimops_flag,
                        spatial_mux_flag,
                        vht_flag,
                        he_flag,
                        twt_requester,
                        twt_responder,
                        auth_flag,
                        need_ptk_4_way,
                        need_gtk_2_way,
                        safe_mode_enabled,
                        is_assoc,
                    ),
                    hw_crypto_disabled,
                )| PeerAssoc {
                    params: PeerAssocParams {
                        vdev_id,
                        peer_new_assoc,
                        peer_associd,
                        peer_mac,
                        peer_rate_caps,
                        peer_caps,
                        peer_listen_intval,
                        peer_ht_caps,
                        peer_max_mpdu,
                        peer_mpdu_density,
                        peer_vht_caps,
                        peer_phymode,
                        peer_nss,
                        peer_bw_rxnss_override,
                        peer_legacy_rates,
                        peer_ht_rates,
                        vht_capable,
                        rx_max_rate,
                        rx_mcs_set,
                        tx_max_rate,
                        tx_mcs_set,
                        peer_he_mcs,
                        min_data_rate,
                        peer_he_cap_macinfo,
                        peer_he_cap_macinfo_internal,
                        peer_he_caps_6ghz,
                        peer_he_ops,
                        peer_he_cap_phyinfo,
                        peer_ppet,
                        is_pmf_enabled,
                        is_wme_set,
                        qos_flag,
                        apsd_flag,
                        ht_flag,
                        bw_40,
                        bw_80,
                        bw_160,
                        stbc_flag,
                        ldpc_flag,
                        static_mimops_flag,
                        dynamic_mimops_flag,
                        spatial_mux_flag,
                        vht_flag,
                        he_flag,
                        twt_requester,
                        twt_responder,
                        auth_flag,
                        need_ptk_4_way,
                        need_gtk_2_way,
                        safe_mode_enabled,
                        is_assoc,
                    },
                    hw_crypto_disabled,
                },
            )
            .boxed()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::trace::{TraceEvent, TraceSink};
    use alloc::vec;

    #[derive(Default)]
    struct Trace(Vec<TraceEvent>);

    impl TraceSink for Trace {
        fn record(&mut self, event: TraceEvent) {
            self.0.push(event);
        }
    }

    #[test]
    fn peer_flags_match_c_ordering_and_gates() {
        let p = PeerAssocParams {
            is_wme_set: true,
            qos_flag: true,
            apsd_flag: true,
            ht_flag: true,
            bw_40: true,
            bw_80: true,
            bw_160: true,
            stbc_flag: true,
            ldpc_flag: true,
            static_mimops_flag: true,
            dynamic_mimops_flag: true,
            spatial_mux_flag: true,
            vht_flag: true,
            he_flag: true,
            twt_requester: true,
            twt_responder: true,
            auth_flag: true,
            need_ptk_4_way: true,
            need_gtk_2_way: true,
            safe_mode_enabled: true,
            is_pmf_enabled: true,
            is_assoc: true,
            peer_ht_rates: vec![1],
            ..Default::default()
        };
        assert_eq!(
            copy_peer_flags(&p, false),
            WMI_PEER_QOS
                | WMI_PEER_APSD
                | WMI_PEER_HT
                | WMI_PEER_40MHZ
                | WMI_PEER_80MHZ
                | WMI_PEER_160MHZ
                | WMI_PEER_STBC
                | WMI_PEER_LDPC
                | WMI_PEER_STATIC_MIMOPS
                | WMI_PEER_DYN_MIMOPS
                | WMI_PEER_SPATIAL_MUX
                | WMI_PEER_VHT
                | WMI_PEER_HE
                | WMI_PEER_TWT_REQ
                | WMI_PEER_TWT_RESP
                | WMI_PEER_PMF
        );

        let p = PeerAssocParams {
            is_wme_set: false,
            ht_flag: true,
            auth_flag: true,
            need_ptk_4_way: true,
            is_assoc: true,
            peer_ht_rates: vec![1],
            ..Default::default()
        };
        assert_eq!(
            copy_peer_flags(&p, true),
            WMI_PEER_AUTH | WMI_PEER_NEED_PTK_4_WAY
        );

        let p = PeerAssocParams {
            is_wme_set: true,
            ht_flag: true,
            ..Default::default()
        };
        assert_eq!(copy_peer_flags(&p, false), 0);
    }

    #[test]
    fn source_derived_peer_assoc_byte_layout() {
        let command = PeerAssoc {
            hw_crypto_disabled: false,
            params: PeerAssocParams {
                vdev_id: 1,
                peer_new_assoc: 2,
                peer_associd: 3,
                peer_mac: [0x11, 0x22, 0x33, 0x44, 0x55, 0x66],
                peer_rate_caps: 10,
                peer_caps: 5,
                peer_listen_intval: 6,
                peer_ht_caps: 7,
                peer_max_mpdu: 8,
                peer_mpdu_density: 9,
                peer_vht_caps: 12,
                peer_phymode: 13,
                peer_nss: 11,
                peer_bw_rxnss_override: 14,
                peer_legacy_rates: vec![0xa1, 0xa2, 0xa3],
                peer_ht_rates: vec![0xb1, 0xb2, 0xb3, 0xb4, 0xb5],
                vht_capable: true,
                rx_max_rate: 0xc1,
                rx_mcs_set: 0xc2,
                tx_max_rate: 0xc3,
                tx_mcs_set: 0xc4,
                peer_he_mcs: vec![
                    PeerHeRateSet {
                        rx_mcs_set: 0xd1,
                        tx_mcs_set: 0xd2,
                    },
                    PeerHeRateSet {
                        rx_mcs_set: 0xd3,
                        tx_mcs_set: 0xd4,
                    },
                ],
                min_data_rate: 0x21,
                peer_he_cap_macinfo: [0xe1, 0xe2],
                peer_he_cap_macinfo_internal: 0xe3,
                peer_he_caps_6ghz: 0xe4,
                peer_he_ops: 0xe5,
                peer_he_cap_phyinfo: [0xe6, 0xe7, 0xe8],
                peer_ppet: PeerPpeThreshold {
                    numss_m1: 0xf1,
                    ru_bit_mask: 0xf2,
                    ppet16_ppet8_ru3_ru0: [0xf3, 0xf4, 0xf5, 0xf6, 0xf7, 0xf8, 0xf9, 0xfa],
                },
                ..Default::default()
            },
        }
        .encode_command()
        .unwrap();

        let words = [
            0x0065_00a0,
            0x4433_2211,
            0x0000_6655,
            1,
            2,
            3,
            0,
            5,
            6,
            7,
            8,
            9,
            10,
            11,
            12,
            13,
            0,
            0,
            3,
            5,
            14,
            0xf1,
            0xf2,
            0xf3,
            0xf4,
            0xf5,
            0xf6,
            0xf7,
            0xf8,
            0xf9,
            0xfa,
            0xe1,
            0xe5,
            0xe6,
            0xe7,
            0xe8,
            2,
            0xe2,
            0xe3,
            0x21,
            0xe4,
            0x0011_0004,
            0x00a3_a2a1,
            0x0011_0008,
            0xb4b3_b2b1,
            0x0000_00b5,
            0x0066_0014,
            0xc3,
            0xc4,
            0xc1,
            0xc2,
            0,
            0x0012_0018,
            0x0285_0008,
            0xd1,
            0xd2,
            0x0285_0008,
            0xd3,
            0xd4,
        ];
        let expected: Vec<u8> = words.into_iter().flat_map(u32::to_le_bytes).collect();
        assert_eq!(command.id, WMI_PEER_ASSOC_CMDID);
        assert_eq!(command.tlvs(), expected);
    }

    #[test]
    fn source_array_capacities_fail_without_panicking() {
        for params in [
            PeerAssocParams {
                peer_legacy_rates: vec![0; 129],
                ..Default::default()
            },
            PeerAssocParams {
                peer_ht_rates: vec![0; 129],
                ..Default::default()
            },
            PeerAssocParams {
                peer_he_mcs: vec![PeerHeRateSet::default(); 4],
                ..Default::default()
            },
        ] {
            assert_eq!(
                PeerAssoc {
                    params,
                    hw_crypto_disabled: false
                }
                .encode_command(),
                Err(WmiError::Malformed)
            );
        }
    }

    #[test]
    fn differential_trace_covers_nested_values_and_branches() {
        let command = PeerAssoc {
            params: PeerAssocParams {
                peer_mac: [1, 2, 3, 4, 5, 6],
                peer_legacy_rates: vec![0x82],
                peer_ht_rates: vec![0x91],
                peer_he_mcs: vec![PeerHeRateSet {
                    rx_mcs_set: 0x1234,
                    tx_mcs_set: 0x5678,
                }],
                peer_ppet: PeerPpeThreshold {
                    ppet16_ppet8_ru3_ru0: [9; 8],
                    ..Default::default()
                },
                vht_capable: true,
                auth_flag: true,
                ..Default::default()
            },
            hw_crypto_disabled: true,
        };
        let mut trace = Trace::default();
        command.encode_command_with_trace(&mut trace).unwrap();

        assert!(trace.0.contains(&TraceEvent::Field {
            name: "PeerAssoc.params.peer_mac[]",
            value: 6,
        }));
        assert!(trace.0.contains(&TraceEvent::Field {
            name: "PeerAssoc.params.peer_legacy_rates[]",
            value: 0x82,
        }));
        assert!(trace.0.contains(&TraceEvent::Field {
            name: "PeerAssoc.params.peer_he_mcs[].tx_mcs_set",
            value: 0x5678,
        }));
        assert!(trace.0.contains(&TraceEvent::Field {
            name: "PeerAssoc.params.peer_ppet.ppet16_ppet8_ru3_ru0[]",
            value: 9,
        }));
        assert!(trace.0.contains(&TraceEvent::Branch {
            name: "PeerAssoc.params.vht_capable",
            taken: true,
        }));
        assert!(trace.0.contains(&TraceEvent::Branch {
            name: "PeerAssoc.hw_crypto_disabled",
            taken: true,
        }));
    }

    #[cfg(feature = "proptest")]
    proptest::proptest! {
        #![proptest_config(proptest::test_runner::Config::with_cases(32))]

        #[test]
        fn generated_peer_assoc_respects_source_capacities(command in PeerAssoc::strategy()) {
            proptest::prop_assert!(command.params.peer_legacy_rates.len() <= 128);
            proptest::prop_assert!(command.params.peer_ht_rates.len() <= 128);
            proptest::prop_assert!(command.params.peer_he_mcs.len() <= 3);
            proptest::prop_assert!(command.encode_command().is_ok());
        }
    }
}
