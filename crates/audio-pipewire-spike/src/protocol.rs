//! Minimal server side of the PipeWire native discovery protocol.

use std::{
    collections::{BTreeMap, VecDeque},
    fs::File,
    io::IoSlice,
    io::{self, Read, Write},
    os::{
        fd::{AsRawFd, RawFd},
        unix::{
            fs::FileExt,
            net::{UnixListener, UnixStream},
        },
    },
    sync::mpsc::{self, Receiver, RecvTimeoutError},
    thread,
    time::{Duration, Instant},
};

use nix::{
    sys::{
        eventfd::{EfdFlags, EventFd},
        memfd::{MFdFlags, memfd_create},
        socket::{ControlMessage, MsgFlags, sendmsg},
    },
    time::{ClockId, clock_gettime},
    unistd::ftruncate,
};
use pipewire_native_spa::{
    param::ParamType,
    pod::{
        RawPod,
        builder::{Builder, StructBuilder},
        parser::Parser,
        types::{Id, Type},
    },
};

use drv_fuchsia_audio_processing::mix_stereo_s16;

use crate::{
    device_registry::{DeviceRegistry, RegisteredDeviceInfo},
    enum_format_pod_for,
};

const HEADER_LEN: usize = 16;
const MAX_PAYLOAD: usize = 64 * 1024;
const CORE_ID: u32 = 0;
const CLIENT_ID: u32 = 1;
const CORE_SYNC: u8 = 2;
const CORE_GET_REGISTRY: u8 = 5;
const CORE_CREATE_OBJECT: u8 = 6;
const CORE_ADD_MEM: u8 = 6;
const CORE_BOUND_PROPS: u8 = 8;
const CLIENT_UPDATE_PROPERTIES: u8 = 2;
const CORE_DONE: u8 = 1;
const REGISTRY_GLOBAL: u8 = 0;
const REGISTRY_BIND: u8 = 1;
const OBJECT_INFO: u8 = 0;
const OBJECT_PARAM: u8 = 1;
const OBJECT_SUBSCRIBE_PARAMS: u8 = 1;
const OBJECT_ENUM_PARAMS: u8 = 2;
const PERMISSIONS_RWX: i32 = (1 << 8) | (1 << 7) | (1 << 6);
const PARAM_INFO_READ: i32 = 1 << 1;
const CLIENT_NODE_TRANSPORT: u8 = 0;
const CLIENT_NODE_SET_PARAM: u8 = 1;
const CLIENT_NODE_PORT_USE_BUFFERS: u8 = 8;
const CLIENT_NODE_COMMAND: u8 = 4;
const CLIENT_NODE_PORT_SET_IO: u8 = 9;
const ACTIVATION_SIZE: i32 = 4096;
const BUFFER_COUNT: i32 = 2;
const BUFFER_STRIDE: i32 = 12 * 1024;
const BUFFER_DATA_OFFSET: i32 = 64;
const BUFFER_DATA_SIZE: i32 = 8 * 1024;

const DEFAULT_METADATA_ID: i32 = 4;
const CLIENT_NODE_FACTORY_ID: i32 = 5;
const CLIENT_NODE_GLOBAL_ID: i32 = 6;

#[derive(Debug)]
struct Header {
    id: u32,
    opcode: u8,
    size: usize,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum BoundKind {
    Node,
    Port,
    Metadata,
    Factory,
}

#[derive(Clone, Copy, Debug)]
struct BoundObject {
    proxy_id: u32,
    global_id: i32,
    kind: BoundKind,
}

#[derive(Debug)]
struct ClientNodeObject {
    proxy_id: u32,
    node_proxy_id: Option<u32>,
    node_updated: bool,
    port_updates: u8,
    transport: Option<ClientTransport>,
    buffers: Option<ClientBuffers>,
}

#[derive(Debug)]
struct ClientTransport {
    activation: File,
    read_event: EventFd,
    _write_event: EventFd,
}

#[derive(Debug)]
struct ClientBuffers {
    memory: File,
    io: File,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PlaybackResult {
    pub frame_position: u64,
    pub processed_sample_checksum: i64,
}

#[derive(Debug)]
enum StreamEvent {
    Started(u64),
    Chunk(u64, Vec<u8>),
    Ended(u64),
}

#[derive(Debug)]
struct ActiveStream {
    quantums: VecDeque<Vec<u8>>,
    ended: bool,
    solo: bool,
    started_at: Instant,
}

impl ActiveStream {
    fn new() -> Self {
        Self {
            quantums: VecDeque::new(),
            ended: false,
            solo: false,
            started_at: Instant::now(),
        }
    }
}

/// Run the single-user compatibility daemon until its listener is closed.
pub fn serve_daemon(listener: &UnixListener) -> io::Result<()> {
    let registry = DeviceRegistry::register_virtual_playback();
    let device = registry.playback().info().clone();
    let (event_tx, event_rx) = mpsc::channel();
    thread::spawn(move || run_ring_buffer_worker(registry, event_rx));
    let mut next_stream_id = 1;

    loop {
        let (mut stream, _) = listener.accept()?;
        let stream_id = next_stream_id;
        next_stream_id += 1;
        let device = device.clone();
        let event_tx = event_tx.clone();
        thread::spawn(move || {
            let mut started = false;
            let result = serve_connection_with(&mut stream, &device, true, &mut |pcm| {
                if !started {
                    let _ = event_tx.send(StreamEvent::Started(stream_id));
                    started = true;
                }
                let _ = event_tx.send(StreamEvent::Chunk(stream_id, pcm));
            });
            if started {
                let _ = event_tx.send(StreamEvent::Ended(stream_id));
            }
            if let Err(error) = result {
                eprintln!("PipeWire client disconnected: {error}");
            }
        });
    }
}

/// Serve one native client and return its Fuchsia-derived playback frame position.
pub fn serve_one(listener: &UnixListener) -> io::Result<PlaybackResult> {
    let mut registry = DeviceRegistry::register_virtual_playback();
    let (mut stream, _) = listener.accept()?;
    let pcm = serve_connection(&mut stream, registry.playback().info())?;
    process_pcm(&mut registry, &pcm)
}

/// Serve two bounded stock streams and mix them into one endpoint result.
pub fn serve_two(listener: &UnixListener) -> io::Result<PlaybackResult> {
    let mut registry = DeviceRegistry::register_virtual_playback();
    let (mut first_stream, _) = listener.accept()?;
    let first = serve_connection(&mut first_stream, registry.playback().info())?;
    let (mut second_stream, _) = listener.accept()?;
    let second = serve_connection(&mut second_stream, registry.playback().info())?;
    let mixed = mix_pcm(&first, &second)?;
    process_pcm(&mut registry, &mixed)
}

fn serve_connection(stream: &mut UnixStream, device: &RegisteredDeviceInfo) -> io::Result<Vec<u8>> {
    let mut chunks = Vec::new();
    serve_connection_with(stream, device, false, &mut |chunk| chunks.push(chunk))?;
    // Keep the legacy bounded probe result stable; persistent mode forwards
    // this stock-client drain quantum to the ring as a real timeline interval.
    if chunks.len() > 1
        && chunks
            .last()
            .is_some_and(|chunk| chunk.iter().all(|sample| *sample == 0))
    {
        chunks.pop();
    }
    Ok(chunks.into_iter().flatten().collect())
}

fn serve_connection_with(
    stream: &mut UnixStream,
    device: &RegisteredDeviceInfo,
    persistent: bool,
    on_pcm: &mut impl FnMut(Vec<u8>),
) -> io::Result<()> {
    let mut out_seq = 0;
    let mut registry_id = None;
    let mut bound_objects: Vec<BoundObject> = Vec::new();
    let mut client_nodes: Vec<ClientNodeObject> = Vec::new();

    loop {
        let (header, payload) = match read_message(stream) {
            Ok(message) => message,
            Err(error)
                if registry_id.is_some()
                    && matches!(
                        error.kind(),
                        io::ErrorKind::UnexpectedEof
                            | io::ErrorKind::WouldBlock
                            | io::ErrorKind::TimedOut
                            | io::ErrorKind::ConnectionReset
                    ) =>
            {
                return Ok(());
            }
            Err(error) => return Err(error),
        };

        if Some(header.id) == registry_id && header.opcode == REGISTRY_BIND {
            let object = decode_bind(&payload, device)?;
            if proxy_id_in_use(object.proxy_id, registry_id, &bound_objects, &client_nodes) {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "duplicate PipeWire proxy id",
                ));
            }
            write_bound_id(stream, object.proxy_id, object.global_id, &mut out_seq)?;
            write_object_info(stream, device, object, &mut out_seq)?;
            bound_objects.push(object);
            continue;
        }

        if let Some(client_node_index) = client_nodes
            .iter()
            .position(|client_node| client_node.proxy_id == header.id)
        {
            match header.opcode {
                1 => {
                    let node_proxy_id = decode_get_node(&payload)?;
                    if client_nodes[client_node_index].node_proxy_id.is_some()
                        || proxy_id_in_use(
                            node_proxy_id,
                            registry_id,
                            &bound_objects,
                            &client_nodes,
                        )
                    {
                        return Err(io::Error::new(
                            io::ErrorKind::InvalidData,
                            "duplicate ClientNode Node proxy id",
                        ));
                    }
                    client_nodes[client_node_index].node_proxy_id = Some(node_proxy_id);
                }
                2 => {
                    let port_config = decode_client_node_update(&payload)?;
                    client_nodes[client_node_index].node_updated = true;
                    if let Some(port_config) = port_config {
                        write_client_node_set_param(
                            stream,
                            client_nodes[client_node_index].proxy_id,
                            &mut out_seq,
                            &port_config,
                        )?;
                    }
                }
                3 => {
                    if !client_nodes[client_node_index].node_updated {
                        return Err(io::Error::new(
                            io::ErrorKind::InvalidData,
                            "ClientNode.PortUpdate arrived before Update",
                        ));
                    }
                    let has_format = decode_client_node_port_update(&payload)?;
                    if client_nodes[client_node_index].port_updates == 0 {
                        write_client_node_port_format(
                            stream,
                            device,
                            client_nodes[client_node_index].proxy_id,
                            &mut out_seq,
                        )?;
                        client_nodes[client_node_index].port_updates = 1;
                    } else if has_format {
                        let buffers = write_client_node_buffers(
                            stream,
                            client_nodes[client_node_index].proxy_id,
                            &mut out_seq,
                        )?;
                        client_nodes[client_node_index].buffers = Some(buffers);
                        write_client_node_start(
                            stream,
                            client_nodes[client_node_index].proxy_id,
                            &mut out_seq,
                        )?;
                        drive_client_node(
                            client_nodes[client_node_index].transport.as_ref().unwrap(),
                            client_nodes[client_node_index].buffers.as_ref().unwrap(),
                            on_pcm,
                        )?;
                        return Ok(());
                    } else {
                        return Err(io::Error::new(
                            io::ErrorKind::InvalidData,
                            "ClientNode.PortUpdate did not confirm the selected format",
                        ));
                    }
                }
                4 => {
                    let active = decode_client_node_set_active(&payload)?;
                    if let Some(transport) = &client_nodes[client_node_index].transport {
                        transport
                            .activation
                            .write_all_at(&(if active { 3_u32 } else { 4_u32 }).to_ne_bytes(), 0)?;
                    }
                }
                opcode => {
                    return Err(io::Error::new(
                        io::ErrorKind::Unsupported,
                        format!("unsupported ClientNode opcode {opcode}"),
                    ));
                }
            }
            continue;
        }

