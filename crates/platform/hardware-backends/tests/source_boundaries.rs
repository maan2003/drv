// SPDX-License-Identifier: MIT OR Apache-2.0

use std::collections::BTreeSet;
use std::fs;
use std::path::Path;

fn collect_rs(path: &Path, out: &mut String) {
    if !path.is_dir() {
        return;
    }
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
fn raw_vfio_authority_stays_in_the_current_owner_crates() {
    let repository = Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(3)
        .unwrap();
    let mut actual = BTreeSet::new();
    let mut pending = vec![repository.join("crates"), repository.join("lab")];

    // Scan manifests on disk rather than cargo metadata: some guarded crates are
    // deliberately excluded from the top-level workspace.
    while let Some(crate_dir) = pending.pop() {
        let manifest_path = crate_dir.join("Cargo.toml");
        if !manifest_path.is_file() {
            for entry in fs::read_dir(&crate_dir).unwrap() {
                let path = entry.unwrap().path();
                if path.is_dir() && path.file_name().unwrap() != "target" {
                    pending.push(path);
                }
            }
            continue;
        }

        let manifest = fs::read_to_string(manifest_path).unwrap();
        let mut sources = String::new();
        collect_rs(&crate_dir.join("src"), &mut sources);
        let issues_raw_ioctls = sources.contains("ioctl")
            && (sources.contains("VFIO_")
                || sources.contains("IOMMUFD_")
                || sources.contains("IOMMU_IOAS_")
                || sources.contains("IOMMU_DESTROY"));

        if manifest.contains("userspace-vfio") || issues_raw_ioctls {
            actual.insert(crate_dir.file_name().unwrap().to_str().unwrap().to_owned());
        }
    }

    let expected = BTreeSet::from([
        // Shared backend owner; eng-65db owns its userspace-vfio dependency.
        "hardware-backends".to_owned(),
        // Shared raw VFIO/iommufd UAPI owner; eng-65db owns this crate.
        "userspace-vfio".to_owned(),
        // Seccomp owner imports the backend's exact request inventory and never issues ioctls.
        "linux-self-sandbox".to_owned(),
        // Preflight cdev/sysfs consumer; eng-xvq1 will review routing it through the backend.
        "ath11k-bringup".to_owned(),
        // Direct ioctl definitions in an unowned spike; manager: eng-k6ud.
        "amd-hda-spike".to_owned(),
    ]);

    assert_eq!(
        actual, expected,
        "the userspace-vfio/raw VFIO allowlist changed; new owners are forbidden and removed owners must be deleted from this allowlist"
    );
}
