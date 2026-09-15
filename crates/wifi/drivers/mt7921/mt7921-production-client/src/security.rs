//! Association-scoped receive integrity and replay checks.
use aes::Aes128;
use cmac::{Cmac, Mac};

/// Verify BIP-CMAC-128 before advancing the IGTK receive IPN.
pub(super) fn verify_bip(frame: &[u8], key: &mut crate::peer::ClientKey) -> bool {
    if frame.len() < 42 || frame[frame.len() - 18..frame.len() - 16] != [76, 16] {
        return false;
    }
    let mmie = frame.len() - 16;
    if u16::from_le_bytes([frame[mmie], frame[mmie + 1]]) != u16::from(key.index) {
        return false;
    }
    let mut wire = [0; 8];
    wire[..6].copy_from_slice(&frame[mmie + 2..mmie + 8]);
    let ipn = u64::from_le_bytes(wire);
    if ipn <= key.rx_pn[0] {
        return false;
    }
    let Ok(mut mac) = Cmac::<Aes128>::new_from_slice(&key.bytes) else {
        return false;
    };
    let fc = u16::from_le_bytes([frame[0], frame[1]]) & !0x3800;
    mac.update(&fc.to_le_bytes());
    mac.update(&frame[4..22]);
    mac.update(&frame[24..frame.len() - 8]);
    mac.update(&[0; 8]);
    if mac
        .verify_truncated_left(&frame[frame.len() - 8..])
        .is_err()
    {
        return false;
    }
    key.rx_pn[0] = ipn;
    true
}

/// Authenticate before exposing plaintext to MLME. Connac2 has already stripped
/// CCMP's IV and MIC; removing them again would corrupt the LLC payload.
pub(super) fn accept_rx(
    envelope: &[u8],
    frame: &mut mt7921_core::Connac2RxFrame,
    local: [u8; 6],
    peer: Option<[u8; 6]>,
    ptk: &mut Option<crate::peer::ClientKey>,
    gtk: &mut Option<crate::peer::ClientKey>,
    igtk: &mut Option<crate::peer::ClientKey>,
    pmf: bool,
) -> bool {
    let b = &frame.bytes;
    if b.len() < 24 {
        return false;
    }
    let fc = u16::from_le_bytes([b[0], b[1]]);
    let data = fc & 0x0c == 8;
    let robust = fc & 0x0c == 0
        && (matches!(fc & 0xf0, 0xa0 | 0xc0)
            || (fc & 0xf0 == 0xd0 && !matches!(b.get(24), Some(4) | Some(7) | Some(15))));
    if !data && !robust {
        return fc & 0x4000 == 0;
    }
    let Some(peer) = peer else {
        return false;
    };
    let group = b[4] & 1 != 0;
    if b[10..16] != peer || (!group && b[4..10] != local) {
        return false;
    }
    if b[22] & 15 != 0 || fc & 0x8400 != 0 {
        return false;
    }
    let rxd1 = u32::from_le_bytes(envelope[4..8].try_into().unwrap());
    let rxd2 = u32::from_le_bytes(envelope[8..12].try_into().unwrap());
    let rxd3 = u32::from_le_bytes(envelope[12..16].try_into().unwrap());
    let rxd4 = u32::from_le_bytes(envelope[16..20].try_into().unwrap());
    if rxd2 & (1 << 27) != 0 || rxd3 & (1 << 22) != 0 || rxd4 & 3 != 0 {
        return false;
    }
    if robust && group {
        if let Some(key) = igtk {
            if !verify_bip(b, key) {
                return false;
            }
            frame.bytes.truncate(frame.bytes.len() - 18);
            frame.bytes[1] &= !0x40;
            return true;
        }
        return !pmf && fc & 0x4000 == 0;
    }
    let qos = data && fc & 0x80 != 0;
    let header = if qos { 26 } else { 24 };
    if data && (fc & 0x300 != 0x200 || b.len() < header || (qos && b[24] & 0x80 != 0)) {
        return false;
    }
    let eapol = data && b.get(header..header + 8) == Some(&[0xaa, 0xaa, 3, 0, 0, 0, 0x88, 0x8e]);
    let cipher = (rxd1 >> 16) & 31;
    if cipher == 0 {
        return fc & 0x4000 == 0 && (eapol || (robust && !pmf));
    }
    if cipher != 4 || rxd1 & ((1 << 23) | (1 << 24)) != 0 || rxd1 & 0x3ff != 1 {
        return false;
    }
    let key = if group { gtk.as_mut() } else { ptk.as_mut() };
    let Some(key) = key else {
        return false;
    };
    if (rxd1 >> 21) & 3 != u32::from(key.index) {
        return false;
    }
    let Some(pn) = frame.pn else {
        return false;
    };
    let pn = u64::from_be_bytes([0, 0, pn[0], pn[1], pn[2], pn[3], pn[4], pn[5]]);
    let tid = if qos { usize::from(b[24] & 15) } else { 0 };
    let retained = if robust {
        &mut key.management_rx_pn
    } else {
        &mut key.rx_pn[tid]
    };
    if pn <= *retained {
        return false;
    }
    *retained = pn;
    frame.bytes[1] &= !0x40;
    true
}