        if let Some(object) = bound_objects
            .iter()
            .find(|object| object.proxy_id == header.id)
            .copied()
        {
            match (object.kind, header.opcode) {
                (BoundKind::Node | BoundKind::Port, OBJECT_ENUM_PARAMS) => {
                    let request = decode_enum_params(&payload)?;
                    write_enum_format(stream, device, object.proxy_id, &mut out_seq, request)?;
                }
                // Clients may subscribe immediately after binding. The fixed
                // format has already been described in Info and never changes.
                (BoundKind::Node | BoundKind::Port, OBJECT_SUBSCRIBE_PARAMS) => {
                    decode_subscribe_params(&payload)?
                }
                (BoundKind::Metadata, 1) => {
                    let property = decode_metadata_set_property(&payload)?;
                    write_metadata_property(
                        stream,
                        object.proxy_id,
                        &mut out_seq,
                        property.0,
                        &property.1,
                        &property.2,
                        &property.3,
                    )?;
                }
                _ => {
                    return Err(io::Error::new(
                        io::ErrorKind::Unsupported,
                        format!("unsupported bound-object opcode {}", header.opcode),
                    ));
                }
            }
            continue;
        }

        match (header.id, header.opcode) {
            (CORE_ID, CORE_GET_REGISTRY) => {
                let new_registry_id = decode_get_registry(&payload)?;
                write_global(
                    stream,
                    new_registry_id,
                    &mut out_seq,
                    device.node_id(),
                    "PipeWire:Interface:Node",
                    &node_properties(device),
                )?;
                write_global(
                    stream,
                    new_registry_id,
                    &mut out_seq,
                    device.port_id(),
                    "PipeWire:Interface:Port",
                    &port_properties(device),
                )?;
                write_global(
                    stream,
                    new_registry_id,
                    &mut out_seq,
                    DEFAULT_METADATA_ID,
                    "PipeWire:Interface:Metadata",
                    &metadata_properties(),
                )?;
                write_global(
                    stream,
                    new_registry_id,
                    &mut out_seq,
                    CLIENT_NODE_FACTORY_ID,
                    "PipeWire:Interface:Factory",
                    &factory_properties(),
                )?;
                registry_id = Some(new_registry_id);
            }
            (CORE_ID, CORE_CREATE_OBJECT) => {
                let mut client_node = decode_create_client_node(&payload)?;
                if proxy_id_in_use(
                    client_node.proxy_id,
                    registry_id,
                    &bound_objects,
                    &client_nodes,
                ) {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidData,
                        "duplicate ClientNode proxy id",
                    ));
                }
                write_bound_props(stream, client_node.proxy_id, &mut out_seq)?;
                client_node.transport = Some(write_client_transport(
                    stream,
                    device,
                    client_node.proxy_id,
                    &mut out_seq,
                )?);
                client_nodes.push(client_node);
                stream.set_read_timeout(None)?;
            }
            (CORE_ID, CORE_SYNC) => {
                let (id, seq) = decode_sync(&payload)?;
                let body = encode_struct(|builder| builder.push_int(id).push_int(seq))?;
                write_message(stream, CORE_ID, CORE_DONE, out_seq, &body)?;
                out_seq += 1;
                if registry_id.is_some() && !persistent {
                    // The stock CLI sends Bind only after processing this Done.
                    // Keep the one-shot probe alive briefly for that request.
                    stream.set_read_timeout(Some(Duration::from_millis(250)))?;
                }
            }
            // Hello and Client.UpdateProperties are required connection bootstrap
            // messages but have no response in this discovery-only milestone.
            (CORE_ID, 1) => {
                decode_hello(&payload)?;
                write_core_info(stream, &mut out_seq)?;
            }
            (CLIENT_ID, CLIENT_UPDATE_PROPERTIES) => decode_properties(&payload)?,
            _ => {
                return Err(io::Error::new(
                    io::ErrorKind::Unsupported,
                    format!(
                        "unsupported PipeWire request id={} opcode={}",
                        header.id, header.opcode
                    ),
                ));
            }
        }
    }
}

fn process_pcm(registry: &mut DeviceRegistry, pcm: &[u8]) -> io::Result<PlaybackResult> {
    let device = registry.playback_mut();
    device
        .write_ring_buffer(pcm)
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, format!("{error:?}")))?;
    Ok(PlaybackResult {
        frame_position: device.frame_position(),
        processed_sample_checksum: device.processed_sample_checksum(),
    })
}

fn run_ring_buffer_worker(mut registry: DeviceRegistry, events: Receiver<StreamEvent>) {
    let mut active = BTreeMap::new();
    loop {
        match events.recv_timeout(Duration::from_millis(20)) {
            Ok(event) => handle_stream_event(&mut registry, &mut active, event),
            Err(RecvTimeoutError::Timeout) => {
                if active.len() == 1 {
                    active.values_mut().for_each(|stream| stream.solo = true);
                    drain_streams(&mut registry, &mut active);
                }
            }
            Err(RecvTimeoutError::Disconnected) => {
                active.values_mut().for_each(|stream| {
                    stream.ended = true;
                    stream.solo = true;
                });
                drain_streams(&mut registry, &mut active);
                return;
            }
        }
    }
}

fn handle_stream_event(
    registry: &mut DeviceRegistry,
    active: &mut BTreeMap<u64, ActiveStream>,
    event: StreamEvent,
) {
    match event {
        StreamEvent::Started(id) => {
            active.insert(id, ActiveStream::new());
            if active.len() > 1 {
                active.values_mut().for_each(|stream| stream.solo = false);
            }
        }
        StreamEvent::Chunk(id, pcm) => {
            let only_stream = active.len() == 1;
            if let Some(stream) = active.get_mut(&id) {
                stream.quantums.push_back(pcm);
                if stream.started_at.elapsed() >= Duration::from_millis(20) {
                    stream.solo = only_stream;
                }
            }
        }
        StreamEvent::Ended(id) => {
            if let Some(stream) = active.get_mut(&id) {
                stream.ended = true;
            }
        }
    }
    drain_streams(registry, active);
}

fn mix_pcm(first: &[u8], second: &[u8]) -> io::Result<Vec<u8>> {
    if first.len() != second.len() || !first.len().is_multiple_of(4) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "overlapping mixer inputs must have equal complete stereo frames",
        ));
    }
    mix_stereo_s16(&decode_s16(first), &decode_s16(second))
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, format!("{error:?}")))
        .map(|samples| samples.into_iter().flat_map(i16::to_le_bytes).collect())
}

fn drain_streams(registry: &mut DeviceRegistry, active: &mut BTreeMap<u64, ActiveStream>) {
    loop {
        let ids = active.keys().copied().take(2).collect::<Vec<_>>();
        match ids.as_slice() {
            [id] => {
                let stream = active.get_mut(id).unwrap();
                if !stream.solo && !stream.ended {
                    return;
                }
                let Some(pcm) = stream.quantums.pop_front() else {
                    if stream.ended {
                        active.remove(id);
                    }
                    return;
                };
                commit_pcm(registry, &pcm);
                if stream.ended && stream.quantums.is_empty() {
                    active.remove(id);
                }
            }
            [first_id, second_id] => {
                if active[first_id].quantums.is_empty() || active[second_id].quantums.is_empty() {
                    let ended_empty = ids.iter().copied().find(|id| {
                        let stream = &active[id];
                        stream.ended && stream.quantums.is_empty()
                    });
                    if let Some(id) = ended_empty {
                        active.remove(&id);
                        if active.len() == 1 {
                            active.values_mut().for_each(|stream| stream.solo = true);
                        }
                        continue;
                    }
                    return;
                }
                let mut first = active
                    .get_mut(first_id)
                    .unwrap()
                    .quantums
                    .pop_front()
                    .unwrap();
                let mut second = active
                    .get_mut(second_id)
                    .unwrap()
                    .quantums
                    .pop_front()
                    .unwrap();
                let size = first.len().min(second.len()) / 4 * 4;
                if first.len() > size {
                    let remainder = first.split_off(size);
                    active
                        .get_mut(first_id)
                        .unwrap()
                        .quantums
                        .push_front(remainder);
                }
                if second.len() > size {
                    let remainder = second.split_off(size);
                    active
                        .get_mut(second_id)
                        .unwrap()
                        .quantums
                        .push_front(remainder);
                }
                match mix_pcm(&first, &second) {
                    Ok(pcm) => commit_pcm(registry, &pcm),
                    Err(error) => eprintln!("PipeWire playback stopped: {error}"),
                }
            }
            [] => return,
            _ => unreachable!(),
        }
    }
}

fn commit_pcm(registry: &mut DeviceRegistry, pcm: &[u8]) {
    match process_pcm(registry, pcm) {
        Ok(result) => report_playback(result),
        Err(error) => eprintln!("PipeWire playback stopped: {error}"),
    }
}

fn report_playback(result: PlaybackResult) {
    println!(
        "registered Fuchsia ADR ring-buffer frame position: {}, Fuchsia-processed sample checksum: {}",
        result.frame_position, result.processed_sample_checksum
    );
}

fn decode_s16(pcm: &[u8]) -> Vec<i16> {
    pcm.chunks_exact(2)
        .map(|sample| i16::from_le_bytes(sample.try_into().unwrap()))
        .collect()
}

