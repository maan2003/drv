// Copyright 2026 The drv Authors.
// SPDX-License-Identifier: BSD-3-Clause

//! Semantic association-request inputs and deterministic IE serialization.

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SupportedChannelRange {
    pub first: u8,
    pub count: u8,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RegulatoryAssociationCapabilities {
    pub min_tx_power_dbm: i8,
    pub max_tx_power_dbm: i8,
    pub supported_channels: Vec<SupportedChannelRange>,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct ImplementedStationCapabilities {
    /// IEEE 802.11k RM Enabled Capabilities; set only when action handling exists.
    pub rm_enabled: Option<[u8; 5]>,
    /// Extended Capabilities; set only for behavior implemented by the station.
    pub extended: Option<Vec<u8>>,
    /// Complete Extension IE bodies, including the extension ID.
    pub extension: Vec<Vec<u8>>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RsnStationPolicy {
    pub mfpc: bool,
    pub mfpr: bool,
    pub ptk_replay_counters: u8,
    pub gtk_replay_counters: u8,
}

impl RsnStationPolicy {
    pub const fn wpa3_personal() -> Self {
        Self {
            mfpc: true,
            mfpr: false,
            ptk_replay_counters: 1,
            gtk_replay_counters: 1,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AssociationRequestProfile {
    pub regulatory: Option<RegulatoryAssociationCapabilities>,
    pub station: ImplementedStationCapabilities,
    /// Optional RSN capability rewrite. `None` keeps the SME's RSNE verbatim,
    /// which is mandatory in production: IEEE 802.11-2020 12.7.6.3 requires
    /// the RSNE in EAPOL-Key message 2/4 to be bit-identical to the RSNE in
    /// the (Re)Association Request, and hostapd disconnects the station
    /// ("WPA IE from (Re)AssocReq did not match with msg 2/4") when it is
    /// not. The supplicant that authors message 2/4 owns the RSNE, so the
    /// driver must not restate it. Only comparison fixtures set a policy.
    pub rsn: Option<RsnStationPolicy>,
    /// Authoritative station capabilities, when the hardware query is more
    /// precise than the ClientMlme intersection used to create the base frame.
    pub ht_capabilities: Option<[u8; 26]>,
    pub vht_capabilities: Option<[u8; 12]>,
}

impl Default for AssociationRequestProfile {
    fn default() -> Self {
        Self {
            regulatory: None,
            station: ImplementedStationCapabilities::default(),
            rsn: None,
            ht_capabilities: None,
            vht_capabilities: None,
        }
    }
}

/// Decoded Linux 6.18.40 oracle fixture. This is a comparison fixture, not a
/// runtime default: callers may advertise its RRM/Extended/FILS fields only
/// after implementing the corresponding station behavior.
pub fn linux_61840_oracle_profile() -> AssociationRequestProfile {
    AssociationRequestProfile {
        regulatory: Some(RegulatoryAssociationCapabilities {
            min_tx_power_dbm: 0,
            max_tx_power_dbm: 20,
            supported_channels: [
                36, 40, 44, 48, 52, 56, 60, 64, 100, 104, 108, 112, 116, 120, 124, 128, 132, 136,
                140, 144, 149, 153, 157, 161, 165, 169, 173, 177,
            ]
            .map(|first| SupportedChannelRange { first, count: 1 })
            .to_vec(),
        }),
        station: ImplementedStationCapabilities {
            rm_enabled: Some([0x70, 0, 0, 0, 0]),
            extended: Some(vec![0x04, 0, 0x08, 0, 0x01, 0, 0, 0x40, 0, 0x01]),
            extension: vec![vec![0x06, 0x1a]],
        },
        // iwd advertises MFPC only; the fixture keeps that so the pinned
        // 204-byte oracle hash stays comparable. Production leaves the RSNE
        // to the supplicant (see `AssociationRequestProfile::rsn`).
        rsn: Some(RsnStationPolicy::wpa3_personal()),
        ht_capabilities: Some([
            0xff, 0x09, 0x03, 0xff, 0xff, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 1, 0, 0, 0, 0, 0, 0, 0, 0,
            0, 0,
        ]),
        vht_capabilities: Some([
            0xb2, 0x71, 0x80, 0x33, 0xfa, 0xff, 0, 0, 0xfa, 0xff, 0, 0x20,
        ]),
    }
}

fn replay_encoding(count: u8) -> Result<u16, &'static str> {
    match count {
        1 => Ok(0),
        2 => Ok(1),
        4 => Ok(2),
        16 => Ok(3),
        _ => Err("RSN replay-counter count must be 1, 2, 4, or 16"),
    }
}

fn rewrite_rsn_capabilities(body: &mut [u8], policy: RsnStationPolicy) -> Result<(), &'static str> {
    if body.len() < 18 || body[0..2] != [1, 0] {
        return Err("malformed RSN body");
    }
    let pairwise_count = usize::from(u16::from_le_bytes([body[6], body[7]]));
    let mut offset = 8usize
        .checked_add(pairwise_count.checked_mul(4).ok_or("RSN overflow")?)
        .ok_or("RSN overflow")?;
    let akm_count_bytes = body
        .get(offset..offset + 2)
        .ok_or("truncated RSN AKM count")?;
    let akm_count = usize::from(u16::from_le_bytes(akm_count_bytes.try_into().unwrap()));
    offset = offset
        .checked_add(2 + akm_count.checked_mul(4).ok_or("RSN overflow")?)
        .ok_or("RSN overflow")?;
    if offset.checked_add(2) != Some(body.len()) {
        return Err("unsupported RSN optional fields");
    }
    let caps = body
        .get_mut(offset..offset + 2)
        .ok_or("missing RSN capabilities")?;
    let mut value = replay_encoding(policy.ptk_replay_counters)? << 2;
    value |= replay_encoding(policy.gtk_replay_counters)? << 4;
    if policy.mfpr && !policy.mfpc {
        return Err("MFPR requires MFPC");
    }
    if policy.mfpr {
        value |= 1 << 6;
    }
    if policy.mfpc {
        value |= 1 << 7;
    }
    caps.copy_from_slice(&value.to_le_bytes());
    Ok(())
}

fn push_ie(out: &mut Vec<u8>, id: u8, body: &[u8]) -> Result<(), &'static str> {
    let len = u8::try_from(body.len()).map_err(|_| "association IE is too long")?;
    out.extend_from_slice(&[id, len]);
    out.extend_from_slice(body);
    Ok(())
}

/// Rewrites a ClientMlme-generated association request from semantic inputs.
///
/// The input must contain exactly SSID, Supported Rates, RSN, HT, and VHT in
/// that order. Device HT/VHT and SME RSN suites remain authoritative; this
/// function only derives RSN capability bits and inserts caller-owned
/// regulatory/implemented-station IEs.
pub fn finalize_association_request(
    frame: &[u8],
    profile: &AssociationRequestProfile,
) -> Result<Vec<u8>, &'static str> {
    if frame.len() < 28 {
        return Err("association request is truncated");
    }
    let mut parsed = Vec::new();
    let mut offset = 28;
    while offset < frame.len() {
        let header = frame.get(offset..offset + 2).ok_or("truncated IE header")?;
        let next = offset
            .checked_add(2 + usize::from(header[1]))
            .ok_or("IE overflow")?;
        let body = frame.get(offset + 2..next).ok_or("truncated IE body")?;
        parsed.push((header[0], body.to_vec()));
        offset = next;
    }
    let ids = parsed.iter().map(|(id, _)| *id).collect::<Vec<_>>();
    if ids != [0, 1, 48, 45, 191]
        && ids != [0, 1, 48, 45, 191, 221]
        && ids != [0, 1, 48, 45, 191, 244, 221]
    {
        return Err("unexpected base association IE order");
    }
    if parsed[3].1.len() != 26 || parsed[4].1.len() != 12 {
        return Err("malformed HT/VHT capability body");
    }
    let tail = match parsed.len() {
        5 => None,
        6 if parsed[5].1 == [0, 0x50, 0xf2, 2, 0, 1, 0] => Some((None, &parsed[5].1)),
        7 if parsed[5].1 == [0x20] && parsed[6].1 == [0, 0x50, 0xf2, 2, 0, 1, 0] => {
            Some((Some(&parsed[5].1), &parsed[6].1))
        }
        _ => return Err("unexpected RSNXE/WMM association tail"),
    };
    let mut rsn = parsed[2].1.clone();
    if let Some(policy) = profile.rsn {
        rewrite_rsn_capabilities(&mut rsn, policy)?;
    }

    let mut out = frame[..28].to_vec();
    // Association capabilities describe station behavior, not the selected
    // AP.  ClientMlme's intersection still retains AP-only short-slot and
    // spectrum/RRM bits, so start from the only baseline bits implemented by
    // this client and add semantic station features below.
    let mut capability = u16::from_le_bytes(out[24..26].try_into().unwrap()) & 0x0011;
    if profile.regulatory.is_some() {
        capability |= 0x0100;
    }
    if profile.station.rm_enabled.is_some() {
        capability |= 0x1000;
    }
    out[24..26].copy_from_slice(&capability.to_le_bytes());
    push_ie(&mut out, 0, &parsed[0].1)?;
    push_ie(&mut out, 1, &parsed[1].1)?;
    if let Some(regulatory) = &profile.regulatory {
        if regulatory.min_tx_power_dbm > regulatory.max_tx_power_dbm
            || regulatory.supported_channels.is_empty()
            || regulatory
                .supported_channels
                .iter()
                .any(|range| range.count == 0)
        {
            return Err("invalid regulatory association inputs");
        }
        push_ie(
            &mut out,
            33,
            &[
                regulatory.min_tx_power_dbm as u8,
                regulatory.max_tx_power_dbm as u8,
            ],
        )?;
        let mut channels = Vec::with_capacity(regulatory.supported_channels.len() * 2);
        for range in &regulatory.supported_channels {
            channels.extend_from_slice(&[range.first, range.count]);
        }
        push_ie(&mut out, 36, &channels)?;
    }
    push_ie(&mut out, 48, &rsn)?;
    if let Some(rm) = profile.station.rm_enabled {
        push_ie(&mut out, 70, &rm)?;
    }
    push_ie(
        &mut out,
        45,
        profile
            .ht_capabilities
            .as_ref()
            .map(|cap| cap.as_slice())
            .unwrap_or(&parsed[3].1),
    )?;
    if let Some(ext) = &profile.station.extended {
        push_ie(&mut out, 127, ext)?;
    }
    push_ie(
        &mut out,
        191,
        profile
            .vht_capabilities
            .as_ref()
            .map(|cap| cap.as_slice())
            .unwrap_or(&parsed[4].1),
    )?;
    for ext in &profile.station.extension {
        if ext.is_empty() {
            return Err("empty extension IE body");
        }
        push_ie(&mut out, 255, ext)?;
    }
    if let Some((rsnxe, wmm)) = tail {
        if let Some(rsnxe) = rsnxe {
            push_ie(&mut out, 244, rsnxe)?;
        }
        push_ie(&mut out, 221, wmm)?;
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn base() -> Vec<u8> {
        let mut frame = vec![0; 28];
        frame[24..26].copy_from_slice(&0x11u16.to_le_bytes());
        for (id, body) in [
            (0, vec![1, 2, 3]),
            (1, vec![0x0c; 8]),
            (
                48,
                vec![
                    1, 0, 0, 0x0f, 0xac, 4, 1, 0, 0, 0x0f, 0xac, 4, 1, 0, 0, 0x0f, 0xac, 8, 0xcc, 0,
                ],
            ),
            (45, vec![0; 26]),
            (191, vec![0; 12]),
        ] {
            push_ie(&mut frame, id, &body).unwrap();
        }
        frame
    }

    #[test]
    fn derives_rsn_and_rejects_bad_inputs() {
        let out =
            finalize_association_request(&base(), &AssociationRequestProfile::default()).unwrap();
        let rsn = out.windows(2).position(|bytes| bytes == [48, 20]).unwrap();
        // Production keeps the SME RSNE verbatim so it matches EAPOL 2/4.
        assert_eq!(&out[rsn + 20..rsn + 22], &[0xcc, 0]);
        let mut fixture = AssociationRequestProfile::default();
        fixture.rsn = Some(RsnStationPolicy::wpa3_personal());
        let out = finalize_association_request(&base(), &fixture).unwrap();
        let rsn = out.windows(2).position(|bytes| bytes == [48, 20]).unwrap();
        assert_eq!(&out[rsn + 20..rsn + 22], &[0x80, 0]);
        let mut invalid = AssociationRequestProfile::default();
        invalid.rsn = Some(RsnStationPolicy {
            mfpc: false,
            mfpr: true,
            ..RsnStationPolicy::wpa3_personal()
        });
        assert!(finalize_association_request(&base(), &invalid).is_err());
        invalid.rsn = None;
        invalid.regulatory = Some(RegulatoryAssociationCapabilities {
            min_tx_power_dbm: 20,
            max_tx_power_dbm: 0,
            supported_channels: vec![],
        });
        assert!(finalize_association_request(&base(), &invalid).is_err());
        let mut malformed = base();
        malformed.pop();
        assert!(
            finalize_association_request(&malformed, &AssociationRequestProfile::default())
                .is_err()
        );

        let mut stale = base();
        push_ie(&mut stale, 119, &[0]).unwrap();
        assert!(
            finalize_association_request(&stale, &AssociationRequestProfile::default()).is_err()
        );

        let mut wrong_ht = base();
        let ht_length = 28 + 2 + 3 + 2 + 8 + 2 + 20 + 1;
        wrong_ht[ht_length] = 25;
        wrong_ht.remove(ht_length + 2 + 25);
        assert!(
            finalize_association_request(&wrong_ht, &AssociationRequestProfile::default()).is_err()
        );
    }

    #[test]
    fn preserves_wmm_when_h2e_rsnxe_is_absent() {
        let mut frame = base();
        push_ie(&mut frame, 221, &[0, 0x50, 0xf2, 2, 0, 1, 0]).unwrap();
        let out =
            finalize_association_request(&frame, &AssociationRequestProfile::default()).unwrap();
        assert!(out.ends_with(&[221, 7, 0, 0x50, 0xf2, 2, 0, 1, 0]));
    }
}
