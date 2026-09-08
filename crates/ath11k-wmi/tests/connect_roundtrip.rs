#![cfg(feature = "proptest")]

use ath11k_wmi::cmd::golden::reverse_map_semantic_command;
use ath11k_wmi::cmd::{
    CommandStrategy, EncodeCommand, PeerAssoc, PeerAuthorize, ScanStart, VdevCreate, VdevDown,
    VdevInstallKey, VdevStart, VdevUp,
};
use proptest::prelude::*;

fn assert_strict_round_trip(request: &impl EncodeCommand) {
    let encoded = request.encode_command().expect("generated request encodes");
    let reversed = reverse_map_semantic_command(encoded.id, encoded.tlvs())
        .unwrap_or_else(|error| panic!("strict reverse mapping failed: {error:?}; {encoded:?}"))
        .expect("connect-flow command has a semantic reverse mapper");
    let reencoded = reversed
        .encode_command()
        .expect("reverse-mapped request encodes");
    assert_eq!(reencoded, encoded);
}

proptest! {
    // These strategies populate every request field, including branches and
    // tail arrays that the native golden capture leaves at zero or empty.
    #[test]
    fn peer_assoc_round_trips(request in PeerAssoc::strategy()) {
        assert_strict_round_trip(&request);
    }

    #[test]
    fn install_key_round_trips(request in VdevInstallKey::strategy()) {
        assert_strict_round_trip(&request);
    }

    #[test]
    fn peer_authorize_round_trips(request in PeerAuthorize::strategy()) {
        assert_strict_round_trip(&request);
    }

    #[test]
    fn vdev_up_round_trips(request in VdevUp::strategy()) {
        assert_strict_round_trip(&request);
    }

    #[test]
    fn vdev_down_round_trips(request in VdevDown::strategy()) {
        assert_strict_round_trip(&request);
    }

    #[test]
    fn vdev_create_round_trips(request in VdevCreate::strategy()) {
        assert_strict_round_trip(&request);
    }

    #[test]
    fn vdev_start_and_restart_round_trip(request in VdevStart::strategy()) {
        assert_strict_round_trip(&request);
    }

    #[test]
    fn scan_start_round_trips(request in ScanStart::strategy()) {
        assert_strict_round_trip(&request);
    }
}