fn proxy_id_in_use(
    id: u32,
    registry_id: Option<u32>,
    bound_objects: &[BoundObject],
    client_nodes: &[ClientNodeObject],
) -> bool {
    id < 2
        || Some(id) == registry_id
        || bound_objects.iter().any(|object| object.proxy_id == id)
        || client_nodes
            .iter()
            .any(|object| object.proxy_id == id || object.node_proxy_id == Some(id))
}

fn decode_create_client_node(payload: &[u8]) -> io::Result<ClientNodeObject> {
    let mut parser = Parser::new(payload);
    parser
        .pop_struct(|fields| {
            let factory = fields.pop_string()?;
            let interface = fields.pop_string()?;
            let version = fields.pop_int()?;
            fields.pop_struct(|properties| {
                let count = properties.pop_int()?;
                if !(0..=128).contains(&count) {
                    return Err(pipewire_native_spa::pod::Error::Invalid(
                        "invalid ClientNode property count".into(),
                    ));
                }
                for _ in 0..count {
                    properties.pop_string()?;
                    properties.pop_string()?;
                }
                if properties.available() != 0 {
                    return Err(pipewire_native_spa::pod::Error::Invalid(
                        "trailing ClientNode properties".into(),
                    ));
                }
                Ok(())
            })?;
            let proxy_id = fields.pop_int()?;
            if factory != "client-node"
                || interface != "PipeWire:Interface:ClientNode"
                || !(1..=6).contains(&version)
                || proxy_id < 2
                || fields.available() != 0
            {
                return Err(pipewire_native_spa::pod::Error::Invalid(
                    "unsupported Core.CreateObject request".into(),
                ));
            }
            Ok(ClientNodeObject {
                proxy_id: proxy_id as u32,
                node_proxy_id: None,
                node_updated: false,
                port_updates: 0,
                transport: None,
                buffers: None,
            })
        })
        .map(|(object, _)| object)
        .map_err(invalid_pod)
}

fn decode_client_node_update(payload: &[u8]) -> io::Result<Option<Vec<u8>>> {
    let mut parser = Parser::new(payload);
    parser
        .pop_struct(|fields| {
            let change_mask = fields.pop_int()?;
            if change_mask & !0x3 != 0 {
                return Err(pipewire_native_spa::pod::Error::Invalid(
                    "invalid ClientNode.Update change mask".into(),
                ));
            }
            let params = parse_inline_params(fields)?;
            fields.pop_struct(|info| {
                let max_input_ports = info.pop_int()?;
                let max_output_ports = info.pop_int()?;
                let info_change_mask = info.pop_long()?;
                let _flags = info.pop_long()?;
                if !(0..=1024).contains(&max_input_ports)
                    || !(1..=1024).contains(&max_output_ports)
                    || info_change_mask & !0x7 != 0
                {
                    return Err(pipewire_native_spa::pod::Error::Invalid(
                        format!(
                            "invalid ClientNode node info input={max_input_ports} output={max_output_ports} mask={info_change_mask:#x}"
                        ),
                    ));
                }
                parse_inline_dict(info)?;
                parse_inline_param_info(info)?;
                require_empty(info, "ClientNode node info")
            })?;
            require_empty(fields, "ClientNode.Update")?;
            Ok(params.into_iter().find(|param| {
                param
                    .get(12..16)
                    .is_some_and(|id| {
                        u32::from_ne_bytes(id.try_into().unwrap()) == ParamType::PortConfig as u32
                    })
            }))
        })
        .map(|(port_config, _)| port_config)
        .map_err(invalid_pod)?
        .map(configure_playback_port)
        .transpose()
}

fn decode_client_node_port_update(payload: &[u8]) -> io::Result<bool> {
    let mut parser = Parser::new(payload);
    parser
        .pop_struct(|fields| {
            let direction = fields.pop_int()?;
            let port_id = fields.pop_int()?;
            let change_mask = fields.pop_int()?;
            if direction != 1 || port_id != 0 || change_mask & !0x3 != 0 {
                return Err(pipewire_native_spa::pod::Error::Invalid(
                    "invalid playback ClientNode.PortUpdate header".into(),
                ));
            }
            let params = parse_inline_params(fields)?;
            fields.pop_struct(|info| {
                let info_change_mask = info.pop_long()?;
                let _flags = info.pop_long()?;
                let _rate_num = info.pop_int()?;
                let rate_denom = info.pop_int()?;
                if info_change_mask & !0xf != 0 || rate_denom == 0 {
                    return Err(pipewire_native_spa::pod::Error::Invalid(
                        "invalid ClientNode port info".into(),
                    ));
                }
                parse_inline_dict(info)?;
                parse_inline_param_info(info)?;
                require_empty(info, "ClientNode port info")
            })?;
            require_empty(fields, "ClientNode.PortUpdate")?;
            Ok(params.iter().any(|param| {
                param.get(12..16).is_some_and(|id| {
                    u32::from_ne_bytes(id.try_into().unwrap()) == ParamType::Format as u32
                })
            }))
        })
        .map(|(has_format, _)| has_format)
        .map_err(invalid_pod)
}

fn decode_client_node_set_active(payload: &[u8]) -> io::Result<bool> {
    let mut parser = Parser::new(payload);
    parser
        .pop_struct(|fields| {
            let active = fields.pop_bool()?;
            require_empty(fields, "ClientNode.SetActive")?;
            Ok(active)
        })
        .map(|(active, _)| active)
        .map_err(invalid_pod)
}

fn parse_inline_params(
    parser: &mut Parser<'_>,
) -> Result<Vec<Vec<u8>>, pipewire_native_spa::pod::Error> {
    let count = parser.pop_int()?;
    if !(0..=64).contains(&count) {
        return Err(pipewire_native_spa::pod::Error::Invalid(
            "invalid ClientNode parameter count".into(),
        ));
    }
    let mut params = Vec::with_capacity(count as usize);
    for _ in 0..count {
        let pod = parser.pop_raw_pod()?;
        params.push(pod.data().to_vec());
    }
    Ok(params)
}

fn configure_playback_port(mut config: Vec<u8>) -> io::Result<Vec<u8>> {
    let mut position = 16;
    while position + 16 <= config.len() {
        let key = u32::from_ne_bytes(config[position..position + 4].try_into().unwrap());
        let size =
            u32::from_ne_bytes(config[position + 8..position + 12].try_into().unwrap()) as usize;
        let pod_type = u32::from_ne_bytes(config[position + 12..position + 16].try_into().unwrap());
        let padding = (8 - size % 8) % 8;
        let next = position
            .checked_add(8 + 8 + size + padding)
            .filter(|next| *next <= config.len())
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "invalid PortConfig pod"))?;
        if key == 2 {
            if size != 4 || pod_type != Type::Id as u32 {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "invalid PortConfig mode",
                ));
            }
            config[position + 16..position + 20].copy_from_slice(&2_u32.to_ne_bytes());
            return Ok(config);
        }
        position = next;
    }
    Err(io::Error::new(
        io::ErrorKind::InvalidData,
        "PortConfig has no mode",
    ))
}

fn parse_inline_dict(parser: &mut Parser<'_>) -> Result<(), pipewire_native_spa::pod::Error> {
    let count = parser.pop_int()?;
    if !(0..=128).contains(&count) {
        return Err(pipewire_native_spa::pod::Error::Invalid(
            "invalid ClientNode dictionary count".into(),
        ));
    }
    for _ in 0..count {
        parser.pop_string()?;
        parser.pop_string()?;
    }
    Ok(())
}

fn parse_inline_param_info(parser: &mut Parser<'_>) -> Result<(), pipewire_native_spa::pod::Error> {
    let count = parser.pop_int()?;
    if !(0..=128).contains(&count) {
        return Err(pipewire_native_spa::pod::Error::Invalid(
            "invalid ClientNode parameter-info count".into(),
        ));
    }
    for _ in 0..count {
        parser.pop_id::<u32>()?;
        parser.pop_int()?;
    }
    Ok(())
}

fn require_empty(
    parser: &Parser<'_>,
    context: &str,
) -> Result<(), pipewire_native_spa::pod::Error> {
    if parser.available() != 0 {
        return Err(pipewire_native_spa::pod::Error::Invalid(format!(
            "trailing {context} fields"
        )));
    }
    Ok(())
}

fn decode_get_node(payload: &[u8]) -> io::Result<u32> {
    let mut parser = Parser::new(payload);
    parser
        .pop_struct(|fields| {
            let version = fields.pop_int()?;
            let proxy_id = fields.pop_int()?;
            if !(1..=3).contains(&version) || proxy_id < 2 || fields.available() != 0 {
                return Err(pipewire_native_spa::pod::Error::Invalid(
                    "invalid ClientNode.GetNode request".into(),
                ));
            }
            Ok(proxy_id as u32)
        })
        .map(|(proxy_id, _)| proxy_id)
        .map_err(invalid_pod)
}

fn decode_bind(payload: &[u8], device: &RegisteredDeviceInfo) -> io::Result<BoundObject> {
    let mut parser = Parser::new(payload);
    parser
        .pop_struct(|fields| {
            let global_id = fields.pop_int()?;
            let interface = fields.pop_string()?;
            let version = fields.pop_int()?;
            let proxy_id = fields.pop_int()?;
            let kind = match (global_id, interface.as_str()) {
                (id, "PipeWire:Interface:Node") if id == device.node_id() => BoundKind::Node,
                (id, "PipeWire:Interface:Port") if id == device.port_id() => BoundKind::Port,
                (DEFAULT_METADATA_ID, "PipeWire:Interface:Metadata") => BoundKind::Metadata,
                (CLIENT_NODE_FACTORY_ID, "PipeWire:Interface:Factory") => BoundKind::Factory,
                _ => {
                    return Err(pipewire_native_spa::pod::Error::Invalid(
                        "bind does not match an advertised global".into(),
                    ));
                }
            };
            if !(1..=3).contains(&version) || proxy_id < 2 || fields.available() != 0 {
                return Err(pipewire_native_spa::pod::Error::Invalid(
                    "invalid bind version or proxy id".into(),
                ));
            }
            Ok(BoundObject {
                proxy_id: proxy_id as u32,
                global_id,
                kind,
            })
        })
        .map(|(object, _)| object)
        .map_err(invalid_pod)
}

#[derive(Clone, Copy, Debug)]
struct EnumParamsRequest {
    seq: i32,
}

