//! Deterministic alignment of a native WMI oracle and a bring-up-runner trace.
//!
//! Both inputs use the tracepoint JSONL schema parsed by [`parse_jsonl`].
//! Sequence numbers and timestamps are diagnostic metadata, not equality
//! inputs. Since the runner schema deliberately has no phase markers, phases
//! begin at the first VDEV_CREATE, START_SCAN, and PEER_CREATE commands. A run
//! that stops after passive scan therefore reports the oracle's connect phase
//! as missing rather than assigning it to the scan phase.

use super::golden::{
    ByteMismatch, GoldenError, TranscriptKind, TranscriptRecord, compare_reencoded, parse_jsonl,
    reverse_map_command_envelope,
};
use crate::CommandId;
use alloc::{format, string::String, vec, vec::Vec};
use core::fmt::Write;

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub enum BringupPhase {
    BootThroughServiceReady,
    VdevCreateStart,
    Scan,
    Connect,
}

impl BringupPhase {
    pub const ALL: [Self; 4] = [
        Self::BootThroughServiceReady,
        Self::VdevCreateStart,
        Self::Scan,
        Self::Connect,
    ];

    pub const fn name(self) -> &'static str {
        match self {
            Self::BootThroughServiceReady => "boot-through-service-ready",
            Self::VdevCreateStart => "vdev-create-start",
            Self::Scan => "scan",
            Self::Connect => "connect",
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CommandAlignmentKind {
    Exact,
    Masked,
    Mismatched,
    Missing,
    Extra,
    Reordered,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CommandAlignment {
    pub kind: CommandAlignmentKind,
    pub id: u32,
    pub expected_seq: Option<u64>,
    pub seen_seq: Option<u64>,
    pub mismatch: Option<ByteMismatch>,
    pub masked_fields: &'static [&'static str],
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct EventAlignment {
    pub expected: usize,
    pub seen: usize,
    pub aligned: usize,
    pub missing: Vec<(u64, u32)>,
    pub extra: Vec<(u64, u32)>,
    pub reordered: Vec<(u64, u64, u32)>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PhaseComparison {
    pub phase: BringupPhase,
    pub commands: Vec<CommandAlignment>,
    pub events: EventAlignment,
}

impl PhaseComparison {
    pub fn command_count(&self, kind: CommandAlignmentKind) -> usize {
        self.commands
            .iter()
            .filter(|item| item.kind == kind)
            .count()
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TranscriptComparison {
    pub phases: Vec<PhaseComparison>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ComparisonError {
    Native(GoldenError),
    Runner(GoldenError),
}

pub fn compare_jsonl(native: &str, runner: &str) -> Result<TranscriptComparison, ComparisonError> {
    let native = parse_jsonl(native).map_err(ComparisonError::Native)?;
    let runner = parse_jsonl(runner).map_err(ComparisonError::Runner)?;
    Ok(compare_transcripts(&native, &runner))
}

pub fn compare_transcripts(
    native: &[TranscriptRecord],
    runner: &[TranscriptRecord],
) -> TranscriptComparison {
    let native = partition(native);
    let runner = partition(runner);
    let phases = BringupPhase::ALL
        .into_iter()
        .enumerate()
        .map(|(index, phase)| PhaseComparison {
            phase,
            commands: compare_commands(&native[index], &runner[index]),
            events: compare_events(&native[index], &runner[index]),
        })
        .collect();
    TranscriptComparison { phases }
}

fn partition(records: &[TranscriptRecord]) -> [Vec<&TranscriptRecord>; 4] {
    let mut phases: [Vec<&TranscriptRecord>; 4] = core::array::from_fn(|_| Vec::new());
    let mut phase = BringupPhase::BootThroughServiceReady;
    for record in records {
        if record.kind == TranscriptKind::Command {
            phase = match record.id {
                0x5001 if phase == BringupPhase::BootThroughServiceReady => {
                    BringupPhase::VdevCreateStart
                }
                0x3001 if phase <= BringupPhase::VdevCreateStart => BringupPhase::Scan,
                0x6001 if phase <= BringupPhase::Scan => BringupPhase::Connect,
                _ => phase,
            };
        }
        phases[phase as usize].push(record);
    }
    phases
}

fn records_of_kind<'a>(
    records: &'a [&'a TranscriptRecord],
    kind: TranscriptKind,
) -> Vec<&'a TranscriptRecord> {
    records
        .iter()
        .copied()
        .filter(|record| record.kind == kind)
        .collect()
}

fn alignment_pairs(
    expected: &[&TranscriptRecord],
    seen: &[&TranscriptRecord],
    score: impl Fn(&TranscriptRecord, &TranscriptRecord) -> usize,
) -> Vec<(usize, usize)> {
    let width = seen.len() + 1;
    let Some(size) = (expected.len() + 1).checked_mul(width) else {
        return Vec::new();
    };
    let mut table = vec![0usize; size];
    for i in 0..expected.len() {
        for j in 0..seen.len() {
            table[(i + 1) * width + j + 1] = (table[i * width + j] + score(expected[i], seen[j]))
                .max(table[i * width + j + 1])
                .max(table[(i + 1) * width + j]);
        }
    }
    let (mut i, mut j) = (expected.len(), seen.len());
    let mut pairs = Vec::new();
    while i != 0 && j != 0 {
        // On a tied optimum, discard the later suffix before taking a
        // diagonal. This aligns repeated IDs earliest-to-earliest, so a
        // truncated runner reports the missing native tail rather than an
        // apparently missing prefix.
        if table[(i - 1) * width + j] == table[i * width + j] {
            i -= 1;
            continue;
        }
        if table[i * width + j - 1] == table[i * width + j] {
            j -= 1;
            continue;
        }
        let diagonal_score = score(expected[i - 1], seen[j - 1]);
        if diagonal_score != 0
            && table[i * width + j] == table[(i - 1) * width + j - 1] + diagonal_score
        {
            pairs.push((i - 1, j - 1));
            i -= 1;
            j -= 1;
        } else if table[(i - 1) * width + j] >= table[i * width + j - 1] {
            i -= 1;
        } else {
            j -= 1;
        }
    }
    pairs.reverse();
    pairs
}

fn command_discriminator(record: &TranscriptRecord) -> Option<u32> {
    let offset = match record.id {
        // pdev/vdev and station power-save parameter commands all encode the
        // parameter ID as their second fixed word.
        0x4003 | 0x5008 | 0x9002 => 12,
        // PeerSetParam follows vdev_id and the padded peer MAC.
        0x6004 => 20,
        _ => return None,
    };
    let bytes = record.bytes.get(offset..offset + 4)?;
    Some(u32::from_le_bytes(bytes.try_into().ok()?))
}

fn command_score(expected: &TranscriptRecord, seen: &TranscriptRecord) -> usize {
    if expected.id != seen.id {
        return 0;
    }
    if expected.bytes == seen.bytes {
        return 5;
    }
    let masks = expected
        .envelope_parts()
        .ok()
        .and_then(|(_, tlvs)| {
            reverse_map_command_envelope(CommandId(expected.id), tlvs)
                .ok()
                .flatten()
        })
        .map_or_else(Vec::new, |request| request.masked_ranges);
    if !masks.is_empty() && compare_reencoded(&expected.bytes, &seen.bytes, &masks).is_none() {
        4
    } else if command_discriminator(expected).is_some()
        && command_discriminator(expected) == command_discriminator(seen)
    {
        3
    } else {
        1
    }
}

fn unmatched_and_reordered(
    expected: &[&TranscriptRecord],
    seen: &[&TranscriptRecord],
    pairs: &[(usize, usize)],
) -> (Vec<(usize, usize)>, Vec<usize>, Vec<usize>) {
    let mut expected_used = vec![false; expected.len()];
    let mut seen_used = vec![false; seen.len()];
    for &(i, j) in pairs {
        expected_used[i] = true;
        seen_used[j] = true;
    }
    let mut reordered = Vec::new();
    for i in 0..expected.len() {
        if expected_used[i] {
            continue;
        }
        if let Some(j) = (0..seen.len()).find(|&j| !seen_used[j] && expected[i].id == seen[j].id) {
            expected_used[i] = true;
            seen_used[j] = true;
            reordered.push((i, j));
        }
    }
    let missing = expected_used
        .iter()
        .enumerate()
        .filter_map(|(index, used)| (!used).then_some(index))
        .collect();
    let extra = seen_used
        .iter()
        .enumerate()
        .filter_map(|(index, used)| (!used).then_some(index))
        .collect();
    (reordered, missing, extra)
}

fn compare_commands(
    expected_phase: &[&TranscriptRecord],
    seen_phase: &[&TranscriptRecord],
) -> Vec<CommandAlignment> {
    let expected = records_of_kind(expected_phase, TranscriptKind::Command);
    let seen = records_of_kind(seen_phase, TranscriptKind::Command);
    let pairs = alignment_pairs(&expected, &seen, command_score);
    let (reordered, missing, extra) = unmatched_and_reordered(&expected, &seen, &pairs);
    let mut output = Vec::new();
    for (i, j) in pairs {
        let expected_record = expected[i];
        let seen_record = seen[j];
        let mapped = expected_record.envelope_parts().ok().and_then(|(_, tlvs)| {
            reverse_map_command_envelope(CommandId(expected_record.id), tlvs)
                .ok()
                .flatten()
        });
        let masked_fields = mapped
            .as_ref()
            .map_or(&[] as &'static [&'static str], |request| {
                request.masked_fields
            });
        let masks = mapped
            .as_ref()
            .map_or(&[][..], |request| request.masked_ranges.as_slice());
        let raw_mismatch =
            super::golden::first_difference(&expected_record.bytes, &seen_record.bytes);
        let masked_mismatch = compare_reencoded(&expected_record.bytes, &seen_record.bytes, masks);
        let (kind, mismatch) = if raw_mismatch.is_none() {
            (CommandAlignmentKind::Exact, None)
        } else if !masks.is_empty() && masked_mismatch.is_none() {
            (CommandAlignmentKind::Masked, None)
        } else {
            (CommandAlignmentKind::Mismatched, masked_mismatch)
        };
        output.push(CommandAlignment {
            kind,
            id: expected_record.id,
            expected_seq: Some(expected_record.seq),
            seen_seq: Some(seen_record.seq),
            mismatch,
            masked_fields,
        });
    }
    for (i, j) in reordered {
        output.push(CommandAlignment {
            kind: CommandAlignmentKind::Reordered,
            id: expected[i].id,
            expected_seq: Some(expected[i].seq),
            seen_seq: Some(seen[j].seq),
            mismatch: None,
            masked_fields: &[],
        });
    }
    for i in missing {
        output.push(CommandAlignment {
            kind: CommandAlignmentKind::Missing,
            id: expected[i].id,
            expected_seq: Some(expected[i].seq),
            seen_seq: None,
            mismatch: None,
            masked_fields: &[],
        });
    }
    for j in extra {
        output.push(CommandAlignment {
            kind: CommandAlignmentKind::Extra,
            id: seen[j].id,
            expected_seq: None,
            seen_seq: Some(seen[j].seq),
            mismatch: None,
            masked_fields: &[],
        });
    }
    output.sort_by_key(|item| {
        (
            item.expected_seq.unwrap_or(u64::MAX),
            item.seen_seq.unwrap_or(u64::MAX),
        )
    });
    output
}

fn compare_events(
    expected_phase: &[&TranscriptRecord],
    seen_phase: &[&TranscriptRecord],
) -> EventAlignment {
    let expected = records_of_kind(expected_phase, TranscriptKind::Event);
    let seen = records_of_kind(seen_phase, TranscriptKind::Event);
    let pairs = alignment_pairs(&expected, &seen, |expected, seen| {
        usize::from(expected.id == seen.id)
    });
    let (reordered, missing, extra) = unmatched_and_reordered(&expected, &seen, &pairs);
    EventAlignment {
        expected: expected.len(),
        seen: seen.len(),
        aligned: pairs.len(),
        missing: missing
            .into_iter()
            .map(|i| (expected[i].seq, expected[i].id))
            .collect(),
        extra: extra
            .into_iter()
            .map(|j| (seen[j].seq, seen[j].id))
            .collect(),
        reordered: reordered
            .into_iter()
            .map(|(i, j)| (expected[i].seq, seen[j].seq, expected[i].id))
            .collect(),
    }
}

impl TranscriptComparison {
    /// Stable line-oriented output suitable for run artifacts and diffs.
    pub fn deterministic_report(&self) -> String {
        let mut output = String::new();
        for phase in &self.phases {
            let _ = writeln!(output, "phase {}", phase.phase.name());
            let _ = writeln!(
                output,
                "  commands exact={} masked={} mismatched={} missing={} extra={} reordered={}",
                phase.command_count(CommandAlignmentKind::Exact),
                phase.command_count(CommandAlignmentKind::Masked),
                phase.command_count(CommandAlignmentKind::Mismatched),
                phase.command_count(CommandAlignmentKind::Missing),
                phase.command_count(CommandAlignmentKind::Extra),
                phase.command_count(CommandAlignmentKind::Reordered),
            );
            for item in &phase.commands {
                let _ = writeln!(
                    output,
                    "    {:?} id={:#08x} expected_seq={} seen_seq={}{}{}",
                    item.kind,
                    item.id,
                    item.expected_seq
                        .map_or_else(|| "-".into(), |seq| format!("{seq}")),
                    item.seen_seq
                        .map_or_else(|| "-".into(), |seq| format!("{seq}")),
                    if item.masked_fields.is_empty() {
                        "".into()
                    } else {
                        format!(" masks={}", item.masked_fields.join(","))
                    },
                    item.mismatch
                        .as_ref()
                        .map_or_else(String::new, |mismatch| format!(
                            " first_difference={}",
                            mismatch.first_differing_offset
                        )),
                );
            }
            let _ = writeln!(
                output,
                "  events expected={} seen={} aligned={} missing={} extra={} reordered={}",
                phase.events.expected,
                phase.events.seen,
                phase.events.aligned,
                phase.events.missing.len(),
                phase.events.extra.len(),
                phase.events.reordered.len(),
            );
            for &(seq, id) in &phase.events.missing {
                let _ = writeln!(output, "    Missing event id={id:#08x} expected_seq={seq}");
            }
            for &(seq, id) in &phase.events.extra {
                let _ = writeln!(output, "    Extra event id={id:#08x} seen_seq={seq}");
            }
            for &(expected_seq, seen_seq, id) in &phase.events.reordered {
                let _ = writeln!(
                    output,
                    "    Reordered event id={id:#08x} expected_seq={expected_seq} seen_seq={seen_seq}"
                );
            }
        }
        output
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cmd::{EncodeCommand, MgmtSend, PeerReorderQueueSetup};

    fn record(seq: u64, kind: TranscriptKind, id: u32, bytes: Vec<u8>) -> TranscriptRecord {
        TranscriptRecord {
            seq,
            timestamp_ns: seq * 10,
            kind,
            id,
            declared_len: bytes.len(),
            bytes,
        }
    }

    fn envelope(id: u32, tail: &[u8]) -> Vec<u8> {
        let mut bytes = (id & 0x00ff_ffff).to_le_bytes().to_vec();
        bytes.extend_from_slice(tail);
        bytes
    }

    fn command_record(seq: u64, id: u32, tail: &[u8]) -> TranscriptRecord {
        record(seq, TranscriptKind::Command, id, envelope(id, tail))
    }

    fn event_record(seq: u64, id: u32) -> TranscriptRecord {
        record(seq, TranscriptKind::Event, id, envelope(id, &[]))
    }

    fn mgmt(seq: u64, paddr: u64, fill: u8) -> TranscriptRecord {
        let command = MgmtSend {
            vdev_id: 0,
            desc_id: 7,
            channel_freq: 0,
            paddr,
            frame: vec![fill; 80],
            tx_params_valid: false,
        }
        .encode_command()
        .unwrap();
        command_record(seq, command.id.0, command.tlvs())
    }

    #[test]
    fn synthetic_pair_classifies_alignment_and_masks() {
        let native = vec![
            event_record(1, 3),
            command_record(2, 0x5001, &[]),
            command_record(3, 0x5003, &[]),
            command_record(4, 0x5008, &[]),
            command_record(5, 0x3001, &[]),
            mgmt(6, 0x1122_3344_5566_7788, 0xaa),
            command_record(7, 0x3006, &[1]),
            event_record(8, 0x3002),
            command_record(9, 0x6001, &[]),
        ];
        let runner = vec![
            event_record(10, 3),
            command_record(11, 0x5001, &[]),
            command_record(12, 0x5008, &[]),
            command_record(13, 0x5003, &[]),
            command_record(14, 0x3001, &[]),
            mgmt(15, 0x8877_6655_4433_2211, 0xbb),
            command_record(16, 0x3006, &[2]),
            command_record(17, 0x3007, &[]),
            event_record(18, 0x3003),
        ];
        let report = compare_transcripts(&native, &runner);
        let vdev = &report.phases[BringupPhase::VdevCreateStart as usize];
        assert_eq!(vdev.command_count(CommandAlignmentKind::Exact), 2);
        assert_eq!(vdev.command_count(CommandAlignmentKind::Reordered), 1);
        let scan = &report.phases[BringupPhase::Scan as usize];
        assert_eq!(scan.command_count(CommandAlignmentKind::Exact), 1);
        assert_eq!(scan.command_count(CommandAlignmentKind::Masked), 1);
        assert_eq!(scan.command_count(CommandAlignmentKind::Mismatched), 1);
        assert_eq!(scan.command_count(CommandAlignmentKind::Extra), 1);
        assert_eq!(scan.events.missing, vec![(8, 0x3002)]);
        assert_eq!(scan.events.extra, vec![(18, 0x3003)]);
        let connect = &report.phases[BringupPhase::Connect as usize];
        assert_eq!(connect.command_count(CommandAlignmentKind::Missing), 1);
        let text = report.deterministic_report();
        assert_eq!(
            text,
            "phase boot-through-service-ready\n  commands exact=0 masked=0 mismatched=0 missing=0 extra=0 reordered=0\n  events expected=1 seen=1 aligned=1 missing=0 extra=0 reordered=0\nphase vdev-create-start\n  commands exact=2 masked=0 mismatched=0 missing=0 extra=0 reordered=1\n    Exact id=0x005001 expected_seq=2 seen_seq=11\n    Exact id=0x005003 expected_seq=3 seen_seq=13\n    Reordered id=0x005008 expected_seq=4 seen_seq=12\n  events expected=0 seen=0 aligned=0 missing=0 extra=0 reordered=0\nphase scan\n  commands exact=1 masked=1 mismatched=1 missing=0 extra=1 reordered=0\n    Exact id=0x003001 expected_seq=5 seen_seq=14\n    Masked id=0x007008 expected_seq=6 seen_seq=15 masks=paddr,frame\n    Mismatched id=0x003006 expected_seq=7 seen_seq=16 first_difference=4\n    Extra id=0x003007 expected_seq=- seen_seq=17\n  events expected=1 seen=1 aligned=0 missing=1 extra=1 reordered=0\n    Missing event id=0x003002 expected_seq=8\n    Extra event id=0x003003 seen_seq=18\nphase connect\n  commands exact=0 masked=0 mismatched=0 missing=1 extra=0 reordered=0\n    Missing id=0x006001 expected_seq=9 seen_seq=-\n  events expected=0 seen=0 aligned=0 missing=0 extra=0 reordered=0\n"
        );
    }

    #[test]
    fn repeated_parameter_commands_align_by_parameter_id() {
        let set_param = |seq: u64, parameter: u32, value: u32| {
            let mut payload = Vec::new();
            payload.extend_from_slice(&((0x5fu32 << 16) | 12).to_le_bytes());
            payload.extend_from_slice(&0u32.to_le_bytes());
            payload.extend_from_slice(&parameter.to_le_bytes());
            payload.extend_from_slice(&value.to_le_bytes());
            command_record(seq, 0x5008, &payload)
        };
        let native = [set_param(1, 0x30, 1), set_param(2, 0x22, 2)];
        let runner = [set_param(9, 0x22, 1)];
        let report = compare_transcripts(&native, &runner);
        let boot = &report.phases[BringupPhase::BootThroughServiceReady as usize];
        assert_eq!(boot.commands[0].kind, CommandAlignmentKind::Missing);
        assert_eq!(boot.commands[0].expected_seq, Some(1));
        assert_eq!(boot.commands[1].kind, CommandAlignmentKind::Mismatched);
        assert_eq!(boot.commands[1].expected_seq, Some(2));
        assert_eq!(boot.commands[1].seen_seq, Some(9));
    }

    #[test]
    fn reorder_queue_dma_address_uses_documented_mask() {
        let make = |seq, queue_address| {
            let command = PeerReorderQueueSetup {
                vdev_id: 1,
                peer_addr: [1, 2, 3, 4, 5, 6],
                tid: 3,
                queue_address,
                ba_window_size_valid: 1,
                ba_window_size: 64,
            }
            .encode_command()
            .unwrap();
            command_record(seq, command.id.0, command.tlvs())
        };
        let report = compare_transcripts(
            &[make(1, 0x1122_3344_5566_7788)],
            &[make(9, 0x8877_6655_4433_2211)],
        );
        let boot = &report.phases[BringupPhase::BootThroughServiceReady as usize];
        assert_eq!(boot.command_count(CommandAlignmentKind::Masked), 1);
        assert_eq!(boot.commands[0].masked_fields, &["queue_address"]);
    }

    #[test]
    fn repeated_id_prefix_aligns_earliest_and_reports_missing_tail() {
        let native = vec![
            command_record(1, 0x111, &[]),
            event_record(2, 0x222),
            command_record(3, 0x111, &[]),
            event_record(4, 0x222),
        ];
        let runner = vec![command_record(10, 0x111, &[]), event_record(11, 0x222)];
        let report = compare_transcripts(&native, &runner);
        let boot = &report.phases[BringupPhase::BootThroughServiceReady as usize];
        assert_eq!(boot.commands[0].kind, CommandAlignmentKind::Exact);
        assert_eq!(boot.commands[0].expected_seq, Some(1));
        assert_eq!(boot.commands[0].seen_seq, Some(10));
        assert_eq!(boot.commands[1].kind, CommandAlignmentKind::Missing);
        assert_eq!(boot.commands[1].expected_seq, Some(3));
        assert_eq!(boot.events.aligned, 1);
        assert_eq!(boot.events.missing, vec![(4, 0x222)]);
    }

    #[test]
    fn json_comparison_ignores_timestamps_and_sequence_numbers() {
        let native = "{\"seq\":1,\"ts_ns\":2,\"kind\":\"wmi_event\",\"id\":3,\"len\":4,\"bytes_hex\":\"03000000\"}\n";
        let runner = "{\"seq\":99,\"ts_ns\":999,\"kind\":\"wmi_event\",\"id\":3,\"len\":4,\"bytes_hex\":\"03000000\"}\n";
        let report = compare_jsonl(native, runner).unwrap();
        assert_eq!(report.phases[0].events.aligned, 1);
        assert!(report.phases[0].events.missing.is_empty());
        assert!(report.phases[0].events.extra.is_empty());
    }
}
