//! Minimal server side of the PipeWire native discovery protocol.

use std::{
    io::{self, Read, Write},
    os::unix::net::{UnixListener, UnixStream},
    time::Duration,
};

use pipewire_native_spa::{
    param::ParamType,
    pod::{
        RawPod,
        builder::{Builder, StructBuilder},
        parser::Parser,
        types::Id,
    },
};

use crate::enum_format_pod;

const HEADER_LEN: usize = 16;
const MAX_PAYLOAD: usize = 64 * 1024;
const CORE_ID: u32 = 0;
const CLIENT_ID: u32 = 1;
const CORE_SYNC: u8 = 2;
const CORE_GET_REGISTRY: u8 = 5;
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

const VIRTUAL_SINK_NODE_ID: i32 = 2;
const VIRTUAL_SINK_PORT_ID: i32 = 3;

const NODE_PROPERTIES: &[(&str, &str)] = &[
    ("object.serial", "2"),
    ("node.name", "drv.virtual-sink"),
    ("node.description", "drv Virtual Sink"),
    ("media.class", "Audio/Sink"),
    ("audio.format", "S16LE"),
    ("audio.rate", "48000"),
    ("audio.channels", "2"),
];

const PORT_PROPERTIES: &[(&str, &str)] = &[
    ("object.serial", "3"),
    ("node.id", "2"),
    ("port.id", "0"),
    ("port.name", "playback"),
    ("port.direction", "in"),
    ("port.alias", "drv.virtual-sink:playback"),
];

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
}

#[derive(Clone, Copy, Debug)]
struct BoundObject {
    proxy_id: u32,
    kind: BoundKind,
}

/// Accept one standard PipeWire native client and finish after its post-registry sync.
pub fn serve_one(listener: &UnixListener) -> io::Result<()> {
    let (mut stream, _) = listener.accept()?;
    serve_connection(&mut stream)
}

fn serve_connection(stream: &mut UnixStream) -> io::Result<()> {
    let mut out_seq = 0;
    let mut registry_id = None;
    let mut bound_objects: Vec<BoundObject> = Vec::new();

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
            let object = decode_bind(&payload)?;
            if Some(object.proxy_id) == registry_id
                || bound_objects
                    .iter()
                    .any(|bound| bound.proxy_id == object.proxy_id)
            {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "duplicate PipeWire proxy id",
                ));
            }
            write_object_info(stream, object, &mut out_seq)?;
            bound_objects.push(object);
            continue;
        }

        if let Some(object) = bound_objects
            .iter()
            .find(|object| object.proxy_id == header.id)
            .copied()
        {
            match header.opcode {
                OBJECT_ENUM_PARAMS => {
                    let request = decode_enum_params(&payload)?;
                    write_enum_format(stream, object.proxy_id, &mut out_seq, request)?;
                }
                // Clients may subscribe immediately after binding. The fixed
                // format has already been described in Info and never changes.
                OBJECT_SUBSCRIBE_PARAMS => decode_subscribe_params(&payload)?,
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
                    VIRTUAL_SINK_NODE_ID,
                    "PipeWire:Interface:Node",
                    NODE_PROPERTIES,
                )?;
                write_global(
                    stream,
                    new_registry_id,
                    &mut out_seq,
                    VIRTUAL_SINK_PORT_ID,
                    "PipeWire:Interface:Port",
                    PORT_PROPERTIES,
                )?;
                registry_id = Some(new_registry_id);
            }
            (CORE_ID, CORE_SYNC) => {
                let (id, seq) = decode_sync(&payload)?;
                let body = encode_struct(|builder| builder.push_int(id).push_int(seq))?;
                write_message(stream, CORE_ID, CORE_DONE, out_seq, &body)?;
                out_seq += 1;
                if registry_id.is_some() {
                    // The stock CLI sends Bind only after processing this Done.
                    // Keep the one-shot probe alive briefly for that request.
                    stream.set_read_timeout(Some(Duration::from_millis(250)))?;
                }
            }
            // Hello and Client.UpdateProperties are required connection bootstrap
            // messages but have no response in this discovery-only milestone.
            (CORE_ID, 1) => decode_hello(&payload)?,
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