fn decode_enum_params(payload: &[u8]) -> io::Result<EnumParamsRequest> {
    let mut parser = Parser::new(payload);
    parser
        .pop_struct(|fields| {
            let seq = fields.pop_int()?;
            let id = fields.pop_id::<u32>()?.0;
            let index = fields.pop_int()?;
            let _num = fields.pop_int()?;
            let filter = fields.pop_raw_pod()?;
            if !matches!(id, 3 | u32::MAX)
                || index != 0
                || filter.type_() != pipewire_native_spa::pod::types::Type::None
                || fields.available() != 0
            {
                return Err(pipewire_native_spa::pod::Error::Invalid(format!(
                    "unsupported EnumParams id={id} index={index} filter={:?}",
                    filter.type_()
                )));
            }
            Ok(EnumParamsRequest { seq })
        })
        .map(|(request, _)| request)
        .map_err(invalid_pod)
}

fn decode_subscribe_params(payload: &[u8]) -> io::Result<()> {
    let mut parser = Parser::new(payload);
    parser
        .pop_struct(|fields| {
            let ids = fields.pop_array::<Id<ParamType>>()?;
            if ids.iter().any(|id| id.0 != ParamType::EnumFormat) || fields.available() != 0 {
                return Err(pipewire_native_spa::pod::Error::Invalid(
                    "unsupported parameter subscription".into(),
                ));
            }
            Ok(())
        })
        .map(|_| ())
        .map_err(invalid_pod)
}

fn decode_metadata_set_property(payload: &[u8]) -> io::Result<(i32, String, String, String)> {
    let mut parser = Parser::new(payload);
    parser
        .pop_struct(|fields| {
            let property = (
                fields.pop_int()?,
                fields.pop_string()?,
                fields.pop_string()?,
                fields.pop_string()?,
            );
            require_empty(fields, "Metadata.SetProperty")?;
            Ok(property)
        })
        .map(|(property, _)| property)
        .map_err(invalid_pod)
}

fn decode_hello(payload: &[u8]) -> io::Result<()> {
    let mut parser = Parser::new(payload);
    parser
        .pop_struct(|fields| {
            let version = fields.pop_int()?;
            if version < 3 || fields.available() != 0 {
                return Err(pipewire_native_spa::pod::Error::Invalid(
                    "invalid core hello".into(),
                ));
            }
            Ok(())
        })
        .map(|_| ())
        .map_err(invalid_pod)
}

fn decode_properties(payload: &[u8]) -> io::Result<()> {
    let mut parser = Parser::new(payload);
    parser
        .pop_struct(|fields| {
            fields.pop_struct(|properties| {
                let count = properties.pop_int()?;
                if !(0..=128).contains(&count) {
                    return Err(pipewire_native_spa::pod::Error::Invalid(
                        "invalid property count".into(),
                    ));
                }
                for _ in 0..count {
                    properties.pop_string()?;
                    properties.pop_string()?;
                }
                if properties.available() != 0 {
                    return Err(pipewire_native_spa::pod::Error::Invalid(
                        "trailing client properties".into(),
                    ));
                }
                Ok(())
            })?;
            if fields.available() != 0 {
                return Err(pipewire_native_spa::pod::Error::Invalid(
                    "trailing client property fields".into(),
                ));
            }
            Ok(())
        })
        .map(|_| ())
        .map_err(invalid_pod)
}

fn decode_get_registry(payload: &[u8]) -> io::Result<u32> {
    let mut parser = Parser::new(payload);
    parser
        .pop_struct(|fields| {
            let _version = fields.pop_int()?;
            let new_id = fields.pop_int()?;
            if new_id < 2 || fields.available() != 0 {
                return Err(pipewire_native_spa::pod::Error::Invalid(
                    "invalid registry object id".into(),
                ));
            }
            Ok(new_id as u32)
        })
        .map(|(id, _)| id)
        .map_err(invalid_pod)
}

fn decode_sync(payload: &[u8]) -> io::Result<(i32, i32)> {
    let mut parser = Parser::new(payload);
    parser
        .pop_struct(|fields| {
            let id = fields.pop_int()?;
            let seq = fields.pop_int()?;
            if fields.available() != 0 {
                return Err(pipewire_native_spa::pod::Error::Invalid(
                    "trailing sync fields".into(),
                ));
            }
            Ok((id, seq))
        })
        .map(|(sync, _)| sync)
        .map_err(invalid_pod)
}

fn write_global(
    stream: &mut UnixStream,
    registry_id: u32,
    out_seq: &mut u32,
    global_id: i32,
    interface: &str,
    properties: &[(String, String)],
) -> io::Result<()> {
    let body = encode_struct(|builder| {
        builder
            .push_int(global_id)
            .push_int(PERMISSIONS_RWX)
            .push_string(interface)
            .push_int(3)
            .push_struct(|mut properties_builder| {
                properties_builder = properties_builder.push_int(properties.len() as i32);
                for (key, value) in properties {
                    properties_builder = properties_builder.push_string(key).push_string(value);
                }
                properties_builder
            })
    })?;
    write_message(stream, registry_id, REGISTRY_GLOBAL, *out_seq, &body)?;
    *out_seq += 1;
    Ok(())
}

fn write_core_info(stream: &mut UnixStream, out_seq: &mut u32) -> io::Result<()> {
    let properties = vec![
        ("core.name".into(), "drv-audio-daemon".into()),
        ("default.clock.rate".into(), "48000".into()),
        ("default.clock.quantum".into(), "480".into()),
    ];
    let body = encode_struct(|builder| {
        push_properties(
            builder
                .push_int(CORE_ID as i32)
                .push_int(1)
                .push_string("drv")
                .push_string("localhost")
                .push_string("1.6.6")
                .push_string("drv-audio-daemon")
                .push_long(1),
            &properties,
        )
    })?;
    write_message(stream, CORE_ID, 0, *out_seq, &body)?;
    *out_seq += 1;
    Ok(())
}

