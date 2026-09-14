#[path = "../../mt7921-port-spike/src/bin/vfio_read.rs"]
mod vfio;

#[cfg(any(all(
    feature = "rate-power-evidence-only",
    feature = "full-firmware-production"
),))]
compile_error!("artifact flavor features are mutually exclusive");

#[cfg(not(any(
    feature = "rate-power-evidence-only",
    feature = "full-firmware-production",
)))]
compile_error!("select exactly one artifact flavor feature");

fn main() {
    vfio::main();
}
