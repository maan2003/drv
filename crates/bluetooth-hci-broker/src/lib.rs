use std::collections::BTreeMap;
use std::io;

pub const MAX_HCI_PACKET: usize = 260;
pub const MAX_OUTBOUND_HCI_PACKET: usize = 4096;
pub const H4_EVENT: u8 = 0x04;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum OutboundKind {
    Command,
    Acl,
    Sco,
    Iso,
}

pub fn validate_outbound(kind: OutboundKind, packet: &[u8]) -> io::Result<()> {
    if packet.len() > MAX_OUTBOUND_HCI_PACKET {
        return Err(invalid("outbound HCI packet exceeds boundary"));
    }
    let (header, payload_len) = match kind {
        OutboundKind::Command => (3, packet.get(2).copied().map(usize::from)),
        OutboundKind::Acl => (
            4,
            packet
                .get(2..4)
                .map(|bytes| usize::from(u16::from_le_bytes([bytes[0], bytes[1]]))),
        ),
        OutboundKind::Sco => (3, packet.get(2).copied().map(usize::from)),
        OutboundKind::Iso => (
            4,
            packet
                .get(2..4)
                .map(|bytes| usize::from(u16::from_le_bytes([bytes[0], bytes[1]]) & 0x3fff)),
        ),
    };
    let payload_len = payload_len.ok_or_else(|| invalid("short outbound HCI packet"))?;
    if packet.len() != header + payload_len {
        return Err(invalid("malformed outbound HCI packet length"));
    }
    Ok(())
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Event<'a> {
    pub code: u8,
    pub parameters: &'a [u8],
}

pub fn decode_event(packet: &[u8]) -> io::Result<Event<'_>> {
    if packet.len() > MAX_HCI_PACKET {
        return Err(invalid("HCI packet exceeds boundary"));
    }
    if packet.len() < 3 || packet[0] != H4_EVENT {
        return Err(invalid("not an HCI event packet"));
    }
    let parameter_len = usize::from(packet[2]);
    if packet.len() != parameter_len + 3 {
        return Err(invalid("malformed HCI event length"));
    }
    Ok(Event {
        code: packet[1],
        parameters: &packet[3..],
    })
}

#[derive(Debug, Default)]
pub struct CommandGate {
    credits: u8,
    pending: Option<u16>,
}

impl CommandGate {
    pub fn new() -> Self {
        Self {
            credits: 1,
            pending: None,
        }
    }

    pub fn begin(&mut self, opcode: u16) -> io::Result<()> {
        if self.credits == 0 || self.pending.is_some() {
            return Err(io::Error::new(
                io::ErrorKind::WouldBlock,
                "no HCI command credit",
            ));
        }
        self.credits -= 1;
        self.pending = Some(opcode);
        Ok(())
    }

