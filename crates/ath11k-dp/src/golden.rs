// PORT-MAP: local-seam
//! Data-driven native HTT trace parsing and byte-exact verification.
//!
//! Capture tooling writes `artifacts/redwood-native-ath11k/htt/ordered.jsonl`.
//! The binary `native-trace.dat`, not `TP_printk`, is the upstream source.

use alloc::string::String;
use alloc::vec::Vec;

use crate::DpError;
use crate::rx::Wcn6750RxDescriptor;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TraceKind {
    Pktlog,
    PpduStats,
    RxDescriptor,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct GoldenRecord {
    pub seq: u64,
    pub timestamp_ns: u64,
    pub kind: TraceKind,
    pub declared_len: usize,
    pub bytes: Vec<u8>,
    pub checksum: Option<u64>,
    pub log_type: Option<u64>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum GoldenError {
    MissingField(&'static str),
    InvalidField(&'static str),
    InvalidHex,
    LengthMismatch { declared: usize, actual: usize },
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
    Exact {
        seq: u64,
        kind: TraceKind,
    },
    Unmapped {
        seq: u64,
        kind: TraceKind,
    },
    Mismatch {
        seq: u64,
        kind: TraceKind,
        mismatch: ByteMismatch,
    },
    RxDescriptorDecoded {
        seq: u64,
        log_type: u64,
    },
    DecodeFailed {
        seq: u64,
        kind: TraceKind,
        error: DpError,
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

fn optional_number(line: &str, key: &'static str) -> Result<Option<u64>, GoldenError> {
    if line.contains(&alloc::format!("\"{key}\"")) {
        number(line, key).map(Some)
    } else {
        Ok(None)
    }
}

fn decode_hex(value: &str) -> Result<Vec<u8>, GoldenError> {
    if !value.len().is_multiple_of(2)
        || value
            .bytes()
            .any(|byte| !byte.is_ascii_digit() && !(b'a'..=b'f').contains(&byte))
    {
        return Err(GoldenError::InvalidHex);
    }
    value
        .as_bytes()
        .chunks_exact(2)
        .map(|pair| {
            let digit = |byte: u8| {
                if byte <= b'9' {
                    byte - b'0'
                } else {
                    byte - b'a' + 10
                }
            };
            Ok((digit(pair[0]) << 4) | digit(pair[1]))
        })
        .collect()
}

pub fn parse_jsonl(input: &str) -> Result<Vec<GoldenRecord>, GoldenError> {
    input
        .lines()
        .filter(|line| !line.trim().is_empty())
        .map(|line| {
            let kind = match json_value(line, "kind")? {
                "htt_pktlog" => TraceKind::Pktlog,
                "htt_ppdu_stats" => TraceKind::PpduStats,
                "htt_rxdesc" => TraceKind::RxDescriptor,
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
            let checksum = optional_number(line, "checksum")?;
            let log_type = optional_number(line, "log_type")?;
            if kind == TraceKind::Pktlog && checksum.is_none() {
                return Err(GoldenError::MissingField("checksum"));
            }
            if kind == TraceKind::RxDescriptor && log_type.is_none() {
                return Err(GoldenError::MissingField("log_type"));
            }
            Ok(GoldenRecord {
                seq: number(line, "seq")?,
                timestamp_ns: number(line, "ts_ns")?,
                kind,
                declared_len,
                bytes,
                checksum,
                log_type,
            })
        })
        .collect()
}

pub fn first_difference(expected: &[u8], actual: &[u8]) -> Option<ByteMismatch> {
    let offset = expected
        .iter()
        .zip(actual)
        .position(|(expected, actual)| expected != actual)
        .or_else(|| (expected.len() != actual.len()).then_some(expected.len().min(actual.len())))?;
    Some(ByteMismatch {
        first_differing_offset: offset,
        expected: expected.get(offset).copied(),
        actual: actual.get(offset).copied(),
        expected_len: expected.len(),
        actual_len: actual.len(),
    })
}

/// Verifies the captured trace while keeping the deferred pktlog and PPDU
/// reverse-mapping policy at the caller. RX descriptors always pass through
/// the real WCN6750 descriptor parser.
pub fn verify_trace<F>(records: &[GoldenRecord], mut reencode: F) -> Vec<Verification>
where
    F: FnMut(&GoldenRecord) -> Result<Option<Vec<u8>>, DpError>,
{
    records
        .iter()
        .map(|record| {
            if record.kind == TraceKind::RxDescriptor {
                return match Wcn6750RxDescriptor::parse(&record.bytes) {
                    Ok(_) => Verification::RxDescriptorDecoded {
                        seq: record.seq,
                        log_type: record.log_type.unwrap_or_default(),
                    },
                    Err(error) => Verification::DecodeFailed {
                        seq: record.seq,
                        kind: record.kind,
                        error,
                    },
                };
            }
            match reencode(record) {
                Ok(None) => Verification::Unmapped {
                    seq: record.seq,
                    kind: record.kind,
                },
                Err(error) => Verification::DecodeFailed {
                    seq: record.seq,
                    kind: record.kind,
                    error,
                },
                Ok(Some(actual)) => match first_difference(&record.bytes, &actual) {
                    None => Verification::Exact {
                        seq: record.seq,
                        kind: record.kind,
                    },
                    Some(mismatch) => Verification::Mismatch {
                        seq: record.seq,
                        kind: record.kind,
                        mismatch,
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
    fn parses_capture_schema_and_reports_first_byte_difference() {
        let input = concat!(
            "{\"seq\":4,\"ts_ns\":99,\"kind\":\"htt_pktlog\",\"len\":2,\"bytes_hex\":\"00af\",\"checksum\":7}\n",
            "{\"seq\":5,\"ts_ns\":100,\"kind\":\"htt_ppdu_stats\",\"len\":0,\"bytes_hex\":\"\"}\n",
        );
        let records = parse_jsonl(input).unwrap();
        assert_eq!(records.len(), 2);
        assert_eq!(records[0].declared_len, 2);
        assert_eq!(
            first_difference(&records[0].bytes, &[0, 0xae]),
            Some(ByteMismatch {
                first_differing_offset: 1,
                expected: Some(0xaf),
                actual: Some(0xae),
                expected_len: 2,
                actual_len: 2,
            })
        );
    }

    #[test]
    fn verifies_exact_unmapped_and_real_rx_descriptor_decode() {
        let records = vec![
            GoldenRecord {
                seq: 1,
                timestamp_ns: 1,
                kind: TraceKind::Pktlog,
                declared_len: 1,
                bytes: vec![7],
                checksum: Some(3),
                log_type: None,
            },
            GoldenRecord {
                seq: 2,
                timestamp_ns: 2,
                kind: TraceKind::PpduStats,
                declared_len: 1,
                bytes: vec![8],
                checksum: None,
                log_type: None,
            },
            GoldenRecord {
                seq: 3,
                timestamp_ns: 3,
                kind: TraceKind::RxDescriptor,
                declared_len: 388,
                bytes: vec![0; 388],
                checksum: None,
                log_type: Some(9),
            },
        ];
        let reports = verify_trace(&records, |record| {
            Ok((record.kind == TraceKind::Pktlog).then(|| record.bytes.clone()))
        });
        assert_eq!(
            reports,
            vec![
                Verification::Exact {
                    seq: 1,
                    kind: TraceKind::Pktlog,
                },
                Verification::Unmapped {
                    seq: 2,
                    kind: TraceKind::PpduStats,
                },
                Verification::RxDescriptorDecoded {
                    seq: 3,
                    log_type: 9,
                },
            ]
        );
    }

    #[test]
    fn rejects_bad_hex_declared_length_and_missing_kind_metadata() {
        for (input, error) in [
            (
                "{\"seq\":1,\"ts_ns\":2,\"kind\":\"htt_ppdu_stats\",\"len\":1,\"bytes_hex\":\"0A\"}",
                GoldenError::InvalidHex,
            ),
            (
                "{\"seq\":1,\"ts_ns\":2,\"kind\":\"htt_ppdu_stats\",\"len\":2,\"bytes_hex\":\"00\"}",
                GoldenError::LengthMismatch {
                    declared: 2,
                    actual: 1,
                },
            ),
            (
                "{\"seq\":1,\"ts_ns\":2,\"kind\":\"htt_rxdesc\",\"len\":1,\"bytes_hex\":\"00\"}",
                GoldenError::MissingField("log_type"),
            ),
        ] {
            assert_eq!(parse_jsonl(input), Err(error));
        }
    }
}
