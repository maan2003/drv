use netstack3_port_integration::{NativeInstant, Runtime, socket_provider::NativeSocketProvider};
use netstack3_port_spike::{
    SocketClientId,
    provider_dispatch_v2::{ProviderDispatcherV2, encode_readiness_changed_v2},
    provider_transport::{
        MAX_PROVIDER_PAYLOAD, PROVIDER_HEADER_LEN, ProviderIdentity, ProviderNamespaceId,
    },
    provider_transport_v2::{ProviderFramedEndpointV2, ProviderOpcodeV2},
};
use std::{
    cell::RefCell,
    collections::HashMap,
    env,
    fs::{File, OpenOptions},
    io::{self, ErrorKind, Read, Write},
    num::NonZeroU64,
    path::Path,
    rc::Rc,
    sync::mpsc::{self, RecvTimeoutError},
    thread,
    time::{Duration, Instant},
};

const DEFAULT_DEVICE: &str = "/dev/netstack3-provider";
const FRAME_LEN: usize = PROVIDER_HEADER_LEN + MAX_PROVIDER_PAYLOAD;
const QUEUE_CAPACITY: usize = 128;
const TICK: Duration = Duration::from_millis(10);

fn invalid_data(error: impl std::fmt::Debug) -> io::Error {
    io::Error::new(ErrorKind::InvalidData, format!("provider ABI: {error:?}"))
}

fn dispatch(
    provider: &NativeSocketProvider,
    request: &[u8],
) -> io::Result<(ProviderIdentity, ProviderOpcodeV2, bool, Vec<u8>)> {
    let endpoint = ProviderFramedEndpointV2::from_received_frame(request, MAX_PROVIDER_PAYLOAD)
        .map_err(invalid_data)?;
    let identity = endpoint.identity();
    let opcode = endpoint.decode(request).map_err(invalid_data)?.opcode;
    let response = ProviderDispatcherV2::new(endpoint, provider.clone())
        .dispatch(request)
        .map_err(invalid_data)?;
    let endpoint =
        ProviderFramedEndpointV2::new(identity, MAX_PROVIDER_PAYLOAD).map_err(invalid_data)?;
    let success = endpoint
        .decode(&response)
        .map_err(invalid_data)?
        .payload
        .first()
        == Some(&0);
    Ok((identity, opcode, success, response))
}

fn read_requests(mut device: File, sender: mpsc::SyncSender<io::Result<Vec<u8>>>) {
    loop {
        let mut frame = vec![0; FRAME_LEN];
        let result = loop {
            match device.read(&mut frame) {
                Err(error) if error.kind() == ErrorKind::Interrupted => continue,
                Ok(0) => return,
                Ok(length) => {
                    frame.truncate(length);
                    break Ok(frame);
                }
                Err(error) => break Err(error),
            }
        };
        let failed = result.is_err();
        if sender.send(result).is_err() || failed {
            return;
        }
    }
}

fn write_readiness_events(
    device: &mut File,
    provider: &mut NativeSocketProvider,
    identities: &HashMap<SocketClientId, ProviderNamespaceId>,
) -> io::Result<()> {
    for (client, handle, snapshot) in provider.take_readiness_changes() {
        let namespace = identities
            .get(&client)
            .copied()
            .ok_or_else(|| invalid_data("readiness for unknown client"))?;
        let endpoint = ProviderFramedEndpointV2::new(
            ProviderIdentity { namespace, client },
            MAX_PROVIDER_PAYLOAD,
        )
        .map_err(invalid_data)?;
        let event =
            encode_readiness_changed_v2(&endpoint, handle, snapshot).map_err(invalid_data)?;
        device.write_all(&event)?;
    }
    Ok(())
}

fn parse_mac(value: &str) -> io::Result<[u8; 6]> {
    let bytes: Vec<_> = value
        .split(':')
        .map(|part| u8::from_str_radix(part, 16))
        .collect::<Result<_, _>>()
        .map_err(invalid_data)?;
    bytes
        .try_into()
        .map_err(|_| invalid_data("MAC must contain six octets"))
}

fn entropy() -> io::Result<Vec<u8>> {
    let mut bytes = vec![0; 8192];
    File::open("/dev/urandom")?.read_exact(&mut bytes)?;
    Ok(bytes)
}

