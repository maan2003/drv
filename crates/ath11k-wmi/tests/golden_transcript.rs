use ath11k_wmi::cmd::golden::{Verification, parse_jsonl, verify_transcript};
use ath11k_wmi::{Command, CommandId};

#[test]
fn ingests_ordered_jsonl_and_reports_each_message() {
    let records = parse_jsonl(include_str!("fixtures/wmi-ordered.sample.jsonl")).unwrap();
    let reports = verify_transcript(
        &records,
        |id, tlvs| {
            // Fixture models a mapped typed encoder's output. Real golden
            // registrations decode `tlvs` into the corresponding cmd request.
            (id == CommandId(0x5001))
                .then(|| Command::from_tlvs(id, tlvs.to_vec()))
                .transpose()
        },
        ath11k_wmi::event::validate_known_event,
    );
    assert_eq!(reports.len(), 2);
    assert_eq!(
        reports[0],
        Verification::CommandExact {
            seq: 41,
            id: 0x5001
        }
    );
    assert!(matches!(reports[1], Verification::EventDecoded { .. }));
}
