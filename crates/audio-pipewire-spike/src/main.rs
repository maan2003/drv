use std::{env, fs, os::unix::net::UnixListener, path::PathBuf, process};

use drv_audio_pipewire_spike::{
    PlaybackEndpoint, VIRTUAL_SINK_FORMAT, VirtualPcmEndpoint, enum_format_pod, protocol,
};

fn main() {
    let mode = env::args().nth(1);
    if matches!(mode.as_deref(), Some("serve" | "serve-two")) {
        let runtime_dir = env::var_os("PIPEWIRE_RUNTIME_DIR")
            .or_else(|| env::var_os("XDG_RUNTIME_DIR"))
            .map(PathBuf::from)
            .expect("PIPEWIRE_RUNTIME_DIR or XDG_RUNTIME_DIR must be set");
        fs::create_dir_all(&runtime_dir).expect("create runtime directory");
        let socket = runtime_dir.join("pipewire-0");
        let listener = UnixListener::bind(&socket).expect("bind pipewire-0");
        let result = match if mode.as_deref() == Some("serve-two") {
            protocol::serve_two(&listener)
        } else {
            protocol::serve_one(&listener)
        } {
            Ok(result) => result,
            Err(error) => {
                eprintln!("PipeWire probe stopped: {error}");
                let _ = fs::remove_file(&socket);
                process::exit(2);
            }
        };
        fs::remove_file(socket).expect("remove pipewire-0");
        println!(
            "registered Fuchsia ADR ring-buffer frame position: {}, Fuchsia-processed sample checksum: {}",
            result.frame_position, result.processed_sample_checksum
        );
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
