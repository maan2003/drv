use std::{env, fs, os::unix::net::UnixListener, path::PathBuf};

use drv_audio_pipewire_spike::{
    PlaybackEndpoint, VIRTUAL_SINK_FORMAT, VirtualPcmEndpoint, enum_format_pod, protocol,
};

fn main() {
    if env::args().nth(1).as_deref() == Some("serve") {
        let runtime_dir = env::var_os("PIPEWIRE_RUNTIME_DIR")
            .or_else(|| env::var_os("XDG_RUNTIME_DIR"))
            .map(PathBuf::from)
            .expect("PIPEWIRE_RUNTIME_DIR or XDG_RUNTIME_DIR must be set");
        fs::create_dir_all(&runtime_dir).expect("create runtime directory");
        let socket = runtime_dir.join("pipewire-0");
        let listener = UnixListener::bind(&socket).expect("bind pipewire-0");
        protocol::serve_one(&listener).expect("serve PipeWire discovery client");
        fs::remove_file(socket).expect("remove pipewire-0");
        return;
    }

    let mut storage = [0; 256];
    let pod = enum_format_pod(&mut storage).expect("fixed format fits in probe buffer");
    let mut endpoint = VirtualPcmEndpoint::default();
    endpoint.write(&[0; 480 * 4]).unwrap();

    println!(
        "virtual playback sink: {:?}, SPA_PARAM_EnumFormat={} bytes, position={} frames",
        VIRTUAL_SINK_FORMAT,
        pod.len(),
        endpoint.frame_position()
    );
}
