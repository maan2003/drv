use ath11k_wmi::cmd::golden::{Verification, parse_jsonl, verify_transcript};
use ath11k_wmi::cmd::{EncodeCommand, VdevDelete};
use ath11k_wmi::{CommandId, WmiError};

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
fn validates_native_capture_when_present() {
    let path = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../artifacts/redwood-native-ath11k/wmi/ordered.jsonl"
    );
    let Ok(input) = std::fs::read_to_string(path) else {
        return;
    };
    let records = parse_jsonl(&input).expect("native WMI JSONL schema");
    let reports = verify_transcript(
        &records,
        |_id, _tlvs| Ok(None),
        ath11k_wmi::event::validate_known_event,
    );
    assert!(
        !reports
            .iter()
            .any(|r| matches!(r, Verification::DecodeFailed { .. }))
    );
}
