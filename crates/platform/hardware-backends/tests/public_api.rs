#[cfg(target_os = "linux")]
#[test]
fn platform_vfio_composition_types_are_public() {
    use drv_hardware_backends::{
        LinuxVfio, LinuxVfioPlatformCapabilities, LinuxVfioPlatformFdIdentities,
    };

    fn accepts_public_types(
        _: Option<(
            LinuxVfio,
            LinuxVfioPlatformCapabilities,
            LinuxVfioPlatformFdIdentities,
        )>,
    ) {
    }

    accepts_public_types(None);
}
