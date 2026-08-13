#[path = "../../mt7921-port-spike/src/bin/vfio_read.rs"]
mod vfio;

#[cfg(all(feature = "rate-power-evidence-only", feature = "full-firmware-production"))]
compile_error!("rate-power-evidence-only and full-firmware-production are mutually exclusive");

#[cfg(not(any(feature = "rate-power-evidence-only", feature = "full-firmware-production")))]
compile_error!("select exactly one artifact flavor feature");

fn main() {
    vfio::main();
}
