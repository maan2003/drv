use netstack3_port_integration::{
    Runtime,
    ethernet_transport::{EthernetAttachment, EthernetInput, SeqpacketEthernet},
    service::{DhcpService, DhcpStatus},
    socket_provider::NativeSocketProvider,
};
use netstack3_port_spike::{
    EthernetFrame, NetworkServiceEndpoint as _, SocketClientId, StackEthernetEndpoint as _,
    provider_dispatch_v2::{
        ProviderDispatcherV2, RemoteSocketProviderV2 as _, encode_readiness_changed_v2,
    },
    provider_transport::{
        MAX_PROVIDER_PAYLOAD, PROVIDER_HEADER_LEN, ProviderIdentity, ProviderNamespaceId,
    },
    provider_transport_v2::{ProviderFramedEndpointV2, ProviderOpcodeV2},
};
use rand::{SeedableRng as _, rngs::StdRng};
use std::{
    collections::HashMap,
    env,
    fs::{File, OpenOptions},
    io::{self, ErrorKind, Read, Write},
    num::NonZeroU64,
    os::{
        fd::{FromRawFd as _, OwnedFd, RawFd},
        unix::fs::OpenOptionsExt as _,
    },
    path::Path,
    sync::mpsc::{self, RecvTimeoutError, TrySendError},
    thread,
    time::{Duration, Instant},
};

const DEFAULT_DEVICE: &str = "/dev/netstack3-provider";
const FRAME_LEN: usize = PROVIDER_HEADER_LEN + MAX_PROVIDER_PAYLOAD;
const QUEUE_CAPACITY: usize = 128;
const TICK: Duration = Duration::from_millis(10);

enum Input {
    Ethernet(EthernetFrame),
    LinkDown,
}

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

fn read_ethernet(ethernet: SeqpacketEthernet, sender: mpsc::SyncSender<io::Result<Input>>) {
    loop {
        let result = ethernet.receive().map(|input| match input {
            EthernetInput::Frame(frame) => Input::Ethernet(frame),
            EthernetInput::LinkDown => Input::LinkDown,
        });
        let failed = result.is_err();
        if sender.send(result).is_err() || failed {
            return;
        }
    }
}

fn write_ethernet(
    ethernet: SeqpacketEthernet,
    frames: mpsc::Receiver<EthernetFrame>,
    sender: mpsc::SyncSender<io::Result<Input>>,
) {
    while let Ok(frame) = frames.recv() {
        if let Err(error) = ethernet.transmit(&frame) {
            let _ = sender.send(Err(error));
            return;
        }
    }
}

