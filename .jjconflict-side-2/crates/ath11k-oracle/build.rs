use std::env;
use std::path::{Path, PathBuf};

const COMMIT: &str = "509ce3d952d550f93b544c8d94c99e798f09a9b4";

fn main() {
    let manifest = PathBuf::from(env::var_os("CARGO_MANIFEST_DIR").unwrap());
    let root = env::var_os("ATH11K_REFERENCE_DIR").map(PathBuf::from).unwrap_or_else(|| {
        manifest.join("../../result-ath11k/reference").join(format!("linux-{COMMIT}"))
    });
    let codec = root.join("drivers/soc/qcom/qmi_encdec.c");
    if !codec.is_file() {
        panic!("pinned ath11k source is missing at {}; run `nix build .#ath11k-reference-source --out-link result-ath11k` or set ATH11K_REFERENCE_DIR", root.display());
    }
    assert_commit(&root);
    cc::Build::new()
        .file(codec)
        .file(manifest.join("c/oracle.c"))
        .include(manifest.join("stubs"))
        .warnings(true)
        .flag_if_supported("-std=gnu11")
        .compile("ath11k_c_oracle");
    println!("cargo:rerun-if-env-changed=ATH11K_REFERENCE_DIR");
    println!("cargo:rerun-if-changed=c/oracle.c");
    println!("cargo:rerun-if-changed=stubs");
    println!("cargo:rerun-if-changed={}", root.join("COMMIT").display());
}

fn assert_commit(root: &Path) {
    let actual = std::fs::read_to_string(root.join("COMMIT")).expect("reference must contain COMMIT");
    assert_eq!(actual.trim(), COMMIT, "wrong ath11k C reference commit");
}