/// Read the MFPC bit from the association request selected by SME. The
/// request's RSNE, rather than installed-key presence, owns this requirement.
pub(super) fn association_pmf(frame: &[u8]) -> Result<bool, zx::Status> {
    let mut ies = frame.get(28..).ok_or(zx::Status::INVALID_ARGS)?;
    let mut pmf = None;
    while !ies.is_empty() {
        let len = usize::from(*ies.get(1).ok_or(zx::Status::INVALID_ARGS)?);
        let body = ies.get(2..2 + len).ok_or(zx::Status::INVALID_ARGS)?;
        if ies[0] == 48 {
            if pmf.is_some() || body.len() < 8 || body[..2] != [1, 0] {
                return Err(zx::Status::INVALID_ARGS);
            }
            let pairs = usize::from(u16::from_le_bytes([body[6], body[7]]));
            let offset = 8 + 4 * pairs;
            let count = body
                .get(offset..offset + 2)
                .ok_or(zx::Status::INVALID_ARGS)?;
            let akms = usize::from(u16::from_le_bytes([count[0], count[1]]));
            let offset = offset + 2 + 4 * akms;
            let rest = body.get(offset..).ok_or(zx::Status::INVALID_ARGS)?;
            if rest.len() == 1 {
                return Err(zx::Status::INVALID_ARGS);
            }
            pmf = Some(rest.first().is_some_and(|caps| caps & 0x80 != 0));
        }
        ies = &ies[2 + len..];
    }
    Ok(pmf.unwrap_or(false))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn bip_authentication_rejects_tampering_and_replay() {
        let mut key = crate::peer::ClientKey {
            index: 4,
            bytes: zeroize::Zeroizing::new(vec![0x5a; 16]),
            rx_pn: [0; 16],
            management_rx_pn: 0,
        };
        let mut frame = vec![0u8; 26];
        frame[0] = 0xc0;
        frame[1] = 0x40;
        frame[4..10].fill(0xff);
        frame[10..22].fill(2);
        frame.extend_from_slice(&[76, 16, 4, 0, 1, 0, 0, 0, 0, 0]);
        frame.extend_from_slice(&[0; 8]);
        let mut mac = Cmac::<Aes128>::new_from_slice(&key.bytes).unwrap();
        mac.update(&frame[..2]);
        mac.update(&frame[4..22]);
        mac.update(&frame[24..]);
        let tag = mac.finalize().into_bytes();
        let offset = frame.len() - 8;
        frame[offset..].copy_from_slice(&tag[..8]);
        let mut tampered = frame.clone();
        tampered[24] ^= 1;
        assert!(!verify_bip(&tampered, &mut key));
        assert_eq!(key.rx_pn[0], 0);
        assert!(verify_bip(&frame, &mut key));
        assert!(!verify_bip(&frame, &mut key));
    }

    #[test]
    fn pmf_rejects_plaintext_robust_management_before_keys_exist() {
        let mut bytes = vec![0; 26];
        bytes[0] = 0xc0;
        bytes[4..10].copy_from_slice(&[2; 6]);
        bytes[10..16].copy_from_slice(&[4; 6]);
        let mut frame = mt7921_core::Connac2RxFrame {
            bytes,
            band: mt7921_core::PhysicalBand::Ghz5,
            channel: 149,
            rssi_dbm: -60,
            pn: None,
        };
        assert!(!accept_rx(
            &[0; 24],
            &mut frame,
            [2; 6],
            Some([4; 6]),
            &mut None,
            &mut None,
            &mut None,
            true
        ));
    }
}
