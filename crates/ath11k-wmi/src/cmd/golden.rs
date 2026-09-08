//! Data-driven native WMI transcript parsing and byte-exact verification.
use crate::{Command, CommandId, Event, EventId, WmiError};
use alloc::{string::String, vec::Vec};
use core::ops::Range;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TranscriptKind {
    Command,
    Event,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TranscriptRecord {
    pub seq: u64,
    pub timestamp_ns: u64,
    pub kind: TranscriptKind,
    pub id: u32,
    pub declared_len: usize,
    /// Full tracepoint payload, including the four-byte WMI command header.
    pub bytes: Vec<u8>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum GoldenError {
    MissingField(&'static str),
    InvalidField(&'static str),
    InvalidHex,
    LengthMismatch { declared: usize, actual: usize },
    TruncatedEnvelope,
    IdentifierMismatch { declared: u32, envelope: u32 },
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ByteMismatch {
    pub first_differing_offset: usize,
    pub expected: Option<u8>,
    pub actual: Option<u8>,
    pub expected_len: usize,
    pub actual_len: usize,
}

/// A command envelope recovered from a native trace record.
///
/// This is deliberately envelope-level verification: each TLV is separated
/// into its tag, declared value, and canonical zero padding, but the value is
/// retained as opaque bytes. It checks command-family dispatch, envelope and
/// TLV headers, lengths, padding, and documented masks. It does **not** verify
/// the field semantics or the existing concrete command-family encoders.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct GoldenCommandEnvelope {
    pub family: &'static str,
    tlvs: Vec<GoldenTlv>,
    /// Host-only fields excluded from comparison for this command family.
    pub masked_fields: &'static [&'static str],
    /// Byte ranges in the complete command envelope corresponding to fields
    /// present in this particular request.
    pub masked_ranges: Vec<Range<usize>>,
    id: CommandId,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct GoldenTlv {
    tag: u16,
    wire_len: u16,
    value: Vec<u8>,
}

impl crate::cmd::EncodeCommand for GoldenCommandEnvelope {
    fn encode_command(&self) -> Result<Command, WmiError> {
        let mut bytes = Vec::new();
        for tlv in &self.tlvs {
            bytes.extend_from_slice(
                &((u32::from(tlv.tag) << 16) | u32::from(tlv.wire_len)).to_le_bytes(),
            );
            bytes.extend_from_slice(&tlv.value);
            if tlv.value.len() == usize::from(tlv.wire_len) {
                bytes.resize(
                    bytes.len() + (tlv.value.len().next_multiple_of(4) - tlv.value.len()),
                    0,
                );
            }
        }
        Command::from_tlvs(self.id, bytes)
    }
}

const NO_MASKS: &[&str] = &[];
const INIT_MASKS: &[&str] = &["host_memory_chunks[].paddr"];
const MGMT_TX_MASKS: &[&str] = &["paddr", "frame"];

fn command_family(id: u32) -> Option<(&'static str, &'static [&'static str])> {
    Some(match id {
        0x000001 => ("init", INIT_MASKS),
        0x003001 => ("scan-start", NO_MASKS),
        0x003003 => ("scan-channel-list", NO_MASKS),
        0x003006 => ("scan-probe-request-oui", NO_MASKS),
        0x004003 => ("pdev-set-param", NO_MASKS),
        0x005001 => ("vdev-create", NO_MASKS),
        0x005002 => ("vdev-delete", NO_MASKS),
        0x005003 => ("vdev-start", NO_MASKS),
        0x005005 => ("vdev-up", NO_MASKS),
        0x005006 => ("vdev-stop", NO_MASKS),
        0x005008 => ("vdev-set-param", NO_MASKS),
        0x005009 => ("vdev-install-key", NO_MASKS),
        0x00500d => ("vdev-wmm-update", NO_MASKS),
        0x006001 => ("peer-create", NO_MASKS),
        0x006002 => ("peer-delete", NO_MASKS),
        0x006004 => ("peer-set-param", NO_MASKS),
        0x006005 => ("peer-assoc", NO_MASKS),
        0x006013 => ("peer-reorder-queue-setup", NO_MASKS),
        0x007008 => ("mgmt-tx-send", MGMT_TX_MASKS),
        0x00700c => ("bss-color-change-enable", NO_MASKS),
        0x009001 => ("sta-powersave-mode", NO_MASKS),
        0x009002 => ("sta-powersave-param", NO_MASKS),
        0x00a005 => ("pdev-dfs-phyerr-offload-enable", NO_MASKS),
        0x016001 => ("request-stats", NO_MASKS),
        0x01d010 => ("pdev-lro-config", NO_MASKS),
        0x02a003 => ("obss-color-collision-config", NO_MASKS),
        0x03a001 => ("set-current-country", NO_MASKS),
        0x03a002 => ("11d-scan-start", NO_MASKS),
        0x03a003 => ("11d-scan-stop", NO_MASKS),
        0x040001 => ("obss-spatial-reuse", NO_MASKS),
        _ => return None,
    })
}

/// Reverse maps a command captured by the pinned native ath11k tracepoint.
/// Unknown command IDs are deliberately not guessed.
pub fn reverse_map_command_envelope(
    id: CommandId,
    bytes: &[u8],
) -> Result<Option<GoldenCommandEnvelope>, WmiError> {
    let Some((family, masked_fields)) = command_family(id.0) else {
        return Ok(None);
    };
    let mut tlvs = Vec::new();
    let mut offset = 0usize;
    while offset < bytes.len() {
        let header = bytes.get(offset..offset + 4).ok_or(WmiError::Malformed)?;
        let header = u32::from_le_bytes(header.try_into().map_err(|_| WmiError::Malformed)?);
        let wire_len = (header & 0xffff) as u16;
        let len = usize::from(wire_len);
        // The pinned ath11k scan-channel-list encoder advertises the array as
        // `nested_bytes - TLV_HDR_SIZE`; the production Rust encoder preserves
        // this observable ABI quirk.  Consume the four bytes here rather than
        // misreading the final channel word as a new top-level TLV.
        let consumed_len = if id.0 == 0x003003 && offset != 0 {
            len.checked_add(4).ok_or(WmiError::Malformed)?
        } else {
            len
        };
        let padded = consumed_len.next_multiple_of(4);
        let value = bytes
            .get(offset + 4..offset + 4 + consumed_len)
            .ok_or(WmiError::Malformed)?;
        let padding = bytes
            .get(offset + 4 + consumed_len..offset + 4 + padded)
            .ok_or(WmiError::Malformed)?;
        if padding.iter().any(|byte| *byte != 0) {
            return Err(WmiError::Malformed);
        }
        tlvs.push(GoldenTlv {
            tag: (header >> 16) as u16,
            wire_len,
            value: value.to_vec(),
        });
        offset += 4 + padded;
    }

    let mut masked_ranges = Vec::new();
    if id.0 == 0x007008 {
        // Envelope (4), fixed TLV header (4), then vdev/desc/frequency (12).
        masked_ranges.push(20..28);
        // The byte-array TLV follows the 36-byte fixed TLV.  Its declared
        // value is the downloaded prefix of the host management frame.
        if let Some(frame) = tlvs.get(1) {
            masked_ranges.push(48..48 + frame.value.len());
        }
    } else if id.0 == 0x000001 {
        // A host-memory chunk is encoded as a 16-byte nested TLV whose first
        // eight value bytes are the DMA address.  The redwood golden has no
        // chunks, but keep the rule here so future captures cannot silently
        // compare process-specific addresses.
        let mut top = 4usize;
        for tlv in &tlvs {
            if tlv.tag == 0x12 {
                let mut nested = 0usize;
                while nested + 20 <= tlv.value.len() {
                    let h = u32::from_le_bytes(
                        tlv.value[nested..nested + 4]
                            .try_into()
                            .map_err(|_| WmiError::Malformed)?,
                    );
                    if (h >> 16) as u16 != 0x4c || (h & 0xffff) != 16 {
                        return Err(WmiError::Malformed);
                    }
                    masked_ranges.push(top + 4 + nested + 4..top + 4 + nested + 12);
                    nested += 20;
                }
            }
            top += 4 + tlv.value.len().next_multiple_of(4);
        }
    }
    Ok(Some(GoldenCommandEnvelope {
        family,
        tlvs,
        masked_fields,
        masked_ranges,
        id,
    }))
}

/// Compares command envelopes after applying the reverse mapper's documented
/// host-state masks.
pub fn compare_reencoded(
    expected: &[u8],
    actual: &[u8],
    masks: &[Range<usize>],
) -> Option<ByteMismatch> {
    let mut expected = expected.to_vec();
    let mut actual = actual.to_vec();
    for range in masks {
        let end = range.end.min(expected.len()).min(actual.len());
        if range.start < end {
            expected[range.start..end].fill(0);
            actual[range.start..end].fill(0);
        }
    }
    first_difference(&expected, &actual)
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Verification {
    CommandExact {
        seq: u64,
        id: u32,
    },
    CommandUnmapped {
        seq: u64,
        id: u32,
    },
    CommandMismatch {
        seq: u64,
        id: u32,
        mismatch: ByteMismatch,
    },
    EventDecoded {
        seq: u64,
        id: u32,
        decoder: &'static str,
    },
    DecodeFailed {
        seq: u64,
        id: u32,
        error: WmiError,
    },
}

fn json_value<'a>(line: &'a str, key: &'static str) -> Result<&'a str, GoldenError> {
    let mut needle = String::from("\"");
    needle.push_str(key);
    needle.push_str("\":");
    let start = line.find(&needle).ok_or(GoldenError::MissingField(key))? + needle.len();
    let tail = line[start..].trim_start();
    if let Some(tail) = tail.strip_prefix('"') {
        let end = tail.find('"').ok_or(GoldenError::InvalidField(key))?;
        Ok(&tail[..end])
    } else {
        Ok(tail
            .split([',', '}'])
            .next()
            .ok_or(GoldenError::InvalidField(key))?
            .trim())
    }
}
fn number(line: &str, key: &'static str) -> Result<u64, GoldenError> {
    json_value(line, key)?
        .parse()
        .map_err(|_| GoldenError::InvalidField(key))
}
fn decode_hex(value: &str) -> Result<Vec<u8>, GoldenError> {
    if !value.len().is_multiple_of(2)
        || value
            .bytes()
            .any(|b| !b.is_ascii_digit() && !(b'a'..=b'f').contains(&b))
    {
        return Err(GoldenError::InvalidHex);
    }
    value
        .as_bytes()
        .chunks_exact(2)
        .map(|p| {
            let d = |b: u8| if b <= b'9' { b - b'0' } else { b - b'a' + 10 };
            Ok((d(p[0]) << 4) | d(p[1]))
        })
        .collect()
}

pub fn parse_jsonl(input: &str) -> Result<Vec<TranscriptRecord>, GoldenError> {
    input
        .lines()
        .filter(|line| !line.trim().is_empty())
        .map(|line| {
            let kind = match json_value(line, "kind")? {
                "wmi_cmd" => TranscriptKind::Command,
                "wmi_event" => TranscriptKind::Event,
                _ => return Err(GoldenError::InvalidField("kind")),
            };
            let bytes = decode_hex(json_value(line, "bytes_hex")?)?;
            let declared_len = usize::try_from(number(line, "len")?)
                .map_err(|_| GoldenError::InvalidField("len"))?;
            if declared_len != bytes.len() {
                return Err(GoldenError::LengthMismatch {
                    declared: declared_len,
                    actual: bytes.len(),
                });
            }
            let record = TranscriptRecord {
                seq: number(line, "seq")?,
                timestamp_ns: number(line, "ts_ns")?,
                kind,
                id: u32::try_from(number(line, "id")?)
                    .map_err(|_| GoldenError::InvalidField("id"))?,
                declared_len,
                bytes,
            };
            record.envelope_parts()?;
            Ok(record)
        })
        .collect()
}

impl TranscriptRecord {
    pub fn envelope_parts(&self) -> Result<(u32, &[u8]), GoldenError> {
        let h = self.bytes.get(..4).ok_or(GoldenError::TruncatedEnvelope)?;
        let envelope =
            u32::from_le_bytes(h.try_into().map_err(|_| GoldenError::TruncatedEnvelope)?)
                & 0x00ff_ffff;
        if envelope != (self.id & 0x00ff_ffff) {
            return Err(GoldenError::IdentifierMismatch {
                declared: self.id,
                envelope,
            });
        }
        Ok((envelope, &self.bytes[4..]))
    }
}

pub fn first_difference(expected: &[u8], actual: &[u8]) -> Option<ByteMismatch> {
    let offset = expected
        .iter()
        .zip(actual)
        .position(|(a, b)| a != b)
        .or_else(|| (expected.len() != actual.len()).then_some(expected.len().min(actual.len())))?;
    Some(ByteMismatch {
        first_differing_offset: offset,
        expected: expected.get(offset).copied(),
        actual: actual.get(offset).copied(),
        expected_len: expected.len(),
        actual_len: actual.len(),
    })
}
fn command_envelope(command: &Command) -> Vec<u8> {
    let mut b = Vec::with_capacity(4 + command.tlvs().len());
    b.extend_from_slice(&(command.id.0 & 0x00ff_ffff).to_le_bytes());
    b.extend_from_slice(command.tlvs());
    b
}

/// Verifies a transcript while keeping command reverse-mapping and event
/// dispatch owned by their typed protocol modules.
pub fn verify_transcript<C, E>(
    records: &[TranscriptRecord],
    mut reencode: C,
    mut decode_event: E,
) -> Vec<Verification>
where
    C: FnMut(CommandId, &[u8]) -> Result<Option<Command>, WmiError>,
    E: FnMut(Event) -> Result<&'static str, WmiError>,
{
    records
        .iter()
        .map(|r| {
            let (_, tlvs) = match r.envelope_parts() {
                Ok(v) => v,
                Err(_) => {
                    return Verification::DecodeFailed {
                        seq: r.seq,
                        id: r.id,
                        error: WmiError::Malformed,
                    };
                }
            };
            match r.kind {
                TranscriptKind::Command => match reencode(CommandId(r.id), tlvs) {
                    Ok(None) => Verification::CommandUnmapped {
                        seq: r.seq,
                        id: r.id,
                    },
                    Err(error) => Verification::DecodeFailed {
                        seq: r.seq,
                        id: r.id,
                        error,
                    },
                    Ok(Some(command)) => {
                        let actual = command_envelope(&command);
                        match first_difference(&r.bytes, &actual) {
                            None => Verification::CommandExact {
                                seq: r.seq,
                                id: r.id,
                            },
                            Some(mismatch) => Verification::CommandMismatch {
                                seq: r.seq,
                                id: r.id,
                                mismatch,
                            },
                        }
                    }
                },
                TranscriptKind::Event => match Event::from_tlvs(EventId(r.id), tlvs.to_vec())
                    .and_then(&mut decode_event)
                {
                    Ok(decoder) => Verification::EventDecoded {
                        seq: r.seq,
                        id: r.id,
                        decoder,
                    },
                    Err(error) => Verification::DecodeFailed {
                        seq: r.seq,
                        id: r.id,
                        error,
                    },
                },
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::vec;
    #[test]
    fn parses_capture_schema_and_reports_first_offset() {
        let input = "{\"seq\":7,\"ts_ns\":99,\"kind\":\"wmi_cmd\",\"id\":1,\"len\":8,\"bytes_hex\":\"0100000000000000\"}\n";
        let records = parse_jsonl(input).unwrap();
        assert_eq!(records[0].timestamp_ns, 99);
        let out = verify_transcript(
            &records,
            |_, _| {
                Ok(Some(
                    Command::from_tlvs(CommandId(1), vec![1, 0, 0, 0]).unwrap(),
                ))
            },
            |_| Ok("unused"),
        );
        assert_eq!(
            out[0],
            Verification::CommandMismatch {
                seq: 7,
                id: 1,
                mismatch: ByteMismatch {
                    first_differing_offset: 4,
                    expected: Some(0),
                    actual: Some(1),
                    expected_len: 8,
                    actual_len: 8
                }
            }
        );
    }
    #[test]
    fn validates_event_through_caller_dispatch() {
        let records=parse_jsonl("{\"seq\":8,\"ts_ns\":100,\"kind\":\"wmi_event\",\"id\":2,\"len\":8,\"bytes_hex\":\"0200000000000000\"}").unwrap();
        assert_eq!(
            verify_transcript(&records, |_, _| Ok(None), |_| Ok("Ready"))[0],
            Verification::EventDecoded {
                seq: 8,
                id: 2,
                decoder: "Ready"
            }
        );
    }
    #[test]
    fn rejects_bad_hex_length_and_header() {
        assert_eq!(
            parse_jsonl(
                "{\"seq\":1,\"ts_ns\":0,\"kind\":\"wmi_cmd\",\"id\":1,\"len\":1,\"bytes_hex\":\"A0\"}"
            ),
            Err(GoldenError::InvalidHex)
        );
        assert!(matches!(
            parse_jsonl(
                "{\"seq\":1,\"ts_ns\":0,\"kind\":\"wmi_cmd\",\"id\":2,\"len\":4,\"bytes_hex\":\"01000000\"}"
            ),
            Err(GoldenError::IdentifierMismatch { .. })
        ));
    }
}