fn queue_ethernet_frame(
    outbound: &mpsc::SyncSender<EthernetFrame>,
    pending: &mut Option<EthernetFrame>,
    next: Option<EthernetFrame>,
) -> io::Result<()> {
    if pending.is_none() {
        *pending = next;
    }
    if let Some(frame) = pending.take() {
        match outbound.try_send(frame) {
            Ok(()) => {}
            Err(TrySendError::Full(frame)) => *pending = Some(frame),
            Err(TrySendError::Disconnected(_)) => {
                return Err(io::Error::new(
                    ErrorKind::BrokenPipe,
                    "Ethernet writer stopped",
                ));
            }
        }
    }
    Ok(())
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

fn open_provider(path: &Path) -> io::Result<File> {
    OpenOptions::new()
        .read(true)
        .write(true)
        .custom_flags(libc::O_NONBLOCK)
        .open(path)
}

fn read_request(device: &mut File) -> io::Result<Option<Vec<u8>>> {
    let mut frame = vec![0; FRAME_LEN];
    loop {
        match device.read(&mut frame) {
            Ok(0) => {
                return Err(io::Error::new(
                    ErrorKind::BrokenPipe,
                    "kernel provider closed",
                ));
            }
            Ok(length) => {
                frame.truncate(length);
                return Ok(Some(frame));
            }
            Err(error) if error.kind() == ErrorKind::Interrupted => continue,
            Err(error) if error.kind() == ErrorKind::WouldBlock => return Ok(None),
            Err(error) => return Err(error),
        }
    }
}

fn reconcile_provider(
    status: DhcpStatus,
    device: &mut Option<File>,
    provider: &mut NativeSocketProvider,
    identities: &mut HashMap<SocketClientId, ProviderNamespaceId>,
    open: impl FnOnce() -> io::Result<File>,
) -> io::Result<()> {
    if status == DhcpStatus::Bound {
        if device.is_none() {
            *device = Some(open()?);
        }
    } else if device.is_some() {
        // Close the singleton kernel endpoint before revoking all userspace
        // clients, so new and live kernel sockets fail closed immediately.
        drop(device.take());
        for client in identities.keys().copied().collect::<Vec<_>>() {
            let _ = provider.close_client(client);
        }
        identities.clear();
    }
    Ok(())
}

fn entropy() -> io::Result<(Vec<u8>, [u8; 32])> {
    let mut bytes = vec![0; 8192 + 32];
    File::open("/dev/urandom")?.read_exact(&mut bytes)?;
    let seed = bytes.split_off(8192).try_into().unwrap();
    Ok((bytes, seed))
}

fn run(
    device_path: &Path,
    attachment: EthernetAttachment,
    ethernet: SeqpacketEthernet,
) -> io::Result<()> {
    let (runtime_entropy, dhcp_seed) = entropy()?;
    let runtime = Runtime::new(
        QUEUE_CAPACITY,
        runtime_entropy,
        NonZeroU64::new(1).unwrap(),
        attachment.mac,
        u32::from(attachment.mtu),
    )
    .map_err(invalid_data)?;
    let mut service = DhcpService::new(runtime, StdRng::from_seed(dhcp_seed), attachment.mac);
    let mut provider = service.socket_provider();

    // DHCP starts while the kernel provider remains inactive. The provider
    // becomes reachable only after address, routes, and DNS are all applied.
    let mut device = None;
    let (sender, inputs) = mpsc::sync_channel(QUEUE_CAPACITY);
    let (outbound, frames) = mpsc::sync_channel(QUEUE_CAPACITY);
    let ethernet_reader = ethernet.try_clone()?;
    let ethernet_writer = ethernet;
    thread::spawn({
        let sender = sender.clone();
        move || read_ethernet(ethernet_reader, sender)
    });
    thread::spawn({
        let sender = sender.clone();
        move || write_ethernet(ethernet_writer, frames, sender)
    });
    let started = Instant::now();
    let mut identities = HashMap::new();
    let mut pending_frame = None;

    loop {
        match inputs.recv_timeout(TICK) {
            Ok(Ok(Input::Ethernet(frame))) => service
                .receive_frame(frame)
                .map_err(|_| invalid_data("runtime rejected Ethernet frame"))?,
            Ok(Ok(Input::LinkDown)) => return Ok(()),
            Ok(Err(error)) => return Err(error),
            Err(RecvTimeoutError::Disconnected) => return Ok(()),
            Err(RecvTimeoutError::Timeout) => {}
        }
        service.poll_at(started.elapsed(), QUEUE_CAPACITY);
        reconcile_provider(
            service.status(),
            &mut device,
            &mut provider,
            &mut identities,
            || open_provider(device_path),
        )?;
        if let Some(device) = device.as_mut() {
            for _ in 0..QUEUE_CAPACITY {
                let Some(request) = read_request(device)? else {
                    break;
                };
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
            write_readiness_events(device, &mut provider, &identities)?;
        }
        let next_frame = pending_frame
            .is_none()
            .then(|| service.take_transmit())
            .flatten();
        queue_ethernet_frame(&outbound, &mut pending_frame, next_frame)?;
    }
}

fn main() -> io::Result<()> {
    let args: Vec<_> = env::args().skip(1).collect();
    let (device, ethernet_fd) = match args.as_slice() {
        [ethernet_fd] => (DEFAULT_DEVICE, ethernet_fd.as_str()),
        [device, ethernet_fd] => (device.as_str(), ethernet_fd.as_str()),
        _ => {
            return Err(invalid_data(
                "usage: netstack3-provider-daemon [DEVICE] ETHERNET_SEQPACKET_FD",
            ));
        }
    };
    let ethernet_fd: RawFd = ethernet_fd.parse().map_err(invalid_data)?;
    if ethernet_fd <= libc::STDERR_FILENO {
        return Err(invalid_data("Ethernet fd must be above stdio"));
    }
    // SAFETY: the CLI contract transfers one owned descriptor to this process.
    let ethernet = SeqpacketEthernet::from_owned_fd(unsafe { OwnedFd::from_raw_fd(ethernet_fd) })?;
    let attachment = ethernet.attach()?;
    run(Path::new(device), attachment, ethernet)
}

#[cfg(test)]
mod tests {
    use super::*;
    use netstack3_port_spike::{
        RemoteIpAddress, RemoteIpVersion, SocketClientId,
        provider_transport::{ProviderFrameType, ProviderNamespaceId},
        provider_transport_v2::{ProviderFrameV2, ProviderSocketAddressV2, ProviderSocketKindV2},
    };
    use std::{
        cell::{Cell, RefCell},
        num::NonZeroU16,
        rc::Rc,
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

    #[test]
    fn provider_operation_crosses_the_live_ethernet_endpoint() {
        let client_mac = [2, 0, 0, 0, 0, 1];
        let server_mac = [2, 0, 0, 0, 0, 2];
        let mut client = Runtime::new(
            8,
            (0u8..=255).cycle().take(8192),
            NonZeroU64::new(1).unwrap(),
            client_mac,
            1500,
        )
        .unwrap();
        client.apply_ipv4([192, 0, 2, 1], 24, None).unwrap();
        let mut service = DhcpService::new(client, StdRng::from_seed([7; 32]), client_mac);
        let mut provider = service.socket_provider();

        let mut server = Runtime::new(
            8,
            (0u8..=255).rev().cycle().take(8192),
            NonZeroU64::new(2).unwrap(),
            server_mac,
            1500,
        )
        .unwrap();
        server.apply_ipv4([192, 0, 2, 2], 24, None).unwrap();
        let server_socket = server.udp_socket().unwrap();
        server
            .udp_bind(
                server_socket,
                Some([192, 0, 2, 2]),
                NonZeroU16::new(10002).unwrap(),
            )
            .unwrap();

        let client_id = SocketClientId::from_raw(5);
        provider.open_client(client_id, 1).unwrap();
        let socket = provider
            .open_socket(client_id, ProviderSocketKindV2::Udp, RemoteIpVersion::V4)
            .unwrap();
        provider
            .bind(socket, Some(RemoteIpAddress::V4([192, 0, 2, 1])), 10001)
            .unwrap();
        provider
            .send_msg(
                socket,
                0,
                Some(ProviderSocketAddressV2 {
                    address: RemoteIpAddress::V4([192, 0, 2, 2]),
                    port: 10002,
                }),
                b"provider Ethernet",
            )
            .unwrap();

        let arp = service
            .take_transmit()
            .expect("provider send starts address resolution");
        assert_eq!(&arp.as_bytes()[12..14], &[0x08, 0x06]);
        server.receive_frame(arp);
        for _ in 0..16 {
            let mut exchanged = 0;
            while let Some(frame) = server.take_tx() {
                service.receive_frame(frame).unwrap();
                exchanged += 1;
            }
            while let Some(frame) = service.take_transmit() {
                server.receive_frame(frame);
                exchanged += 1;
            }
            if exchanged == 0 {
                break;
            }
        }
        assert_eq!(
            server.udp_receive(server_socket).unwrap().as_deref(),
            Some(&b"provider Ethernet"[..])
        );
    }

    #[test]
    fn provider_ownership_follows_usable_configuration_and_revokes_clients() {
        let runtime = Runtime::new(
            8,
            [1; 8192],
            NonZeroU64::new(1).unwrap(),
            [2, 0, 0, 0, 0, 1],
            1500,
        )
        .unwrap();
        let mut provider = NativeSocketProvider::new(Rc::new(RefCell::new(runtime)));
        let mut device = None;
        let mut identities = HashMap::new();
        let opens = Cell::new(0);
        let open = || {
            opens.set(opens.get() + 1);
            File::open("/dev/null")
        };

        reconcile_provider(
            DhcpStatus::Acquiring,
            &mut device,
            &mut provider,
            &mut identities,
            open,
        )
        .unwrap();
        assert!(device.is_none());
        assert_eq!(
            opens.get(),
            0,
            "link-up without a lease must not capture sockets"
        );

        reconcile_provider(
            DhcpStatus::Bound,
            &mut device,
            &mut provider,
            &mut identities,
            open,
        )
        .unwrap();
        assert!(device.is_some());
        assert_eq!(opens.get(), 1);

        let client = SocketClientId::from_raw(19);
        provider.open_client(client, 1).unwrap();
        provider
            .open_socket(client, ProviderSocketKindV2::Udp, RemoteIpVersion::V4)
            .unwrap();
        identities.insert(client, ProviderNamespaceId::from_raw(23));

        reconcile_provider(
            DhcpStatus::Acquiring,
            &mut device,
            &mut provider,
            &mut identities,
            open,
        )
        .unwrap();
        assert!(device.is_none());
        assert!(identities.is_empty());
        assert!(
            provider
                .open_socket(client, ProviderSocketKindV2::Udp, RemoteIpVersion::V4)
                .is_err(),
            "lease loss revokes live sockets"
        );

        reconcile_provider(
            DhcpStatus::Bound,
            &mut device,
            &mut provider,
            &mut identities,
            open,
        )
        .unwrap();
        assert!(device.is_some());
        assert_eq!(
            opens.get(),
            2,
            "configuration recovery opens a new generation"
        );
    }

    #[test]
    fn ethernet_writer_backpressure_retains_exactly_one_frame() {
        let (sender, receiver) = mpsc::sync_channel(1);
        let first = EthernetFrame::try_from(vec![1; 14]).unwrap();
        let second = EthernetFrame::try_from(vec![2; 14]).unwrap();
        sender.send(first.clone()).unwrap();
        let mut pending = None;
        queue_ethernet_frame(&sender, &mut pending, Some(second.clone())).unwrap();
        assert_eq!(pending, Some(second.clone()));
        assert_eq!(receiver.recv().unwrap(), first);
        queue_ethernet_frame(&sender, &mut pending, None).unwrap();
        assert!(pending.is_none());
        assert_eq!(receiver.recv().unwrap(), second);
    }
}