fn write_object_info(
    stream: &mut UnixStream,
    device: &RegisteredDeviceInfo,
    object: BoundObject,
    out_seq: &mut u32,
) -> io::Result<()> {
    let body = match object.kind {
        BoundKind::Node => encode_struct(|builder| {
            push_param_info(push_properties(
                builder
                    .push_int(device.node_id())
                    .push_int(1)
                    .push_int(0)
                    .push_long(0x1d)
                    .push_int(1)
                    .push_int(0)
                    .push_id(Id(2_u32))
                    .push_none(),
                &node_properties(device),
            ))
        })?,
        BoundKind::Port => encode_struct(|builder| {
            push_param_info(push_properties(
                builder
                    .push_int(device.port_id())
                    .push_int(0)
                    .push_long(0x3),
                &port_properties(device),
            ))
        })?,
        BoundKind::Metadata => {
            return write_metadata_property(
                stream,
                object.proxy_id,
                out_seq,
                0,
                "default.audio.sink",
                "Spa:String:JSON",
                &format!(r#"{{"name":"{}"}}"#, device.name()),
            );
        }
        BoundKind::Factory => encode_struct(|builder| {
            push_properties(
                builder
                    .push_int(CLIENT_NODE_FACTORY_ID)
                    .push_string("client-node")
                    .push_string("PipeWire:Interface:ClientNode")
                    .push_int(6)
                    .push_long(1),
                &factory_properties(),
            )
        })?,
    };
    write_message(stream, object.proxy_id, OBJECT_INFO, *out_seq, &body)?;
    *out_seq += 1;
    Ok(())
}

fn write_metadata_property(
    stream: &mut UnixStream,
    proxy_id: u32,
    out_seq: &mut u32,
    subject: i32,
    key: &str,
    type_name: &str,
    value: &str,
) -> io::Result<()> {
    let body = encode_struct(|builder| {
        builder
            .push_int(subject)
            .push_string(key)
            .push_string(type_name)
            .push_string(value)
    })?;
    write_message(stream, proxy_id, 0, *out_seq, &body)?;
    *out_seq += 1;
    Ok(())
}

fn write_enum_format(
    stream: &mut UnixStream,
    device: &RegisteredDeviceInfo,
    proxy_id: u32,
    out_seq: &mut u32,
    request: EnumParamsRequest,
) -> io::Result<()> {
    let mut storage = [0; 256];
    let format = enum_format_pod_for(device.format(), &mut storage).map_err(invalid_pod)?;
    let format = RawPod::wrap(format).map_err(invalid_pod)?;
    let body = encode_struct(|builder| {
        builder
            .push_int(request.seq)
            .push_id(Id(ParamType::EnumFormat))
            .push_int(0)
            .push_int(1)
            .push_pod(&format)
    })?;
    write_message(stream, proxy_id, OBJECT_PARAM, *out_seq, &body)?;
    *out_seq += 1;
    Ok(())
}

fn write_client_transport(
    stream: &mut UnixStream,
    device: &RegisteredDeviceInfo,
    client_node_id: u32,
    out_seq: &mut u32,
) -> io::Result<ClientTransport> {
    let activation = memfd_create(
        "drv-client-node-activation",
        MFdFlags::MFD_CLOEXEC | MFdFlags::MFD_ALLOW_SEALING,
    )
    .map_err(io::Error::from)?;
    ftruncate(&activation, i64::from(ACTIVATION_SIZE)).map_err(io::Error::from)?;
    let activation = File::from(activation);
    // `PW_NODE_ACTIVATION_INACTIVE`; the remaining activation page starts zeroed.
    activation.write_all_at(&4_u32.to_ne_bytes(), 0)?;
    activation.write_all_at(&(CLIENT_NODE_GLOBAL_ID as u32).to_ne_bytes(), 564)?;
    activation.write_all_at(&1_u32.to_ne_bytes(), 560)?;
    activation.write_all_at(&1_u32.to_ne_bytes(), 640)?;
    activation.write_all_at(&device.format().rate.to_ne_bytes(), 644)?;
    activation.write_all_at(&480_u64.to_ne_bytes(), 656)?;
    activation.write_all_at(&1_u32.to_ne_bytes(), 688)?;
    activation.write_all_at(&device.format().rate.to_ne_bytes(), 692)?;
    activation.write_all_at(&480_u64.to_ne_bytes(), 696)?;
    activation.write_all_at(&1_u32.to_ne_bytes(), 720)?;
    activation.write_all_at(&i64::MIN.to_ne_bytes(), 760)?;
    activation.write_all_at(&2_u32.to_ne_bytes(), 768)?;
    activation.write_all_at(&1_u32.to_ne_bytes(), 772)?;
    for segment in 0..8_u64 {
        activation.write_all_at(&1_f64.to_ne_bytes(), 800 + segment * 184)?;
    }

    let event_flags = EfdFlags::EFD_CLOEXEC | EfdFlags::EFD_NONBLOCK;
    let read_event = EventFd::from_value_and_flags(0, event_flags).map_err(io::Error::from)?;
    let write_event = EventFd::from_value_and_flags(0, event_flags).map_err(io::Error::from)?;

    let add_mem = encode_struct(|builder| {
        builder
            .push_int(0)
            .push_id(Id(2_u32))
            .push_fd(0)
            .push_int(3)
    })?;
    write_message_with_fds(
        stream,
        CORE_ID,
        CORE_ADD_MEM,
        *out_seq,
        &add_mem,
        &[activation.as_raw_fd()],
    )?;
    *out_seq += 1;

    let transport = encode_struct(|builder| {
        builder
            .push_fd(0)
            .push_fd(1)
            .push_int(0)
            .push_int(0)
            .push_int(ACTIVATION_SIZE)
    })?;
    write_message_with_fds(
        stream,
        client_node_id,
        CLIENT_NODE_TRANSPORT,
        *out_seq,
        &transport,
        &[read_event.as_raw_fd(), write_event.as_raw_fd()],
    )?;
    *out_seq += 1;

    Ok(ClientTransport {
        activation,
        read_event,
        _write_event: write_event,
    })
}

fn write_client_node_set_param(
    stream: &mut UnixStream,
    client_node_id: u32,
    out_seq: &mut u32,
    param: &[u8],
) -> io::Result<()> {
    let param = RawPod::wrap(param).map_err(invalid_pod)?;
    let body = encode_struct(|builder| {
        builder
            .push_id(Id(ParamType::PortConfig))
            .push_int(0)
            .push_pod(&param)
    })?;
    write_message(
        stream,
        client_node_id,
        CLIENT_NODE_SET_PARAM,
        *out_seq,
        &body,
    )?;
    *out_seq += 1;
    Ok(())
}

fn write_client_node_port_format(
    stream: &mut UnixStream,
    device: &RegisteredDeviceInfo,
    client_node_id: u32,
    out_seq: &mut u32,
) -> io::Result<()> {
    let mut storage = [0; 256];
    let mut format = enum_format_pod_for(device.format(), &mut storage)
        .map_err(invalid_pod)?
        .to_vec();
    format[12..16].copy_from_slice(&(ParamType::Format as u32).to_ne_bytes());
    let format = RawPod::wrap(&format).map_err(invalid_pod)?;
    let body = encode_struct(|builder| {
        builder
            .push_int(1)
            .push_int(0)
            .push_id(Id(ParamType::Format))
            .push_int(0)
            .push_pod(&format)
    })?;
    write_message(stream, client_node_id, 7, *out_seq, &body)?;
    *out_seq += 1;
    Ok(())
}

fn write_client_node_buffers(
    stream: &mut UnixStream,
    client_node_id: u32,
    out_seq: &mut u32,
) -> io::Result<ClientBuffers> {
    let memory = memfd_create(
        "drv-client-node-buffers",
        MFdFlags::MFD_CLOEXEC | MFdFlags::MFD_ALLOW_SEALING,
    )
    .map_err(io::Error::from)?;
    ftruncate(&memory, i64::from(BUFFER_COUNT * BUFFER_STRIDE)).map_err(io::Error::from)?;
    let memory = File::from(memory);

    let add_mem = encode_struct(|builder| {
        builder
            .push_int(1)
            .push_id(Id(2_u32))
            .push_fd(0)
            .push_int(3)
    })?;
    write_message_with_fds(
        stream,
        CORE_ID,
        CORE_ADD_MEM,
        *out_seq,
        &add_mem,
        &[memory.as_raw_fd()],
    )?;
    *out_seq += 1;

    let buffers = encode_struct(|mut builder| {
        builder = builder
            .push_int(1)
            .push_int(0)
            .push_int(-1)
            .push_int(0)
            .push_int(BUFFER_COUNT);
        for index in 0..BUFFER_COUNT {
            builder = builder
                .push_int(1)
                .push_int(index * BUFFER_STRIDE)
                .push_int(BUFFER_DATA_OFFSET + BUFFER_DATA_SIZE)
                .push_int(0)
                .push_int(1)
                .push_id(Id(1_u32))
                .push_int(BUFFER_DATA_OFFSET)
                .push_int(3)
                .push_int(0)
                .push_int(BUFFER_DATA_SIZE);
        }
        builder
    })?;
    write_message(
        stream,
        client_node_id,
        CLIENT_NODE_PORT_USE_BUFFERS,
        *out_seq,
        &buffers,
    )?;
    *out_seq += 1;

    let io = memfd_create(
        "drv-client-node-buffer-io",
        MFdFlags::MFD_CLOEXEC | MFdFlags::MFD_ALLOW_SEALING,
    )
    .map_err(io::Error::from)?;
    ftruncate(&io, 8).map_err(io::Error::from)?;
    let io = File::from(io);
    io.write_all_at(&1_i32.to_ne_bytes(), 0)?;
    io.write_all_at(&u32::MAX.to_ne_bytes(), 4)?;

    let add_io_mem = encode_struct(|builder| {
        builder
            .push_int(2)
            .push_id(Id(2_u32))
            .push_fd(0)
            .push_int(3)
    })?;
    write_message_with_fds(
        stream,
        CORE_ID,
        CORE_ADD_MEM,
        *out_seq,
        &add_io_mem,
        &[io.as_raw_fd()],
    )?;
    *out_seq += 1;

    let set_io = encode_struct(|builder| {
        builder
            .push_int(1)
            .push_int(0)
            .push_int(-1)
            .push_id(Id(1_u32))
            .push_int(2)
            .push_int(0)
            .push_int(8)
    })?;
    write_message(
        stream,
        client_node_id,
        CLIENT_NODE_PORT_SET_IO,
        *out_seq,
        &set_io,
    )?;
    *out_seq += 1;

    Ok(ClientBuffers { memory, io })
}

fn write_client_node_start(
    stream: &mut UnixStream,
    client_node_id: u32,
    out_seq: &mut u32,
) -> io::Result<()> {
    let mut command = Vec::with_capacity(16);
    command.extend(8_u32.to_ne_bytes());
    command.extend((Type::Object as u32).to_ne_bytes());
    command.extend(0x30002_u32.to_ne_bytes());
    command.extend(2_u32.to_ne_bytes());
    let command = RawPod::wrap(&command).map_err(invalid_pod)?;
    let body = encode_struct(|builder| builder.push_pod(&command))?;
    write_message(stream, client_node_id, CLIENT_NODE_COMMAND, *out_seq, &body)?;
    *out_seq += 1;
    Ok(())
}

fn drive_client_node(
    transport: &ClientTransport,
    buffers: &ClientBuffers,
    on_pcm: &mut impl FnMut(Vec<u8>),
) -> io::Result<()> {
    thread::sleep(Duration::from_millis(10));
    let mut recycled = u32::MAX;
    let mut produced = false;
    let mut graph_position = 0_u64;
    let mut cycle = 0_u32;

    loop {
        let now = clock_gettime(ClockId::CLOCK_MONOTONIC).map_err(io::Error::from)?;
        let nsec = u64::try_from(now.tv_sec()).unwrap() * 1_000_000_000
            + u64::try_from(now.tv_nsec()).unwrap();
        transport
            .activation
            .write_all_at(&nsec.to_ne_bytes(), 632)?;
        transport
            .activation
            .write_all_at(&graph_position.to_ne_bytes(), 648)?;
        transport
            .activation
            .write_all_at(&(nsec + 10_000_000).to_ne_bytes(), 680)?;
        transport
            .activation
            .write_all_at(&cycle.to_ne_bytes(), 708)?;
        buffers.io.write_all_at(&1_i32.to_ne_bytes(), 0)?;
        buffers.io.write_all_at(&recycled.to_ne_bytes(), 4)?;
        transport.activation.write_all_at(&1_u32.to_ne_bytes(), 0)?;
        transport.read_event.write(1).map_err(io::Error::from)?;

        let deadline = Instant::now() + Duration::from_secs(2);
        let inactive = loop {
            let mut status = [0; 4];
            transport.activation.read_exact_at(&mut status, 0)?;
            let status = u32::from_ne_bytes(status);
            if status == 3 {
                break false;
            }
            if status == 4 {
                break true;
            }
            if Instant::now() >= deadline {
                return Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    format!("client did not complete an activation cycle (status {status})"),
                ));
            }
            thread::sleep(Duration::from_millis(1));
        };
        if inactive {
            for buffer_id in 0..BUFFER_COUNT as u32 {
                let base = u64::from(buffer_id) * BUFFER_STRIDE as u64;
                let mut size = [0; 4];
                buffers.memory.read_exact_at(&mut size, base + 4)?;
                if u32::from_ne_bytes(size) != 0 {
                    on_pcm(consume_client_buffer(buffers, buffer_id)?);
                    produced = true;
                    break;
                }
            }
            if produced {
                return Ok(());
            }
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "client deactivated without producing PCM",
            ));
        }

        let mut io_state = [0; 8];
        buffers.io.read_exact_at(&mut io_state, 0)?;
        let status = i32::from_ne_bytes(io_state[0..4].try_into().unwrap());
        let buffer_id = u32::from_ne_bytes(io_state[4..8].try_into().unwrap());
        let mut node_status = [0; 4];
        transport.activation.read_exact_at(&mut node_status, 8)?;
        let node_status = i32::from_ne_bytes(node_status);
        cycle = cycle.wrapping_add(1);
        if status & 2 != 0 || node_status & 2 != 0 {
            let mut consumed = false;
            // The exported node can leave SPA_IO_Buffers pointing at the
            // just-recycled descriptor while reporting HAVE_DATA through its
            // activation state, so select whichever descriptor is populated.
            let buffer_id =
                if buffer_id < BUFFER_COUNT as u32 && client_buffer_has_data(buffers, buffer_id)? {
                    Some(buffer_id)
                } else {
                    find_produced_buffer(buffers)?
                };
            if let Some(buffer_id) = buffer_id {
                let pcm = consume_client_buffer(buffers, buffer_id)?;
                buffers.memory.write_all_at(
                    &0_u32.to_ne_bytes(),
                    u64::from(buffer_id) * BUFFER_STRIDE as u64 + 4,
                )?;
                on_pcm(pcm);
                produced = true;
                recycled = buffer_id;
                graph_position = graph_position.saturating_add(480);
                consumed = true;
            } else if status & 2 != 0 {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "client reported data without a valid buffer",
                ));
            }
            if !produced {
                graph_position = graph_position.saturating_add(480);
            }
            if consumed || !produced {
                thread::sleep(Duration::from_millis(10));
            } else {
                thread::yield_now();
            }
        } else if status & 8 != 0 {
            return Ok(());
        } else if status < 0 {
            return Err(io::Error::from_raw_os_error(-status));
        } else {
            recycled = u32::MAX;
            if !produced {
                graph_position = graph_position.saturating_add(480);
                thread::sleep(Duration::from_millis(10));
            } else {
                thread::yield_now();
            }
        }
    }
}

