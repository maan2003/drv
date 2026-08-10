use drv_audio_pipewire_spike::{
    PlaybackEndpoint, VIRTUAL_SINK_FORMAT, VirtualPcmEndpoint, enum_format_pod,
};

fn main() {
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
