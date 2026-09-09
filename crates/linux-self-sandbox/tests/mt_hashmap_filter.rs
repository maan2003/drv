#![cfg(feature = "filter-integration-test")]

use std::os::unix::process::ExitStatusExt;
use std::process::Command;

#[test]
fn mt_hashmap_random_state_is_initialized_before_the_fatal_filter() {
    let probe = env!("CARGO_BIN_EXE_mt-hashmap-filter-probe");
    let cold = Command::new(probe).arg("cold").status().unwrap();
    assert_eq!(cold.signal(), Some(libc::SIGSYS));

    let prewarmed = Command::new(probe).arg("prewarmed").status().unwrap();
    assert!(prewarmed.success(), "prewarmed probe failed: {prewarmed}");
}
