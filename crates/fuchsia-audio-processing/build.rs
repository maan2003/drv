fn main() {
    for file in ["gain.h", "channel_strip.h", "sampler.h"] {
        println!("cargo:rerun-if-changed=upstream-cargo/src/media/audio/lib/processing/{file}");
    }
    println!("cargo:rerun-if-changed=src/bridge.cc");
    println!("cargo:rerun-if-changed=host-include/lib/stdcompat/span.h");
    println!("cargo:rerun-if-changed=host-include/lib/syslog/cpp/macros.h");

    cc::Build::new()
        .cpp(true)
        .std("c++20")
        .opt_level(2)
        .include("upstream-cargo")
        .include("host-include")
        .file("src/bridge.cc")
        .compile("fuchsia_audio_processing");
}