fn find_produced_buffer(buffers: &ClientBuffers) -> io::Result<Option<u32>> {
    for buffer_id in 0..BUFFER_COUNT as u32 {
        if client_buffer_has_data(buffers, buffer_id)? {
            return Ok(Some(buffer_id));
        }
    }
    Ok(None)
}

fn client_buffer_has_data(buffers: &ClientBuffers, buffer_id: u32) -> io::Result<bool> {
    let mut size = [0; 4];
    buffers
        .memory
        .read_exact_at(&mut size, u64::from(buffer_id) * BUFFER_STRIDE as u64 + 4)?;
    Ok(u32::from_ne_bytes(size) != 0)
}

fn consume_client_buffer(buffers: &ClientBuffers, buffer_id: u32) -> io::Result<Vec<u8>> {
    let base = u64::from(buffer_id) * BUFFER_STRIDE as u64;
    let mut chunk = [0; 16];
    buffers.memory.read_exact_at(&mut chunk, base)?;
    let offset = u32::from_ne_bytes(chunk[0..4].try_into().unwrap()) as usize;
    let size = u32::from_ne_bytes(chunk[4..8].try_into().unwrap()) as usize;
    if size > BUFFER_DATA_SIZE as usize || !size.is_multiple_of(4) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "client produced an invalid PCM chunk size",
        ));
    }
    let offset = offset % BUFFER_DATA_SIZE as usize;
    let first = size.min(BUFFER_DATA_SIZE as usize - offset);
    let mut pcm = vec![0; size];
    buffers.memory.read_exact_at(
        &mut pcm[..first],
        base + BUFFER_DATA_OFFSET as u64 + offset as u64,
    )?;
    if first < size {
        buffers
            .memory
            .read_exact_at(&mut pcm[first..], base + BUFFER_DATA_OFFSET as u64)?;
    }
    Ok(pcm)
}

fn write_bound_props(stream: &mut UnixStream, proxy_id: u32, out_seq: &mut u32) -> io::Result<()> {
    let body = encode_struct(|builder| {
        builder
            .push_int(proxy_id as i32)
            .push_int(CLIENT_NODE_GLOBAL_ID)
            .push_struct(|builder| builder.push_int(0))
    })?;
    write_message(stream, CORE_ID, CORE_BOUND_PROPS, *out_seq, &body)?;
    *out_seq += 1;
    Ok(())
}

fn write_bound_id(
    stream: &mut UnixStream,
    proxy_id: u32,
    global_id: i32,
    out_seq: &mut u32,
) -> io::Result<()> {
    let body = encode_struct(|builder| builder.push_int(proxy_id as i32).push_int(global_id))?;
    write_message(stream, CORE_ID, 5, *out_seq, &body)?;
    *out_seq += 1;
    Ok(())
}

fn push_properties<'a>(
    builder: StructBuilder<'a>,
    properties: &[(String, String)],
) -> StructBuilder<'a> {
    builder.push_struct(|mut pair_list| {
        pair_list = pair_list.push_int(properties.len() as i32);
        for (key, value) in properties {
            pair_list = pair_list.push_string(key).push_string(value);
        }
        pair_list
    })
}

fn node_properties(device: &RegisteredDeviceInfo) -> Vec<(String, String)> {
    let format = device.format();
    vec![
        ("object.serial".into(), device.token_id().to_string()),
        ("device.id".into(), device.token_id().to_string()),
        ("device.api".into(), "fuchsia.audio.device".into()),
        ("node.name".into(), device.name().into()),
        ("node.description".into(), device.description().into()),
        ("media.class".into(), "Audio/Sink".into()),
        ("audio.format".into(), device.sample_format_name().into()),
        ("audio.rate".into(), format.rate.to_string()),
        ("audio.channels".into(), format.channels.to_string()),
    ]
}

fn port_properties(device: &RegisteredDeviceInfo) -> Vec<(String, String)> {
    vec![
        ("object.serial".into(), device.port_id().to_string()),
        ("node.id".into(), device.node_id().to_string()),
        ("port.id".into(), "0".into()),
        ("port.name".into(), "playback".into()),
        ("port.direction".into(), "in".into()),
        ("port.alias".into(), format!("{}:playback", device.name())),
    ]
}

fn metadata_properties() -> Vec<(String, String)> {
    vec![
        ("object.serial".into(), DEFAULT_METADATA_ID.to_string()),
        ("metadata.name".into(), "default".into()),
    ]
}

fn factory_properties() -> Vec<(String, String)> {
    vec![
        ("object.serial".into(), CLIENT_NODE_FACTORY_ID.to_string()),
        ("factory.name".into(), "client-node".into()),
        (
            "factory.type.name".into(),
            "PipeWire:Interface:ClientNode".into(),
        ),
        ("factory.type.version".into(), "6".into()),
    ]
}

fn push_param_info(builder: StructBuilder<'_>) -> StructBuilder<'_> {
    builder.push_struct(|pair_list| {
        pair_list
            .push_int(1)
            .push_id(Id(ParamType::EnumFormat))
            .push_int(PARAM_INFO_READ)
    })
}

fn encode_struct(
    build: impl FnOnce(
        pipewire_native_spa::pod::builder::StructBuilder<'_>,
    ) -> pipewire_native_spa::pod::builder::StructBuilder<'_>,
) -> io::Result<Vec<u8>> {
    let mut storage = vec![0; 4096];
    let encoded = Builder::new(&mut storage)
        .push_struct(build)
        .build()
        .map_err(invalid_pod)?;
    Ok(encoded.to_vec())
}

fn read_message(stream: &mut UnixStream) -> io::Result<(Header, Vec<u8>)> {
    let mut bytes = [0; HEADER_LEN];
    stream.read_exact(&mut bytes)?;
    let word = u32::from_ne_bytes(bytes[4..8].try_into().unwrap());
    let size = (word & 0x00ff_ffff) as usize;
    let n_fds = u32::from_ne_bytes(bytes[12..16].try_into().unwrap());
    if size > MAX_PAYLOAD || n_fds != 0 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "oversized payload or unsupported file descriptors",
        ));
    }
    let header = Header {
        id: u32::from_ne_bytes(bytes[0..4].try_into().unwrap()),
        opcode: (word >> 24) as u8,
        size,
    };
    let mut payload = vec![0; header.size];
    stream.read_exact(&mut payload)?;
    Ok((header, payload))
}

fn write_message(
    stream: &mut UnixStream,
    id: u32,
    opcode: u8,
    seq: u32,
    payload: &[u8],
) -> io::Result<()> {
    let size = u32::try_from(payload.len())
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidInput, error))?;
    let mut header = [0; HEADER_LEN];
    header[0..4].copy_from_slice(&id.to_ne_bytes());
    header[4..8].copy_from_slice(&((u32::from(opcode) << 24) | size).to_ne_bytes());
    header[8..12].copy_from_slice(&seq.to_ne_bytes());
    stream.write_all(&header)?;
    stream.write_all(payload)
}

fn write_message_with_fds(
    stream: &mut UnixStream,
    id: u32,
    opcode: u8,
    seq: u32,
    payload: &[u8],
    fds: &[RawFd],
) -> io::Result<()> {
    let size = u32::try_from(payload.len())
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidInput, error))?;
    let n_fds = u32::try_from(fds.len())
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidInput, error))?;
    if fds.is_empty() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "file-descriptor message has no descriptors",
        ));
    }

    let mut bytes = Vec::with_capacity(HEADER_LEN + payload.len());
    bytes.extend(id.to_ne_bytes());
    bytes.extend(((u32::from(opcode) << 24) | size).to_ne_bytes());
    bytes.extend(seq.to_ne_bytes());
    bytes.extend(n_fds.to_ne_bytes());
    bytes.extend(payload);

    let sent = sendmsg::<()>(
        stream.as_raw_fd(),
        &[IoSlice::new(&bytes)],
        &[ControlMessage::ScmRights(fds)],
        MsgFlags::empty(),
        None,
    )
    .map_err(io::Error::from)?;
    stream.write_all(&bytes[sent..])
}

fn invalid_pod(error: pipewire_native_spa::pod::Error) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, format!("{error:?}"))
}

#[cfg(test)]
mod tests {
    use std::{io::IoSliceMut, os::unix::net::UnixStream, thread};

    use nix::{
        cmsg_space,
        sys::socket::{ControlMessageOwned, recvmsg},
        unistd::close,
    };

    use super::*;

    fn request(
        id: u32,
        opcode: u8,
        fields: impl FnOnce(
            pipewire_native_spa::pod::builder::StructBuilder<'_>,
        ) -> pipewire_native_spa::pod::builder::StructBuilder<'_>,
    ) -> Vec<u8> {
        let payload = encode_struct(fields).unwrap();
        let mut bytes = Vec::new();
        let size = payload.len() as u32;
        bytes.extend(id.to_ne_bytes());
        bytes.extend(((u32::from(opcode) << 24) | size).to_ne_bytes());
        bytes.extend(0_u32.to_ne_bytes());
        bytes.extend(0_u32.to_ne_bytes());
        bytes.extend(payload);
        bytes
    }

    fn client_port_update(id: u32, param: &RawPod<'_>, param_id: ParamType) -> Vec<u8> {
        request(id, 3, |b| {
            b.push_int(1)
                .push_int(0)
                .push_int(3)
                .push_int(1)
                .push_pod(param)
                .push_struct(|b| {
                    b.push_long(0xf)
                        .push_long(0)
                        .push_int(0)
                        .push_int(1)
                        .push_int(1)
                        .push_string("port.name")
                        .push_string("output")
                        .push_int(1)
                        .push_id(Id(param_id))
                        .push_int(PARAM_INFO_READ)
                })
        })
    }

