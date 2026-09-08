use ath11k_wmi::cmd::golden::{
    TranscriptKind, Verification, compare_reencoded, parse_jsonl, reverse_map_command_envelope,
    verify_transcript,
};
use ath11k_wmi::cmd::{EncodeCommand, VdevDelete};
use ath11k_wmi::{CommandId, WmiError};
use std::collections::BTreeMap;

#[test]
fn ingests_ordered_jsonl_and_reports_each_message() {
    let records = parse_jsonl(include_str!("fixtures/wmi-ordered.sample.jsonl")).unwrap();
    let reports = verify_transcript(
        &records,
        |id, tlvs| {
            if id != CommandId(0x5002) || tlvs.len() != 8 {
                return Ok(None);
            }
            let vdev_id =
                u32::from_le_bytes(tlvs[4..8].try_into().map_err(|_| WmiError::Malformed)?);
            VdevDelete { vdev_id }.encode_command().map(Some)
        },
        ath11k_wmi::event::validate_known_event,
    );
    assert_eq!(reports.len(), 2);
    assert_eq!(
        reports[0],
        Verification::CommandExact {
            seq: 41,
            id: 0x5002
        }
    );
    assert!(matches!(reports[1], Verification::EventDecoded { .. }));
}

#[test]
fn validates_native_event_decoders_and_command_envelopes_when_present() {
    let path = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../artifacts/redwood-native-ath11k/20260908T093708Z/wmi/ordered.jsonl"
    );
    let Ok(input) = std::fs::read_to_string(path) else {
        return;
    };
    let records = parse_jsonl(&input).expect("native WMI JSONL schema");
    let reports = verify_transcript(
        &records,
        |id, tlvs| {
            reverse_map_command_envelope(id, tlvs)
                .and_then(|request| request.map(|request| request.encode_command()).transpose())
        },
        ath11k_wmi::event::validate_known_event,
    );
    let failures: Vec<_> = reports
        .iter()
        .filter(|r| matches!(r, Verification::DecodeFailed { .. }))
        .collect();
    let unknown: Vec<_> = reports
        .iter()
        .filter(|r| {
            matches!(
                r,
                Verification::EventDecoded {
                    decoder: "opaque/unknown",
                    ..
                }
            )
        })
        .collect();
    assert!(
        failures.is_empty(),
        "native event decode failures: {failures:#?}"
    );
    assert_eq!(
        unknown.len(),
        6,
        "review newly typed or newly unknown native events"
    );
    assert_eq!(
        reports
            .iter()
            .filter(|report| matches!(report, Verification::EventDecoded { .. }))
            .count(),
        1_486
    );

    #[derive(Default)]
    struct FamilyResult {
        matched: usize,
        mismatched: Vec<String>,
        masked_fields: &'static [&'static str],
    }
    let mut families: BTreeMap<&'static str, FamilyResult> = BTreeMap::new();
    for record in records
        .iter()
        .filter(|record| record.kind == TranscriptKind::Command)
    {
        let (_, tlvs) = record.envelope_parts().expect("validated envelope");
        let request = reverse_map_command_envelope(CommandId(record.id), tlvs)
            .unwrap_or_else(|error| panic!("seq {} id {:#x}: {error:?}", record.seq, record.id))
            .unwrap_or_else(|| panic!("unmapped command id {:#x}", record.id));
        let command = request.encode_command().expect("re-encode native request");
        let mut actual = (command.id.0 & 0x00ff_ffff).to_le_bytes().to_vec();
        actual.extend_from_slice(command.tlvs());
        let result = families.entry(request.family).or_default();
        result.masked_fields = request.masked_fields;
        if let Some(mismatch) = compare_reencoded(&record.bytes, &actual, &request.masked_ranges) {
            result.mismatched.push(format!(
                "seq {} first offset {} native {:?} rust {:?} (lengths {}/{})",
                record.seq,
                mismatch.first_differing_offset,
                mismatch.expected,
                mismatch.actual,
                mismatch.expected_len,
                mismatch.actual_len
            ));
        } else {
            result.matched += 1;
        }
    }
    eprintln!("family | matched | mismatched | masked fields");
    for (family, result) in &families {
        eprintln!(
            "{family} | {} | {} | {}",
            result.matched,
            result.mismatched.len(),
            if result.masked_fields.is_empty() {
                "none".to_string()
            } else {
                result.masked_fields.join(", ")
            }
        );
        assert!(
            result.mismatched.is_empty(),
            "{family} mismatches: {:#?}",
            result.mismatched
        );
    }
    assert_eq!(families.len(), 30, "review newly observed command families");
    assert_eq!(
        families
            .values()
            .map(|result| result.matched)
            .sum::<usize>(),
        411
    );
}
