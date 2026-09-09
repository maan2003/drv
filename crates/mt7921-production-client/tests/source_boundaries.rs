// SPDX-License-Identifier: GPL-2.0-only

#[test]
fn hardware_containment_never_leaks_or_suppresses_owner_drop() {
    fn collect_rs(path: &std::path::Path, source: &mut String) {
        for entry in std::fs::read_dir(path).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                collect_rs(&path, source);
            } else if path.extension().is_some_and(|extension| extension == "rs") {
                source.push_str(&std::fs::read_to_string(path).unwrap());
            }
        }
    }
    let mut source = String::new();
    collect_rs(
        &std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src"),
        &mut source,
    );
    for forbidden in [
        "mem::forget",
        "ManuallyDrop",
        "Box::leak",
        "process::exit",
        "_exit",
    ] {
        assert!(
            !source.contains(forbidden),
            "MT7921 hardware containment must not use {forbidden}"
        );
    }
}
