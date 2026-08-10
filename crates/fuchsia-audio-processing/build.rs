fn main() {
    let gain = "upstream-cargo/src/media/audio/lib/processing/gain.h";
    println!("cargo:rerun-if-changed={gain}");
    println!("cargo:rerun-if-changed=src/bridge.cc");

    cc::Build::new()
        .cpp(true)
        .std("c++17")
        .opt_level(2)
        .include("upstream-cargo")
        .file("src/bridge.cc")
        .compile("fuchsia_audio_processing");
}