    fn read_message_with_fds(stream: &mut UnixStream) -> (Header, Vec<RawFd>, Vec<u8>) {
        let mut bytes = [0; 4096];
        let mut iov = [IoSliceMut::new(&mut bytes)];
        let mut control = cmsg_space!([RawFd; 2]);
        let message = recvmsg::<()>(
            stream.as_raw_fd(),
            &mut iov,
            Some(&mut control),
            MsgFlags::empty(),
        )
        .unwrap();
        let received = message.bytes;
        let fds = message
            .cmsgs()
            .unwrap()
            .flat_map(|message| match message {
                ControlMessageOwned::ScmRights(fds) => fds,
                _ => panic!("unexpected ancillary message"),
            })
            .collect::<Vec<_>>();
        assert!(!message.flags.contains(MsgFlags::MSG_CTRUNC));

        let word = u32::from_ne_bytes(bytes[4..8].try_into().unwrap());
        let header = Header {
            id: u32::from_ne_bytes(bytes[0..4].try_into().unwrap()),
            opcode: (word >> 24) as u8,
            size: (word & 0x00ff_ffff) as usize,
        };
        let n_fds = u32::from_ne_bytes(bytes[12..16].try_into().unwrap());
        assert_eq!(fds.len(), n_fds as usize);
        assert_eq!(received, HEADER_LEN + header.size);
        (header, fds, bytes[HEADER_LEN..received].to_vec())
    }

    fn assert_received_fds(fds: Vec<RawFd>, expected: usize, expected_size: Option<u64>) {
        assert_eq!(fds.len(), expected);
        for fd in fds {
            let metadata = std::fs::metadata(format!("/proc/self/fd/{fd}")).unwrap();
            if let Some(expected_size) = expected_size {
                assert_eq!(metadata.len(), expected_size);
            }
            close(fd).unwrap();
        }
    }

    fn decode_global(payload: &[u8]) -> (i32, String, Vec<(String, String)>) {
        let mut parser = Parser::new(payload);
        parser
            .pop_struct(|fields| {
                let id = fields.pop_int()?;
                assert_eq!(fields.pop_int()?, PERMISSIONS_RWX);
                let interface = fields.pop_string()?;
                assert_eq!(fields.pop_int()?, 3);
                let (properties, _) = fields.pop_struct(|properties| {
                    let count = properties.pop_int()?;
                    (0..count)
                        .map(|_| Ok((properties.pop_string()?, properties.pop_string()?)))
                        .collect::<Result<Vec<_>, _>>()
                })?;
                Ok((id, interface, properties))
            })
            .unwrap()
            .0
    }

    fn assert_bound_id(stream: &mut UnixStream, proxy_id: i32, global_id: i32) {
        let (header, payload) = read_message(stream).unwrap();
        assert_eq!((header.id, header.opcode), (CORE_ID, 5));
        let mut parser = Parser::new(&payload);
        parser
            .pop_struct(|fields| {
                assert_eq!(fields.pop_int()?, proxy_id);
                assert_eq!(fields.pop_int()?, global_id);
                require_empty(fields, "test Core.BoundId")
            })
            .unwrap();
    }

    #[test]
    fn get_registry_advertises_virtual_sink_node_and_input_port() {
        let (mut client, mut server) = UnixStream::pair().unwrap();
        let worker = thread::spawn(move || {
            let registry = DeviceRegistry::register_virtual_playback();
            serve_connection(&mut server, registry.playback().info()).unwrap()
        });

        client
            .write_all(&request(CORE_ID, 1, |b| b.push_int(3)))
            .unwrap();
        client
            .write_all(&request(CLIENT_ID, CLIENT_UPDATE_PROPERTIES, |b| {
                b.push_struct(|b| b.push_int(0))
            }))
            .unwrap();
        client
            .write_all(&request(CORE_ID, CORE_GET_REGISTRY, |b| {
                b.push_int(3).push_int(7)
            }))
            .unwrap();

        let (core_header, _) = read_message(&mut client).unwrap();
        assert_eq!((core_header.id, core_header.opcode), (CORE_ID, 0));
        let (node_header, node_payload) = read_message(&mut client).unwrap();
        let (port_header, port_payload) = read_message(&mut client).unwrap();
        let (metadata_header, metadata_payload) = read_message(&mut client).unwrap();
        let (factory_header, factory_payload) = read_message(&mut client).unwrap();
        assert_eq!((node_header.id, node_header.opcode), (7, REGISTRY_GLOBAL));
        assert_eq!((port_header.id, port_header.opcode), (7, REGISTRY_GLOBAL));
        assert_eq!(
            (metadata_header.id, metadata_header.opcode),
            (7, REGISTRY_GLOBAL)
        );
        assert_eq!(
            (factory_header.id, factory_header.opcode),
            (7, REGISTRY_GLOBAL)
        );

        let node = decode_global(&node_payload);
        assert_eq!(node.0, 2);
        assert_eq!(node.1, "PipeWire:Interface:Node");
        assert!(
            node.2
                .contains(&("media.class".into(), "Audio/Sink".into()))
        );
        assert!(
            node.2
                .contains(&("device.api".into(), "fuchsia.audio.device".into()))
        );
        assert!(
            node.2
                .contains(&("node.name".into(), "drv.adr-virtual-sink".into()))
        );
        assert!(node.2.contains(&("audio.rate".into(), "48000".into())));

        let port = decode_global(&port_payload);
        assert_eq!(port.0, 3);
        assert_eq!(port.1, "PipeWire:Interface:Port");
        assert!(port.2.contains(&("port.direction".into(), "in".into())));
        assert!(port.2.contains(&("node.id".into(), "2".into())));

        let metadata = decode_global(&metadata_payload);
        assert_eq!(
            (metadata.0, metadata.1.as_str()),
            (4, "PipeWire:Interface:Metadata")
        );
        assert!(
            metadata
                .2
                .contains(&("metadata.name".into(), "default".into()))
        );
        let factory = decode_global(&factory_payload);
        assert_eq!(
            (factory.0, factory.1.as_str()),
            (5, "PipeWire:Interface:Factory")
        );
        assert!(
            factory
                .2
                .contains(&("factory.name".into(), "client-node".into()))
        );

        client
            .write_all(&request(CORE_ID, CORE_SYNC, |b| b.push_int(0).push_int(99)))
            .unwrap();
        let (done_header, done_payload) = read_message(&mut client).unwrap();
        assert_eq!((done_header.id, done_header.opcode), (CORE_ID, CORE_DONE));
        assert_eq!(decode_sync(&done_payload).unwrap(), (0, 99));
        drop(client);
        worker.join().unwrap();
    }

