//! Data-driven native WMI transcript parsing and byte-exact verification.
use crate::{Command, CommandId, Event, EventId, WmiError};
use alloc::{string::String, vec::Vec};

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
