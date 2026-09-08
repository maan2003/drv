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
    let generated = generate_qmi_oracle(&root, &manifest);
    cc::Build::new()
        .file(codec)
        .file(generated)
        .file(manifest.join("c/wmi_oracle.c"))
        .include(manifest.join("stubs"))
        .warnings(true)
        .flag_if_supported("-std=gnu11")
        .compile("ath11k_c_oracle");
    println!("cargo:rerun-if-env-changed=ATH11K_REFERENCE_DIR");
    println!("cargo:rerun-if-changed=c/oracle.c");
    println!("cargo:rerun-if-changed=c/wmi_oracle.c");
    println!("cargo:rerun-if-changed=stubs");
    println!("cargo:rerun-if-changed={}", root.join("COMMIT").display());
}

fn generate_qmi_oracle(root: &Path, manifest: &Path) -> PathBuf {
    let driver = root.join("drivers/net/wireless/ath/ath11k");
    let header = std::fs::read_to_string(driver.join("qmi.h")).expect("read pinned qmi.h");
    let source = std::fs::read_to_string(driver.join("qmi.c")).expect("read pinned qmi.c");
    let structs = between(
        &header,
        "#define QMI_WLANFW_HOST_CAP_REQ_MSG_V01_MAX_LEN",
        "int ath11k_qmi_firmware_start",
    );
    let tables = between(
        &source,
        "static const struct qmi_elem_info qmi_wlanfw_host_cap_req_msg_v01_ei[]",
        "/* clang stack usage explodes if this is inlined */",
    );
    let wrappers = std::fs::read_to_string(manifest.join("c/oracle.c")).expect("read oracle.c");
    let generated = PathBuf::from(env::var_os("OUT_DIR").unwrap()).join("qmi_oracle.c");
    let prelude = r#"#include <linux/kernel.h>
#include <linux/slab.h>
#include <linux/soc/qcom/qmi.h>
#include <stddef.h>
#include <string.h>
#define ATH11K_QMI_WLANFW_MAX_TIMESTAMP_LEN_V01 32
#define ATH11K_QMI_WLANFW_MAX_BUILD_ID_LEN_V01 128
#define ATH11K_QMI_WLANFW_MAX_NUM_MEM_SEG_V01 52
#define QMI_WLANFW_MAX_DATA_SIZE_V01 6144
"#;
    std::fs::write(generated.as_path(), format!("{prelude}\n{structs}\n{tables}\n{wrappers}"))
        .expect("write generated QMI oracle translation unit");
    generated
}

fn between<'a>(text: &'a str, start: &str, end: &str) -> &'a str {
    let start = text.find(start).unwrap_or_else(|| panic!("pinned source lost marker {start}"));
    let end = text[start..]
        .find(end)
        .map(|offset| start + offset)
        .unwrap_or_else(|| panic!("pinned source lost marker {end}"));
    &text[start..end]
}

fn assert_commit(root: &Path) {
    let actual = std::fs::read_to_string(root.join("COMMIT")).expect("reference must contain COMMIT");
    assert_eq!(actual.trim(), COMMIT, "wrong ath11k C reference commit");
}
