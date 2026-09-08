fn main() {
    for file in [
        "gain.h",
        "channel_strip.h",
        "sampler.h",
        "flags.h",
        "position_manager.h",
        "position_manager.cc",
    ] {
        println!("cargo:rerun-if-changed=upstream-cargo/src/media/audio/lib/processing/{file}");
    }
    println!("cargo:rerun-if-changed=src/bridge.cc");
    println!("cargo:rerun-if-changed=host-include/lib/stdcompat/span.h");
    println!("cargo:rerun-if-changed=host-include/lib/syslog/cpp/macros.h");
    println!("cargo:rerun-if-changed=host-include/lib/trace/event.h");
    println!("cargo:rerun-if-changed=host-include/ffl/string.h");
    println!("cargo:rerun-if-changed=host-include/src/media/audio/lib/format2/fixed.h");

    cc::Build::new()
        .cpp(true)
        .std("c++20")
        .opt_level(2)
        .include("upstream-cargo")
        .include("host-include")
        .file("src/bridge.cc")
        .file("upstream-cargo/src/media/audio/lib/processing/position_manager.cc")
        .compile("fuchsia_audio_processing");
}
