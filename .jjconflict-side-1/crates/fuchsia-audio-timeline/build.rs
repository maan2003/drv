fn main() {
    let root = "upstream-cargo";
    let timeline = "upstream-cargo/src/media/audio/lib/timeline";
    for file in [
        "timeline_function.cc",
        "timeline_function.h",
        "timeline_rate.cc",
        "timeline_rate.h",
    ] {
        println!("cargo:rerun-if-changed={timeline}/{file}");
    }
    println!("cargo:rerun-if-changed=src/bridge.cc");
    println!("cargo:rerun-if-changed=host-include/zircon/assert.h");
    println!("cargo:rerun-if-changed=host-include/lib/syslog/cpp/macros.h");

    cc::Build::new()
        .cpp(true)
        .std("c++17")
        .opt_level(1)
        // GCC 15 cannot prove the exhaustive enum switch initializes the
        // upstream `result`; Clang (Fuchsia's compiler) does not emit this.
        .flag_if_supported("-Wno-maybe-uninitialized")
        .include(root)
        .include("host-include")
        .file(format!("{timeline}/timeline_function.cc"))
        .file(format!("{timeline}/timeline_rate.cc"))
        .file("src/bridge.cc")
        .compile("fuchsia_audio_timeline");
}