    #[test]
    fn bind_emits_info_and_enum_format_for_node_and_port() {
        let (mut client, mut server) = UnixStream::pair().unwrap();
        let worker = thread::spawn(move || {
            let registry = DeviceRegistry::register_virtual_playback();
            serve_connection(&mut server, registry.playback().info()).unwrap()
        });

        client
            .write_all(&request(CORE_ID, 1, |b| b.push_int(3)))
            .unwrap();
        client
            .write_all(&request(CLIENT_ID, CLIENT_UPDATE_PROPERTIES, |b| {
                b.push_struct(|b| b.push_int(0))
            }))
            .unwrap();
        client
            .write_all(&request(CORE_ID, CORE_GET_REGISTRY, |b| {
                b.push_int(3).push_int(7)
            }))
            .unwrap();
        let (core_header, _) = read_message(&mut client).unwrap();
        assert_eq!((core_header.id, core_header.opcode), (CORE_ID, 0));
        read_message(&mut client).unwrap();
        read_message(&mut client).unwrap();
        read_message(&mut client).unwrap();
        read_message(&mut client).unwrap();
        client
            .write_all(&request(CORE_ID, CORE_SYNC, |b| b.push_int(0).push_int(1)))
            .unwrap();
        read_message(&mut client).unwrap();

        client
            .write_all(&request(7, REGISTRY_BIND, |b| {
                b.push_int(2)
                    .push_string("PipeWire:Interface:Node")
                    .push_int(3)
                    .push_int(8)
            }))
            .unwrap();
        assert_bound_id(&mut client, 8, 2);
        let (node_header, node_info) = read_message(&mut client).unwrap();
        assert_eq!((node_header.id, node_header.opcode), (8, OBJECT_INFO));
        let mut node_parser = Parser::new(&node_info);
        node_parser
            .pop_struct(|fields| {
                assert_eq!(fields.pop_int()?, 2);
                assert_eq!((fields.pop_int()?, fields.pop_int()?), (1, 0));
                assert_eq!(fields.pop_long()?, 0x1d);
                assert_eq!((fields.pop_int()?, fields.pop_int()?), (1, 0));
                assert_eq!(fields.pop_id::<u32>()?.0, 2);
                fields.pop_none()?;
                assert_eq!(fields.pop_struct(|p| p.pop_int())?.0, 9);
                let (param_count, _) = fields.pop_struct(|p| p.pop_int())?;
                assert_eq!(param_count, 1);
                Ok(())
            })
            .unwrap();

        client
            .write_all(&request(7, REGISTRY_BIND, |b| {
                b.push_int(3)
                    .push_string("PipeWire:Interface:Port")
                    .push_int(3)
                    .push_int(9)
            }))
            .unwrap();
        assert_bound_id(&mut client, 9, 3);
        let (port_header, port_info) = read_message(&mut client).unwrap();
        assert_eq!((port_header.id, port_header.opcode), (9, OBJECT_INFO));
        let mut port_parser = Parser::new(&port_info);
        port_parser
            .pop_struct(|fields| {
                assert_eq!(fields.pop_int()?, 3);
                assert_eq!(fields.pop_int()?, 0);
                assert_eq!(fields.pop_long()?, 0x3);
                Ok(())
            })
            .unwrap();

        client
            .write_all(&request(7, REGISTRY_BIND, |b| {
                b.push_int(DEFAULT_METADATA_ID)
                    .push_string("PipeWire:Interface:Metadata")
                    .push_int(3)
                    .push_int(10)
            }))
            .unwrap();
        assert_bound_id(&mut client, 10, DEFAULT_METADATA_ID);
        let (metadata_header, metadata_property) = read_message(&mut client).unwrap();
        assert_eq!((metadata_header.id, metadata_header.opcode), (10, 0));
        let mut metadata_parser = Parser::new(&metadata_property);
        metadata_parser
            .pop_struct(|fields| {
                assert_eq!(fields.pop_int()?, 0);
                assert_eq!(fields.pop_string()?, "default.audio.sink");
                assert_eq!(fields.pop_string()?, "Spa:String:JSON");
                assert_eq!(fields.pop_string()?, r#"{"name":"drv.adr-virtual-sink"}"#);
                require_empty(fields, "test Metadata.Property")
            })
            .unwrap();

        client
            .write_all(&request(7, REGISTRY_BIND, |b| {
                b.push_int(CLIENT_NODE_FACTORY_ID)
                    .push_string("PipeWire:Interface:Factory")
                    .push_int(3)
                    .push_int(11)
            }))
            .unwrap();
        assert_bound_id(&mut client, 11, CLIENT_NODE_FACTORY_ID);
        let (factory_header, factory_info) = read_message(&mut client).unwrap();
        assert_eq!((factory_header.id, factory_header.opcode), (11, 0));
        let mut factory_parser = Parser::new(&factory_info);
        factory_parser
            .pop_struct(|fields| {
                assert_eq!(fields.pop_int()?, CLIENT_NODE_FACTORY_ID);
                assert_eq!(fields.pop_string()?, "client-node");
                assert_eq!(fields.pop_string()?, "PipeWire:Interface:ClientNode");
                assert_eq!(fields.pop_int()?, 6);
                assert_eq!(fields.pop_long()?, 1);
                assert_eq!(fields.pop_struct(|props| props.pop_int())?.0, 4);
                Ok(())
            })
            .unwrap();

        client
            .write_all(&request(8, OBJECT_ENUM_PARAMS, |b| {
                b.push_int(55)
                    .push_id(Id(ParamType::EnumFormat))
                    .push_int(0)
                    .push_int(1)
                    .push_none()
            }))
            .unwrap();
        let (param_header, param) = read_message(&mut client).unwrap();
        assert_eq!((param_header.id, param_header.opcode), (8, OBJECT_PARAM));
        let mut param_parser = Parser::new(&param);
        param_parser
            .pop_struct(|fields| {
                assert_eq!(fields.pop_int()?, 55);
                assert_eq!(fields.pop_id::<ParamType>()?.0, ParamType::EnumFormat);
                assert_eq!((fields.pop_int()?, fields.pop_int()?), (0, 1));
                let returned = fields.pop_raw_pod()?;
                let mut expected_storage = [0; 256];
                let registry = DeviceRegistry::register_virtual_playback();
                let expected = enum_format_pod_for(
                    registry.playback().info().format(),
                    &mut expected_storage,
                )?;
                assert_eq!(returned.data(), expected);
                Ok(())
            })
            .unwrap();

        client
            .write_all(&request(CORE_ID, CORE_SYNC, |b| b.push_int(0).push_int(2)))
            .unwrap();
        read_message(&mut client).unwrap();
        drop(client);
        worker.join().unwrap();
    }

    #[test]
    fn emits_client_transport_and_accepts_client_node_updates() {
        let (mut client, mut server) = UnixStream::pair().unwrap();
        let worker = thread::spawn(move || {
            let registry = DeviceRegistry::register_virtual_playback();
            serve_connection(&mut server, registry.playback().info())
        });

        client
            .write_all(&request(CORE_ID, 1, |b| b.push_int(3)))
            .unwrap();
        client
            .write_all(&request(CLIENT_ID, CLIENT_UPDATE_PROPERTIES, |b| {
                b.push_struct(|b| b.push_int(0))
            }))
            .unwrap();
        client
            .write_all(&request(CORE_ID, CORE_GET_REGISTRY, |b| {
                b.push_int(3).push_int(7)
            }))
            .unwrap();
        let (core_header, _) = read_message(&mut client).unwrap();
        assert_eq!((core_header.id, core_header.opcode), (CORE_ID, 0));
        read_message(&mut client).unwrap();
        read_message(&mut client).unwrap();
        read_message(&mut client).unwrap();
        read_message(&mut client).unwrap();
        client
            .write_all(&request(CORE_ID, CORE_SYNC, |b| b.push_int(0).push_int(1)))
            .unwrap();
        read_message(&mut client).unwrap();

        client
            .write_all(&request(CORE_ID, CORE_CREATE_OBJECT, |b| {
                b.push_string("client-node")
                    .push_string("PipeWire:Interface:ClientNode")
                    .push_int(6)
                    .push_struct(|b| {
                        b.push_int(2)
                            .push_string("media.class")
                            .push_string("Stream/Output/Audio")
                            .push_string("target.object")
                            .push_string("2")
                    })
                    .push_int(8)
            }))
            .unwrap();
        let (bound, _) = read_message(&mut client).unwrap();
        assert_eq!((bound.id, bound.opcode), (CORE_ID, CORE_BOUND_PROPS));
        client
            .write_all(&request(8, 1, |b| b.push_int(3).push_int(9)))
            .unwrap();
        client
            .write_all(&request(8, 2, |b| {
                b.push_int(3).push_int(0).push_struct(|b| {
                    b.push_int(0)
                        .push_int(1)
                        .push_long(7)
                        .push_long(0)
                        .push_int(1)
                        .push_string("node.name")
                        .push_string("pw-cat")
                        .push_int(0)
                })
            }))
            .unwrap();

        let (add_mem, add_mem_fds, _) = read_message_with_fds(&mut client);
        assert_eq!((add_mem.id, add_mem.opcode), (CORE_ID, 6));
        assert_received_fds(add_mem_fds, 1, Some(ACTIVATION_SIZE as u64));
        let (transport, transport_fds, _) = read_message_with_fds(&mut client);
        assert_eq!((transport.id, transport.opcode), (8, 0));
        assert_received_fds(transport_fds, 2, None);
        client
            .write_all(&request(8, 4, |b| b.push_bool(true)))
            .unwrap();

        let mut format_storage = [0; 256];
        let registry = DeviceRegistry::register_virtual_playback();
        let format = RawPod::wrap(
            enum_format_pod_for(registry.playback().info().format(), &mut format_storage).unwrap(),
        )
        .unwrap();
        let port_update = client_port_update(8, &format, ParamType::EnumFormat);
        client.write_all(&port_update).unwrap();
        let (set_format, _) = read_message(&mut client).unwrap();
        assert_eq!((set_format.id, set_format.opcode), (8, 7));
        drop(client);
        assert!(worker.join().unwrap().unwrap().is_empty());
    }

    #[test]
    fn sends_real_shared_pcm_and_io_memory() {
        let (mut client, mut server) = UnixStream::pair().unwrap();
        let worker = thread::spawn(move || {
            let mut seq = 0;
            write_client_node_buffers(&mut server, 8, &mut seq).unwrap()
        });

        let (buffer_mem, buffer_mem_fds, _) = read_message_with_fds(&mut client);
        assert_eq!((buffer_mem.id, buffer_mem.opcode), (0, 6));
        assert_received_fds(
            buffer_mem_fds,
            1,
            Some((BUFFER_COUNT * BUFFER_STRIDE) as u64),
        );
        let (use_buffers, use_buffers_payload) = read_message(&mut client).unwrap();
        assert_eq!((use_buffers.id, use_buffers.opcode), (8, 8));
        let mut parser = Parser::new(&use_buffers_payload);
        parser
            .pop_struct(|fields| {
                assert_eq!((fields.pop_int()?, fields.pop_int()?), (1, 0));
                assert_eq!((fields.pop_int()?, fields.pop_int()?), (-1, 0));
                assert_eq!(fields.pop_int()?, BUFFER_COUNT);
                for index in 0..BUFFER_COUNT {
                    assert_eq!(fields.pop_int()?, 1);
                    assert_eq!(fields.pop_int()?, index * BUFFER_STRIDE);
                    assert_eq!(fields.pop_int()?, BUFFER_DATA_OFFSET + BUFFER_DATA_SIZE);
                    assert_eq!((fields.pop_int()?, fields.pop_int()?), (0, 1));
                    assert_eq!(fields.pop_id::<u32>()?.0, 1);
                    assert_eq!(fields.pop_int()?, BUFFER_DATA_OFFSET);
                    assert_eq!((fields.pop_int()?, fields.pop_int()?), (3, 0));
                    assert_eq!(fields.pop_int()?, BUFFER_DATA_SIZE);
                }
                require_empty(fields, "test PortUseBuffers")
            })
            .unwrap();

        let (io_mem, io_mem_fds, _) = read_message_with_fds(&mut client);
        assert_eq!((io_mem.id, io_mem.opcode), (0, 6));
        assert_received_fds(io_mem_fds, 1, Some(8));
        let (set_io, _) = read_message(&mut client).unwrap();
        assert_eq!((set_io.id, set_io.opcode), (8, 9));
        drop(client);
        let buffers = worker.join().unwrap();
        buffers
            .memory
            .write_all_at(&0_u32.to_ne_bytes(), 0)
            .unwrap();
        buffers
            .memory
            .write_all_at(&1920_u32.to_ne_bytes(), 4)
            .unwrap();
        buffers
            .memory
            .write_all_at(&[0; 1920], BUFFER_DATA_OFFSET as u64)
            .unwrap();
        let pcm = consume_client_buffer(&buffers, 0).unwrap();
        let mut registry = DeviceRegistry::register_virtual_playback();
        assert_eq!(
            process_pcm(&mut registry, &pcm).unwrap().frame_position,
            480
        );
    }

    #[test]
    fn persistent_worker_commits_quanta_and_mixes_active_streams() {
        let mut registry = DeviceRegistry::register_virtual_playback();
        let mut active = BTreeMap::new();
        handle_stream_event(&mut registry, &mut active, StreamEvent::Started(1));
        active.get_mut(&1).unwrap().solo = true;
        handle_stream_event(
            &mut registry,
            &mut active,
            StreamEvent::Chunk(1, vec![0; 240 * 4]),
        );
        assert_eq!(registry.playback().frame_position(), 240);
        handle_stream_event(
            &mut registry,
            &mut active,
            StreamEvent::Chunk(1, vec![0; 240 * 4]),
        );
        assert_eq!(registry.playback().frame_position(), 480);

        handle_stream_event(&mut registry, &mut active, StreamEvent::Started(2));
        handle_stream_event(
            &mut registry,
            &mut active,
            StreamEvent::Chunk(1, vec![0; 480 * 4]),
        );
        handle_stream_event(
            &mut registry,
            &mut active,
            StreamEvent::Chunk(2, vec![0; 480 * 4]),
        );
        assert_eq!(registry.playback().frame_position(), 960);
    }
}