    pub fn observe(&mut self, event: &Event<'_>) -> io::Result<Option<u8>> {
        let (credits, opcode, status) = match event.code {
            0x0e if event.parameters.len() >= 4 => (
                event.parameters[0],
                u16::from_le_bytes([event.parameters[1], event.parameters[2]]),
                event.parameters[3],
            ),
            0x0f if event.parameters.len() == 4 => (
                event.parameters[1],
                u16::from_le_bytes([event.parameters[2], event.parameters[3]]),
                event.parameters[0],
            ),
            0x0e | 0x0f => return Err(invalid("malformed HCI command response")),
            _ => return Ok(None),
        };
        self.credits = credits;
        if self.pending == Some(opcode) {
            self.pending = None;
            return Ok(Some(status));
        }
        Ok(None)
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Peer {
    pub address_type: &'static str,
    pub address: [u8; 6],
    pub name: Option<String>,
    pub best_rssi: Option<i8>,
    pub sightings: u32,
}

#[derive(Debug, Default)]
pub struct Peers(BTreeMap<(u8, [u8; 6]), Peer>);

impl Peers {
    pub fn values(&self) -> impl Iterator<Item = &Peer> {
        self.0.values()
    }
    pub fn len(&self) -> usize {
        self.0.len()
    }
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    fn record(&mut self, kind: u8, address: [u8; 6], data: &[u8], rssi: Option<i8>) {
        let address_type = match kind {
            0 => "bredr",
            1 => "le-public",
            _ => "le-random",
        };
        let peer = self.0.entry((kind, address)).or_insert(Peer {
            address_type,
            address,
            name: None,
            best_rssi: None,
            sightings: 0,
        });
        peer.sightings += 1;
        if let Some(rssi) = rssi {
            peer.best_rssi = Some(peer.best_rssi.map_or(rssi, |old| old.max(rssi)));
        }
        if let Some(name) = advertised_name(data) {
            peer.name = Some(name);
        }
    }

    pub fn observe(&mut self, event: &Event<'_>) -> io::Result<bool> {
        match event.code {
            0x01 => return Ok(true),
            0x02 => self.observe_inquiry(event.parameters, false)?,
            0x22 => self.observe_inquiry(event.parameters, true)?,
            0x2f => self.observe_extended_inquiry(event.parameters)?,
            0x3e => self.observe_le(event.parameters)?,
            _ => {}
        }
        Ok(false)
    }

    fn observe_inquiry(&mut self, bytes: &[u8], with_rssi: bool) -> io::Result<()> {
        let (&count, rest) = bytes
            .split_first()
            .ok_or_else(|| invalid("empty inquiry result"))?;
        // Both response forms are 14 bytes. The non-RSSI form has two
        // reserved bytes; the RSSI form replaces one with the signal value.
        let width = 14;
        if rest.len() != usize::from(count) * width {
            return Err(invalid("malformed inquiry result"));
        }
        for entry in rest.chunks_exact(width) {
            let address = entry[0..6].try_into().unwrap();
            self.record(0, address, &[], with_rssi.then(|| entry[13] as i8));
        }
        Ok(())
    }

    fn observe_extended_inquiry(&mut self, bytes: &[u8]) -> io::Result<()> {
        let (&count, rest) = bytes
            .split_first()
            .ok_or_else(|| invalid("empty extended inquiry result"))?;
        if rest.len() != usize::from(count) * 254 {
            return Err(invalid("malformed extended inquiry result"));
        }
        for entry in rest.chunks_exact(254) {
            let address = entry[0..6].try_into().unwrap();
            self.record(0, address, &entry[14..254], Some(entry[13] as i8));
        }
        Ok(())
    }

    fn observe_le(&mut self, bytes: &[u8]) -> io::Result<()> {
        if bytes.first() != Some(&0x02) {
            return Ok(());
        }
        let count = *bytes
            .get(1)
            .ok_or_else(|| invalid("short LE advertising report"))?;
        let mut rest = &bytes[2..];
        for _ in 0..count {
            if rest.len() < 10 {
                return Err(invalid("short LE advertising entry"));
            }
            let kind = match rest[1] {
                0 => 1,
                1 => 2,
                _ => return Err(invalid("invalid LE address type")),
            };
            let address = rest[2..8].try_into().unwrap();
            let data_len = usize::from(rest[8]);
            if rest.len() < 10 + data_len {
                return Err(invalid("truncated LE advertising data"));
            }
            self.record(
                kind,
                address,
                &rest[9..9 + data_len],
                Some(rest[9 + data_len] as i8),
            );
            rest = &rest[10 + data_len..];
        }
        if !rest.is_empty() {
            return Err(invalid("trailing LE advertising bytes"));
        }
        Ok(())
    }
}

fn advertised_name(mut data: &[u8]) -> Option<String> {
    while let Some((&len, tail)) = data.split_first() {
        if len == 0 {
            break;
        }
        if tail.len() < usize::from(len) {
            return None;
        }
        let field = &tail[..usize::from(len)];
        if matches!(field[0], 0x08 | 0x09) {
            return std::str::from_utf8(&field[1..]).ok().map(ToOwned::to_owned);
        }
        data = &tail[usize::from(len)..];
    }
    None
}

fn invalid(message: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CleanupAction {
    DisableLeScan,
    CancelInquiry,
}

#[derive(Debug, Default)]
pub struct ActiveProcedures(u8);

impl ActiveProcedures {
    const LE_SCAN: u8 = 1;
    const INQUIRY: u8 = 2;

    pub fn start_le_scan(&mut self) {
        self.0 |= Self::LE_SCAN;
    }

    pub fn finish_le_scan(&mut self) {
        self.0 &= !Self::LE_SCAN;
    }

    pub fn start_inquiry(&mut self) {
        self.0 |= Self::INQUIRY;
    }

    pub fn finish_inquiry(&mut self) {
        self.0 &= !Self::INQUIRY;
    }

    pub fn cleanup_actions(&self) -> impl Iterator<Item = CleanupAction> {
        [
            (self.0 & Self::LE_SCAN != 0).then_some(CleanupAction::DisableLeScan),
            (self.0 & Self::INQUIRY != 0).then_some(CleanupAction::CancelInquiry),
        ]
        .into_iter()
        .flatten()
    }

    pub fn clear(&mut self) {
        self.0 = 0;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn event(code: u8, parameters: &[u8]) -> Vec<u8> {
        let mut out = vec![H4_EVENT, code, parameters.len() as u8];
        out.extend_from_slice(parameters);
        out
    }

    #[test]
    fn rejects_malformed_and_oversize_packets() {
        assert!(decode_event(&[4, 1, 2, 0]).is_err());
        assert!(decode_event(&vec![0; MAX_HCI_PACKET + 1]).is_err());
        assert!(decode_event(&[2, 0, 0]).is_err());
    }

    #[test]
    fn validates_bounded_outbound_transport_frames() {
        validate_outbound(OutboundKind::Command, &[0x0c, 0x20, 1, 1]).unwrap();
        validate_outbound(OutboundKind::Acl, &[1, 0, 2, 0, 0xaa, 0xbb]).unwrap();
        validate_outbound(OutboundKind::Sco, &[1, 0, 1, 0xaa]).unwrap();
        validate_outbound(OutboundKind::Iso, &[1, 0, 2, 0, 0xaa, 0xbb]).unwrap();

        assert!(validate_outbound(OutboundKind::Command, &[0x0c, 0x20, 2, 1]).is_err());
        assert!(validate_outbound(OutboundKind::Acl, &[1, 0, 3, 0, 0xaa]).is_err());
        assert!(
            validate_outbound(OutboundKind::Iso, &vec![0; MAX_OUTBOUND_HCI_PACKET + 1]).is_err()
        );
    }

    #[test]
    fn command_gate_tracks_credits_and_matching_completion() {
        let mut gate = CommandGate::new();
        gate.begin(0x200c).unwrap();
        assert_eq!(
            gate.begin(0x0401).unwrap_err().kind(),
            io::ErrorKind::WouldBlock
        );
        let packet = event(0x0e, &[2, 0x0c, 0x20, 0]);
        assert_eq!(
            gate.observe(&decode_event(&packet).unwrap()).unwrap(),
            Some(0)
        );
        gate.begin(0x0401).unwrap();
    }

    #[test]
    fn unrelated_response_does_not_complete_pending_command() {
        let mut gate = CommandGate::new();
        gate.begin(0x0401).unwrap();

        let unrelated = event(0x0e, &[1, 0x03, 0x0c, 0]);
        assert_eq!(
            gate.observe(&decode_event(&unrelated).unwrap()).unwrap(),
            None
        );
        assert_eq!(
            gate.begin(0x200c).unwrap_err().kind(),
            io::ErrorKind::WouldBlock
        );

        let matching_status = event(0x0f, &[0x0c, 2, 0x01, 0x04]);
        assert_eq!(
            gate.observe(&decode_event(&matching_status).unwrap())
                .unwrap(),
            Some(0x0c)
        );
        gate.begin(0x200c).unwrap();
    }

    #[test]
    fn deduplicates_le_reports_and_keeps_best_signal_and_name() {
        let address = [1, 2, 3, 4, 5, 6];
        let mut peers = Peers::default();
        for rssi in [-70i8, -45] {
            let mut parameters = vec![0x02, 1, 0, 0];
            parameters.extend_from_slice(&address);
            parameters.extend_from_slice(&[5, 4, 0x09, b't', b'a', b'g', rssi as u8]);
            peers
                .observe(&decode_event(&event(0x3e, &parameters)).unwrap())
                .unwrap();
        }
        let peer = peers.values().next().unwrap();
        assert_eq!(peer.sightings, 2);
        assert_eq!(peer.best_rssi, Some(-45));
        assert_eq!(peer.name.as_deref(), Some("tag"));
    }

    #[test]
    fn rejects_truncated_advertising_and_inquiry() {
        let mut peers = Peers::default();
        assert!(
            peers
                .observe(&decode_event(&event(0x3e, &[2, 1, 0])).unwrap())
                .is_err()
        );
        assert!(
            peers
                .observe(&decode_event(&event(0x22, &[1, 0])).unwrap())
                .is_err()
        );
    }

    #[test]
    fn decodes_sapphire_rssi_inquiry_fixture_shape() {
        // Ported byte-for-byte in shape from Sapphire's pinned
        // bredr_discovery_manager_test.cc kRSSIInquiryResult fixture.
        let packet = event(
            0x22,
            &[
                1, // response count
                2, 0, 0, 0, 0, 0, // address
                0, // page scan repetition mode
                0, // reserved
                0, 0x1f, 0, // class of device
                0, 0,    // clock offset
                0xec, // -20 dBm
            ],
        );
        let mut peers = Peers::default();
        peers.observe(&decode_event(&packet).unwrap()).unwrap();
        let peer = peers.values().next().unwrap();
        assert_eq!(peer.address, [2, 0, 0, 0, 0, 0]);
        assert_eq!(peer.best_rssi, Some(-20));
    }

    #[test]
    fn cleanup_plan_cancels_only_procedures_that_may_be_active() {
        let mut active = ActiveProcedures::default();
        active.start_le_scan();
        active.start_inquiry();
        assert_eq!(
            active.cleanup_actions().collect::<Vec<_>>(),
            vec![CleanupAction::DisableLeScan, CleanupAction::CancelInquiry]
        );
        active.finish_le_scan();
        assert_eq!(
            active.cleanup_actions().collect::<Vec<_>>(),
            vec![CleanupAction::CancelInquiry]
        );
        active.clear();
        assert!(active.cleanup_actions().next().is_none());
    }
}