fn decode_bind(payload: &[u8]) -> io::Result<BoundObject> {
    let mut parser = Parser::new(payload);
    parser
        .pop_struct(|fields| {
            let global_id = fields.pop_int()?;
            let interface = fields.pop_string()?;
            let version = fields.pop_int()?;
            let proxy_id = fields.pop_int()?;
            let kind = match (global_id, interface.as_str()) {
                (VIRTUAL_SINK_NODE_ID, "PipeWire:Interface:Node") => BoundKind::Node,
                (VIRTUAL_SINK_PORT_ID, "PipeWire:Interface:Port") => BoundKind::Port,
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
    properties: &[(&str, &str)],
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

fn write_object_info(
    stream: &mut UnixStream,
    object: BoundObject,
    out_seq: &mut u32,
) -> io::Result<()> {
    let body = match object.kind {
        BoundKind::Node => encode_struct(|builder| {
            push_param_info(push_properties(
                builder
                    .push_int(VIRTUAL_SINK_NODE_ID)
                    .push_int(1)
                    .push_int(0)
                    .push_long(0x1d)
                    .push_int(1)
                    .push_int(0)
                    .push_id(Id(2_u32))
                    .push_none(),
                NODE_PROPERTIES,
            ))
        })?,
        BoundKind::Port => encode_struct(|builder| {
            push_param_info(push_properties(
                builder
                    .push_int(VIRTUAL_SINK_PORT_ID)
                    .push_int(0)
                    .push_long(0x3),
                PORT_PROPERTIES,
            ))
        })?,
    };
    write_message(stream, object.proxy_id, OBJECT_INFO, *out_seq, &body)?;
    *out_seq += 1;
    Ok(())
}

fn write_enum_format(
    stream: &mut UnixStream,
    proxy_id: u32,
    out_seq: &mut u32,
    request: EnumParamsRequest,
) -> io::Result<()> {
    let mut storage = [0; 256];
    let format = enum_format_pod(&mut storage).map_err(invalid_pod)?;
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

fn push_properties<'a>(
    builder: StructBuilder<'a>,
    properties: &[(&str, &str)],
) -> StructBuilder<'a> {
    builder.push_struct(|mut pair_list| {
        pair_list = pair_list.push_int(properties.len() as i32);
        for (key, value) in properties {
            pair_list = pair_list.push_string(key).push_string(value);
        }
        pair_list
    })
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

fn invalid_pod(error: pipewire_native_spa::pod::Error) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, format!("{error:?}"))
}

#[cfg(test)]
mod tests {
    use std::{os::unix::net::UnixStream, thread};

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

    #[test]
    fn get_registry_advertises_virtual_sink_node_and_input_port() {
        let (mut client, mut server) = UnixStream::pair().unwrap();
        let worker = thread::spawn(move || serve_connection(&mut server).unwrap());

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

        let (node_header, node_payload) = read_message(&mut client).unwrap();
        let (port_header, port_payload) = read_message(&mut client).unwrap();
        assert_eq!((node_header.id, node_header.opcode), (7, REGISTRY_GLOBAL));
        assert_eq!((port_header.id, port_header.opcode), (7, REGISTRY_GLOBAL));

        let node = decode_global(&node_payload);
        assert_eq!(node.0, VIRTUAL_SINK_NODE_ID);
        assert_eq!(node.1, "PipeWire:Interface:Node");
        assert!(
            node.2
                .contains(&("media.class".into(), "Audio/Sink".into()))
        );

        let port = decode_global(&port_payload);
        assert_eq!(port.0, VIRTUAL_SINK_PORT_ID);
        assert_eq!(port.1, "PipeWire:Interface:Port");
        assert!(port.2.contains(&("port.direction".into(), "in".into())));
        assert!(port.2.contains(&("node.id".into(), "2".into())));

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
        let worker = thread::spawn(move || serve_connection(&mut server).unwrap());

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
        read_message(&mut client).unwrap();
        read_message(&mut client).unwrap();
        client
            .write_all(&request(CORE_ID, CORE_SYNC, |b| b.push_int(0).push_int(1)))
            .unwrap();
        read_message(&mut client).unwrap();

        client
            .write_all(&request(7, REGISTRY_BIND, |b| {
                b.push_int(VIRTUAL_SINK_NODE_ID)
                    .push_string("PipeWire:Interface:Node")
                    .push_int(3)
                    .push_int(8)
            }))
            .unwrap();
        let (node_header, node_info) = read_message(&mut client).unwrap();
        assert_eq!((node_header.id, node_header.opcode), (8, OBJECT_INFO));
        let mut node_parser = Parser::new(&node_info);
        node_parser
            .pop_struct(|fields| {
                assert_eq!(fields.pop_int()?, VIRTUAL_SINK_NODE_ID);
                assert_eq!((fields.pop_int()?, fields.pop_int()?), (1, 0));
                assert_eq!(fields.pop_long()?, 0x1d);
                assert_eq!((fields.pop_int()?, fields.pop_int()?), (1, 0));
                assert_eq!(fields.pop_id::<u32>()?.0, 2);
                fields.pop_none()?;
                assert_eq!(
                    fields.pop_struct(|p| p.pop_int())?.0,
                    NODE_PROPERTIES.len() as i32
                );
                let (param_count, _) = fields.pop_struct(|p| p.pop_int())?;
                assert_eq!(param_count, 1);
                Ok(())
            })
            .unwrap();

        client
            .write_all(&request(7, REGISTRY_BIND, |b| {
                b.push_int(VIRTUAL_SINK_PORT_ID)
                    .push_string("PipeWire:Interface:Port")
                    .push_int(3)
                    .push_int(9)
            }))
            .unwrap();
        let (port_header, port_info) = read_message(&mut client).unwrap();
        assert_eq!((port_header.id, port_header.opcode), (9, OBJECT_INFO));
        let mut port_parser = Parser::new(&port_info);
        port_parser
            .pop_struct(|fields| {
                assert_eq!(fields.pop_int()?, VIRTUAL_SINK_PORT_ID);
                assert_eq!(fields.pop_int()?, 0);
                assert_eq!(fields.pop_long()?, 0x3);
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
                let expected = enum_format_pod(&mut expected_storage)?;
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
}
