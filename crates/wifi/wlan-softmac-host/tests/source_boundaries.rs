// SPDX-License-Identifier: MIT OR Apache-2.0

use std::fs;
use std::path::Path;

fn collect_rs(path: &Path, out: &mut String) {
    for entry in fs::read_dir(path).unwrap() {
        let path = entry.unwrap().path();
        if path.is_dir() {
            collect_rs(&path, out);
        } else if path.extension().is_some_and(|extension| extension == "rs") {
            out.push_str(&fs::read_to_string(path).unwrap());
        }
    }
}

#[test]
fn host_sources_and_dependencies_are_chip_independent() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let manifest = fs::read_to_string(root.join("Cargo.toml")).unwrap();
    let mut sources = String::new();
    collect_rs(&root.join("src"), &mut sources);
    let owned = format!("{manifest}\n{sources}").to_ascii_lowercase();
    for chip_name in ["mt7921", "mt76", "ath11k", "wcn6750"] {
        assert!(
            !owned.contains(chip_name),
            "host boundary contains chip name {chip_name}"
        );
    }
    assert!(!manifest.contains("mt7921-port-spike"));
    assert!(!manifest.contains("mt7921-softmac-adapter"));
}

#[test]
fn compatibility_crates_depend_on_the_host_owner() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap();
    let manifest = root.join("drivers/mt7921/mt7921-softmac-adapter/Cargo.toml");
    assert!(
        fs::read_to_string(&manifest)
            .unwrap()
            .contains("wlan-softmac-host"),
        "{} does not consume the extracted owner",
        manifest.display()
    );
}

#[test]
fn device_contract_and_production_drivers_do_not_depend_on_protocol_execution() {
    let wifi = Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap();
    let contract = fs::read_to_string(wifi.join("wlan-softmac-class-support/Cargo.toml")).unwrap();
    let dependencies = contract.split("[dev-dependencies]").next().unwrap();
    for forbidden in [
        "wlan-softmac-host",
        "wlan-mlme =",
        "wlan-sme =",
        "netstack3",
        "tokio",
    ] {
        assert!(
            !dependencies.contains(forbidden),
            "contract depends on {forbidden}"
        );
    }
    for driver in [
        "drivers/mt7921/mt7921-production-client",
        "drivers/ath11k/ath11k-softmac-adapter",
    ] {
        let manifest = fs::read_to_string(wifi.join(driver).join("Cargo.toml")).unwrap();
        let dependencies = manifest.split("[dev-dependencies]").next().unwrap();
        assert!(dependencies.contains("wlan-softmac-class-support"));
        for forbidden in [
            "wlan-softmac-host",
            "wlan-mlme =",
            "wlan-sme =",
            "netstack3",
        ] {
            assert!(
                !dependencies.contains(forbidden),
                "{driver} depends on {forbidden}"
            );
        }
    }
}
