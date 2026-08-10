//! Minimal server side of the PipeWire native discovery protocol.

use std::{
    io::{self, Read, Write},
    os::unix::net::{UnixListener, UnixStream},
};

use pipewire_native_spa::pod::{builder::Builder, parser::Parser};

const HEADER_LEN: usize = 16;
const MAX_PAYLOAD: usize = 64 * 1024;
const CORE_ID: u32 = 0;
const CLIENT_ID: u32 = 1;
const CORE_SYNC: u8 = 2;
const CORE_GET_REGISTRY: u8 = 5;
const CLIENT_UPDATE_PROPERTIES: u8 = 2;
const CORE_DONE: u8 = 1;
const REGISTRY_GLOBAL: u8 = 0;
const PERMISSIONS_RWX: i32 = (1 << 8) | (1 << 7) | (1 << 6);

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

/// Accept one standard PipeWire native client and finish after its post-registry sync.
pub fn serve_one(listener: &UnixListener) -> io::Result<()> {
    let (mut stream, _) = listener.accept()?;
    serve_connection(&mut stream)
}

fn serve_connection(stream: &mut UnixStream) -> io::Result<()> {
    let mut out_seq = 0;
    let mut registry_created = false;

    loop {
        let (header, payload) = read_message(stream)?;
        match (header.id, header.opcode) {
            (CORE_ID, CORE_GET_REGISTRY) => {
                let registry_id = decode_get_registry(&payload)?;
                write_global(
                    stream,
                    registry_id,
                    &mut out_seq,
                    VIRTUAL_SINK_NODE_ID,
                    "PipeWire:Interface:Node",
                    NODE_PROPERTIES,
                )?;
                write_global(
                    stream,
                    registry_id,
                    &mut out_seq,
                    VIRTUAL_SINK_PORT_ID,
                    "PipeWire:Interface:Port",
                    PORT_PROPERTIES,
                )?;
                registry_created = true;
            }
            (CORE_ID, CORE_SYNC) => {
                let (id, seq) = decode_sync(&payload)?;
                let body = encode_struct(|builder| builder.push_int(id).push_int(seq))?;
                write_message(stream, CORE_ID, CORE_DONE, out_seq, &body)?;
                out_seq += 1;
                if registry_created {
                    return Ok(());
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
        worker.join().unwrap();
    }
}
