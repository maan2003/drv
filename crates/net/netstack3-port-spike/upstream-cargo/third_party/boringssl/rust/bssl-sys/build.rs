use std::{env, fs, path::PathBuf};

fn main() {
    let crate_dir = PathBuf::from(env::var_os("CARGO_MANIFEST_DIR").unwrap());
    let fuchsia_root = crate_dir.join("../../../..");
    let boringssl = crate_dir.join("../../src");
    let include = boringssl.join("include");
    let out = PathBuf::from(env::var_os("OUT_DIR").unwrap());

    fs::copy(crate_dir.join("bindgen.rs"), out.join("bindgen.rs")).unwrap();
    cc::Build::new()
        .include(&fuchsia_root)
        .include(&include)
        .file(crate_dir.join("wrapper.c"))
        .compile("rust_wrapper");

    let built = cmake::Config::new(&boringssl)
        .build_target("crypto")
        .build();
    println!("cargo:rustc-link-search=native={}/build", built.display());
    println!("cargo:rustc-link-lib=static=crypto");
    println!("cargo:rustc-link-lib=stdc++");
    println!("cargo:rerun-if-changed={}", boringssl.display());
}