fn run(device_path: &Path, mac: [u8; 6]) -> io::Result<()> {
    let mut device = OpenOptions::new()
        .read(true)
        .write(true)
        .open(device_path)?;
    let reader = device.try_clone()?;
    let runtime = Rc::new(RefCell::new(
        Runtime::new(
            QUEUE_CAPACITY,
            entropy()?,
            NonZeroU64::new(1).unwrap(),
            mac,
            1500,
        )
        .map_err(invalid_data)?,
    ));
    let mut provider = NativeSocketProvider::new(runtime.clone());
    let (sender, requests) = mpsc::sync_channel(QUEUE_CAPACITY);
    thread::spawn(move || read_requests(reader, sender));
    let started = Instant::now();
    let mut identities = HashMap::new();

    loop {
        match requests.recv_timeout(TICK) {
            Ok(Ok(request)) => {
                let (identity, opcode, success, response) = dispatch(&provider, &request)?;
                match identities.get(&identity.client) {
                    Some(namespace) if *namespace != identity.namespace => {
                        return Err(invalid_data("client changed namespace"));
                    }
                    Some(_) => {}
                    None if success && opcode == ProviderOpcodeV2::OpenClient => {
                        identities.insert(identity.client, identity.namespace);
                    }
                    None => {}
                }
                device.write_all(&response)?;
                if success && opcode == ProviderOpcodeV2::CloseClient {
                    identities.remove(&identity.client);
                }
            }
            Ok(Err(error)) => return Err(error),
            Err(RecvTimeoutError::Disconnected) => return Ok(()),
            Err(RecvTimeoutError::Timeout) => {}
        }
        let nanos = u64::try_from(started.elapsed().as_nanos()).unwrap_or(u64::MAX);
        let mut runtime = runtime.borrow_mut();
        runtime.set_now(NativeInstant::from_nanos(nanos));
        runtime.dispatch_due(QUEUE_CAPACITY);
        drop(runtime);
        write_readiness_events(&mut device, &mut provider, &identities)?;
    }
}

fn main() -> io::Result<()> {
    let args: Vec<_> = env::args().skip(1).collect();
    let (device, mac) = match args.as_slice() {
        [mac] => (DEFAULT_DEVICE, mac.as_str()),
        [device, mac] => (device.as_str(), mac.as_str()),
        _ => {
            return Err(invalid_data(
                "usage: netstack3-provider-daemon [DEVICE] MAC",
            ));
        }
    };
    run(Path::new(device), parse_mac(mac)?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use netstack3_port_spike::{
        provider_transport::{ProviderFrameType, ProviderNamespaceId},
        provider_transport_v2::ProviderFrameV2,
    };

    #[test]
    fn dispatches_a_device_client_through_the_native_provider() {
        let runtime = Runtime::new(
            8,
            (0u8..=255).cycle().take(8192),
            NonZeroU64::new(1).unwrap(),
            [2, 0, 0, 0, 0, 1],
            1500,
        )
        .unwrap();
        let mut provider = NativeSocketProvider::new(Rc::new(RefCell::new(runtime)));
        let endpoint = ProviderFramedEndpointV2::new(
            ProviderIdentity {
                namespace: ProviderNamespaceId::from_raw(7),
                client: SocketClientId::from_raw(9),
            },
            MAX_PROVIDER_PAYLOAD,
        )
        .unwrap();
        let request = endpoint
            .encode(&ProviderFrameV2 {
                frame_type: ProviderFrameType::Request,
                opcode: ProviderOpcodeV2::OpenClient,
                request_id: 11,
                payload: 2u32.to_le_bytes().to_vec(),
            })
            .unwrap();
        let (_, _, _, bytes) = dispatch(&provider, &request).unwrap();
        let response = endpoint.decode(&bytes).unwrap();
        assert_eq!(response.request_id, 11);
        assert_eq!(response.payload, [0]);

        let request = endpoint
            .encode(&ProviderFrameV2 {
                frame_type: ProviderFrameType::Request,
                opcode: ProviderOpcodeV2::OpenSocket,
                request_id: 12,
                payload: vec![1, 4],
            })
            .unwrap();
        let (_, _, _, bytes) = dispatch(&provider, &request).unwrap();
        let response = endpoint.decode(&bytes).unwrap();
        let raw_handle = u64::from_le_bytes(response.payload[1..9].try_into().unwrap());
        let changes = provider.take_readiness_changes();
        let [(client, handle, snapshot)] = changes.as_slice() else {
            panic!("new socket must publish initial readiness");
        };
        assert_eq!(*client, SocketClientId::from_raw(9));
        assert_eq!(handle.into_raw(), raw_handle);
        let event = encode_readiness_changed_v2(&endpoint, *handle, *snapshot).unwrap();
        let event = endpoint.decode(&event).unwrap();
        assert_eq!(event.frame_type, ProviderFrameType::ReadinessEvent);
        assert_eq!(event.opcode, ProviderOpcodeV2::ReadinessChanged);
        assert_eq!(event.payload.len(), 19);
    }

    #[test]
    fn rejects_malformed_device_frames_before_dispatch() {
        let runtime = Runtime::new(
            1,
            [1; 8192],
            NonZeroU64::new(1).unwrap(),
            [2, 0, 0, 0, 0, 1],
            1500,
        )
        .unwrap();
        let provider = NativeSocketProvider::new(Rc::new(RefCell::new(runtime)));
        assert_eq!(
            dispatch(&provider, b"short").unwrap_err().kind(),
            ErrorKind::InvalidData
        );
    }
}
