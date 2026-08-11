// SPDX-License-Identifier: GPL-2.0-only

//! Bounded Ethernet-II handoff between the pinned client MLME and Netstack3.
//!
//! The client MLME, not this module, owns 802.11 encapsulation/decapsulation,
//! controlled-port policy, and selection of protected data frames.

use netstack3_port_spike::{
    EthernetDevice, EthernetDeviceEvent, EthernetEventSource, EthernetFrame, FrameSizeError,
};
use std::collections::VecDeque;
use std::fmt;
use std::net::IpAddr;
use std::num::{NonZeroU16, NonZeroU64, NonZeroUsize};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use netstack3_port_integration::{
    Runtime,
    service::{DhcpService, DhcpStatus},
};
use netstack3_port_spike::{
    EthernetRunner, NetworkServiceEndpoint, RemoteIpAddress, RemoteIpVersion, RemoteSocketAddress,
    RemoteSocketProvider,
};
use rand::{SeedableRng as _, rngs::StdRng};

pub const MT7921_ETHERNET_MTU: u16 = 1500;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct EthernetPortProperties {
    pub mac_address: [u8; 6],
    pub mtu: u16,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum EthernetPortConfigError {
    ZeroQueueCapacity,
    InvalidMacAddress,
}

impl fmt::Display for EthernetPortConfigError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "invalid MT7921 Ethernet port configuration: {self:?}")
    }
}

impl std::error::Error for EthernetPortConfigError {}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum EthernetIngressError {
    Closed,
    LinkDown,
    Backpressure,
    InvalidFrame(FrameSizeError),
}

struct State {
    properties: Option<EthernetPortProperties>,
    capacity: usize,
    link_up: bool,
    ingress: VecDeque<EthernetFrame>,
    egress: VecDeque<EthernetFrame>,
    events: VecDeque<EthernetDeviceEvent>,
}

/// Netstack3-facing half of the port.
pub struct Mt7921EthernetDevice {
    state: Arc<Mutex<State>>,
}

/// MLME-facing outbound half. Its target is the pinned MLME's Ethernet TX
/// entry point; the target remains responsible for 802.11 encapsulation.
pub struct Mt7921EthernetTx {
    state: Arc<Mutex<State>>,
}

/// Private device-side half retained by `Mt7921ClientDevice`.
pub(crate) struct MlmeEthernetSink {
    state: Arc<Mutex<State>>,
}

pub trait AssociatedSoftmacTx {
    type Error;

    /// Accept one Ethernet II frame without FCS. Success transfers ownership
    /// to the associated SoftMAC path.
    fn transmit_ethernet(&mut self, frame: &[u8]) -> Result<(), Self::Error>;
}

/// Production RX companion to [`AssociatedSoftmacTx`]. One call admits at
/// most one already-validated associated data frame into the MLME Ethernet
/// sink; it must not wait beyond `deadline`.
pub trait AssociatedDataPump: AssociatedSoftmacTx {
    fn pump_receive(&mut self, deadline: std::time::Instant) -> Result<bool, Self::Error>;
}

#[derive(Clone)]
pub struct NetstackProofConfig {
    pub dns_name: String,
    pub server_port: NonZeroU16,
    pub http_request: Vec<u8>,
    pub expected_response_prefix: Vec<u8>,
}

/// The existing Netstack3 DHCP/DNS/socket stack wired to the MT7921 Ethernet
/// port. This is a bounded driver, not a DHCP, DNS, TCP, or HTTP implementation.
pub struct BoundedNetstackProof {
    runner: EthernetRunner<DhcpService, Mt7921EthernetDevice>,
    tx: Mt7921EthernetTx,
    config: NetstackProofConfig,
    now: Duration,
    resolved: Option<[u8; 4]>,
    socket: Option<netstack3_port_spike::RemoteSocketHandle>,
}

impl BoundedNetstackProof {
    pub fn new(
        device: Mt7921EthernetDevice,
        tx: Mt7921EthernetTx,
        config: NetstackProofConfig,
    ) -> Result<Self, &'static str> {
        let mac = device
            .properties()
            .ok_or("Ethernet port is closed")?
            .mac_address;
        let runtime = Runtime::new(
            32,
            (0u8..=255).cycle().take(8192),
            NonZeroU64::new(1).unwrap(),
            mac,
            u32::from(MT7921_ETHERNET_MTU),
        )
        .map_err(|_| "Netstack runtime initialization failed")?;
        Ok(Self {
            runner: EthernetRunner::new(
                DhcpService::new(runtime, StdRng::seed_from_u64(7), mac),
                device,
            ),
            tx,
            config,
            now: Duration::ZERO,
            resolved: None,
            socket: None,
        })
    }

    fn drive<T: AssociatedDataPump>(
        &mut self,
        target: &mut T,
        deadline: std::time::Instant,
    ) -> Result<(), &'static str> {
        if std::time::Instant::now() >= deadline {
            return Err("Netstack proof deadline");
        }
        for _ in 0..8 {
            while let Some(event) = self.runner.device_mut().take_event() {
                if event == netstack3_port_spike::EthernetDeviceEvent::LinkStateChanged(false) {
                    self.runner.discard_pending();
                    self.resolved = None;
                    self.socket = None;
                }
                self.runner.stack_mut().on_device_event(event);
            }
            self.runner.stack_mut().poll_at(self.now, 64);
            while self.runner.pump().transmitted != 0 {}
            while self
                .tx
                .pump_one(target)
                .map_err(|_| "associated data TX failed")?
            {}
            while target
                .pump_receive(deadline)
                .map_err(|_| "associated data RX failed")?
            {}
            while self.runner.pump().received != 0 {}
        }
        self.now += Duration::from_millis(100);
        Ok(())
    }

    pub fn prove_dhcp<T: AssociatedDataPump>(
        &mut self,
        target: &mut T,
        deadline: std::time::Instant,
    ) -> Result<(), &'static str> {
        while self.runner.stack().status() != DhcpStatus::Bound {
            self.drive(target, deadline)?;
        }
        Ok(())
    }

    pub fn prove_dns<T: AssociatedDataPump>(
        &mut self,
        target: &mut T,
        deadline: std::time::Instant,
    ) -> Result<(), &'static str> {
        if self.runner.stack().status() != DhcpStatus::Bound {
            return Err("DNS requires DHCP");
        }
        let lookup = self
            .runner
            .stack_mut()
            .lookup_ip(self.config.dns_name.clone())
            .map_err(|_| "DNS start failed")?;
        loop {
            self.drive(target, deadline)?;
            if let Some(result) = self.runner.stack_mut().take_lookup(lookup) {
                let addresses = result.map_err(|_| "DNS lookup failed")?;
                self.resolved = addresses.into_iter().find_map(|address| match address {
                    IpAddr::V4(v4) => Some(v4.octets()),
                    _ => None,
                });
                return self
                    .resolved
                    .map(|_| ())
                    .ok_or("DNS returned no IPv4 address");
            }
        }
    }

    pub fn prove_tcp<T: AssociatedDataPump>(
        &mut self,
        target: &mut T,
        deadline: std::time::Instant,
    ) -> Result<(), &'static str> {
        let address = self.resolved.ok_or("TCP requires DNS")?;
        let mut provider = self.runner.stack().socket_provider();
        let client = provider
            .open_client(NonZeroUsize::new(1).unwrap())
            .map_err(|_| "socket client failed")?;
        let socket = provider
            .tcp_socket(client, RemoteIpVersion::V4)
            .map_err(|_| "TCP socket failed")?;
        provider
            .tcp_connect(
                socket,
                RemoteSocketAddress {
                    address: RemoteIpAddress::V4(address),
                    port: self.config.server_port,
                },
            )
            .map_err(|_| "TCP connect failed")?;
        loop {
            self.drive(target, deadline)?;
            let ready = provider
                .readiness(socket)
                .map_err(|_| "TCP readiness failed")?;
            if ready.writable {
                self.socket = Some(socket);
                return Ok(());
            }
        }
    }

    pub fn prove_http<T: AssociatedDataPump>(
        &mut self,
        target: &mut T,
        deadline: std::time::Instant,
    ) -> Result<(), &'static str> {
        let socket = self.socket.ok_or("HTTP requires TCP")?;
        let mut provider = self.runner.stack().socket_provider();
        let written = provider
            .tcp_write(socket, &self.config.http_request)
            .map_err(|_| "HTTP request failed")?;
        if written != self.config.http_request.len() {
            return Err("partial HTTP request");
        }
        let mut response = vec![0; 4096];
        loop {
            self.drive(target, deadline)?;
            match provider.tcp_read(socket, &mut response) {
                Ok(read) if read != 0 => {
                    return response[..read]
                        .starts_with(&self.config.expected_response_prefix)
                        .then_some(())
                        .ok_or("unexpected HTTP response");
                }
                Ok(_) | Err(netstack3_port_spike::RemoteSocketError::WouldBlock) => {}
                Err(_) => return Err("HTTP response failed"),
            }
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum EthernetTxPumpError<E> {
    Closed,
    LinkDown,
    Target(E),
}

pub(crate) fn ethernet_port(
    mac_address: [u8; 6],
    queue_capacity: usize,
) -> Result<(Mt7921EthernetDevice, Mt7921EthernetTx, MlmeEthernetSink), EthernetPortConfigError> {
    if queue_capacity == 0 {
        return Err(EthernetPortConfigError::ZeroQueueCapacity);
    }
    if mac_address == [0; 6] || mac_address[0] & 1 != 0 {
        return Err(EthernetPortConfigError::InvalidMacAddress);
    }
    let state = Arc::new(Mutex::new(State {
        properties: Some(EthernetPortProperties {
            mac_address,
            mtu: MT7921_ETHERNET_MTU,
        }),
        capacity: queue_capacity,
        link_up: false,
        ingress: VecDeque::with_capacity(queue_capacity),
        egress: VecDeque::with_capacity(queue_capacity),
        events: VecDeque::with_capacity(queue_capacity.saturating_mul(2).saturating_add(1)),
    }));
    Ok((
        Mt7921EthernetDevice {
            state: state.clone(),
        },
        Mt7921EthernetTx {
            state: state.clone(),
        },
        MlmeEthernetSink { state },
    ))
}

impl Mt7921EthernetDevice {
    pub fn properties(&self) -> Option<EthernetPortProperties> {
        self.state.lock().unwrap().properties
    }
}

impl EthernetDevice for Mt7921EthernetDevice {
    fn receive(&mut self) -> Option<EthernetFrame> {
        self.state.lock().unwrap().ingress.pop_front()
    }

    fn transmit(&mut self, frame: EthernetFrame) -> Result<(), EthernetFrame> {
        let mut state = self.state.lock().unwrap();
        if state.properties.is_none() || !state.link_up || state.egress.len() == state.capacity {
            return Err(frame);
        }
        state.egress.push_back(frame);
        Ok(())
    }
}

impl EthernetEventSource for Mt7921EthernetDevice {
    fn take_event(&mut self) -> Option<EthernetDeviceEvent> {
        self.state.lock().unwrap().events.pop_front()
    }
}

impl Mt7921EthernetTx {
    /// Submit at most one queued frame. A rejected frame is restored at the
    /// head of the queue, so transient MLME backpressure is lossless.
    pub fn pump_one<T: AssociatedSoftmacTx>(
        &mut self,
        target: &mut T,
    ) -> Result<bool, EthernetTxPumpError<T::Error>> {
        let frame = {
            let mut state = self.state.lock().unwrap();
            if state.properties.is_none() {
                return Err(EthernetTxPumpError::Closed);
            }
            if !state.link_up {
                return Err(EthernetTxPumpError::LinkDown);
            }
            state.egress.pop_front()
        };
        let Some(frame) = frame else { return Ok(false) };
        if let Err(error) = target.transmit_ethernet(frame.as_bytes()) {
            self.state.lock().unwrap().egress.push_front(frame);
            return Err(EthernetTxPumpError::Target(error));
        }
        push_event(
            &mut self.state.lock().unwrap(),
            EthernetDeviceEvent::TransmitReady,
        );
        Ok(true)
    }
}

impl<D: wlan_mlme::device::DeviceOps> AssociatedSoftmacTx for wlan_mlme::client::ClientMlme<D> {
    type Error = anyhow::Error;

    fn transmit_ethernet(&mut self, frame: &[u8]) -> Result<(), Self::Error> {
        wlan_mlme::MlmeImpl::handle_eth_frame_tx(self, frame, fuchsia_trace::Id::new())
    }
}

/// The sole production associated-data owner. Both directions pass through
/// the same pinned `ClientMlme`: TX enters its associated Ethernet handler and
/// RX enters its raw MAC handler through the MT7921 runner. Consequently this
/// type cannot be constructed around `OpenClientMlme` or an independently
/// maintained association state.
pub struct PinnedAssociatedDataPump<'a, E, T> {
    mlme: &'a mut wlan_mlme::client::ClientMlme<
        crate::client_device::Mt7921ClientDevice<E, crate::Mt7921SoftmacAdapter<T>>,
    >,
    runner: &'a crate::client_device::Mt7921ScanRunner<E, T>,
}

#[derive(Debug)]
pub enum PinnedDataPumpError {
    Tx(anyhow::Error),
    Rx(zx::Status),
}

impl std::fmt::Display for PinnedDataPumpError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Tx(error) => write!(formatter, "pinned client Ethernet TX failed: {error}"),
            Self::Rx(status) => write!(formatter, "pinned client MAC RX failed: {status}"),
        }
    }
}

impl std::error::Error for PinnedDataPumpError {}

impl<'a, E, T> PinnedAssociatedDataPump<'a, E, T>
where
    E: crate::client_device::Mt7921ClientEffects,
    T: crate::Mt7921PassiveTransport,
{
    pub fn new(
        mlme: &'a mut wlan_mlme::client::ClientMlme<
            crate::client_device::Mt7921ClientDevice<E, crate::Mt7921SoftmacAdapter<T>>,
        >,
        runner: &'a crate::client_device::Mt7921ScanRunner<E, T>,
    ) -> Self {
        Self { mlme, runner }
    }
}

impl<E, T> AssociatedSoftmacTx for PinnedAssociatedDataPump<'_, E, T>
where
    E: crate::client_device::Mt7921ClientEffects,
    T: crate::Mt7921PassiveTransport,
{
    type Error = PinnedDataPumpError;

    fn transmit_ethernet(&mut self, frame: &[u8]) -> Result<(), Self::Error> {
        wlan_mlme::MlmeImpl::handle_eth_frame_tx(self.mlme, frame, fuchsia_trace::Id::new())
            .map_err(PinnedDataPumpError::Tx)
    }
}

impl<E, T> AssociatedDataPump for PinnedAssociatedDataPump<'_, E, T>
where
    E: crate::client_device::Mt7921ClientEffects,
    T: crate::Mt7921PassiveTransport,
{
    fn pump_receive(&mut self, deadline: std::time::Instant) -> Result<bool, Self::Error> {
        if std::time::Instant::now() >= deadline {
            return Err(PinnedDataPumpError::Rx(zx::Status::TIMED_OUT));
        }
        futures::executor::block_on(self.runner.pump_client_rx(self.mlme))
            .map_err(PinnedDataPumpError::Rx)
    }
}

impl MlmeEthernetSink {
    pub(crate) fn deliver(&mut self, bytes: &[u8]) -> Result<(), EthernetIngressError> {
        let frame =
            EthernetFrame::copy_from_slice(bytes).map_err(EthernetIngressError::InvalidFrame)?;
        let mut state = self.state.lock().unwrap();
        if state.properties.is_none() {
            return Err(EthernetIngressError::Closed);
        }
        if !state.link_up {
            return Err(EthernetIngressError::LinkDown);
        }
        if state.ingress.len() == state.capacity {
            return Err(EthernetIngressError::Backpressure);
        }
        state.ingress.push_back(frame);
        push_event(&mut state, EthernetDeviceEvent::ReceiveReady);
        Ok(())
    }

    pub(crate) fn set_link(&mut self, up: bool) {
        let mut state = self.state.lock().unwrap();
        if state.properties.is_some() && state.link_up != up {
            state.link_up = up;
            if !up {
                zeroize_frames(&mut state.ingress);
                zeroize_frames(&mut state.egress);
                state.events.retain(|event| {
                    !matches!(
                        event,
                        EthernetDeviceEvent::ReceiveReady | EthernetDeviceEvent::TransmitReady
                    )
                });
            }
            push_event(&mut state, EthernetDeviceEvent::LinkStateChanged(up));
        }
    }

    pub(crate) fn teardown(&mut self) {
        let mut state = self.state.lock().unwrap();
        if state.properties.is_none() {
            return;
        }
        state.events.clear();
        if state.link_up {
            push_event(&mut state, EthernetDeviceEvent::LinkStateChanged(false));
        }
        state.link_up = false;
        state.properties = None;
        zeroize_frames(&mut state.ingress);
        zeroize_frames(&mut state.egress);
    }
}

fn zeroize_frames(frames: &mut VecDeque<EthernetFrame>) {
    for frame in frames.drain(..) {
        let mut bytes = frame.into_vec();
        bytes.fill(0);
    }
}

fn push_event(state: &mut State, event: EthernetDeviceEvent) {
    if let EthernetDeviceEvent::LinkStateChanged(_) = event {
        state
            .events
            .retain(|queued| !matches!(queued, EthernetDeviceEvent::LinkStateChanged(_)));
    } else if state.events.contains(&event) {
        return;
    }
    state.events.push_back(event);
}

#[cfg(test)]
mod tests {
    use super::*;
    use netstack3_port_spike::{EthernetRunner, StackEthernetEndpoint};

    fn frame(ether_type: [u8; 2], marker: u8) -> EthernetFrame {
        let mut bytes = vec![0; 14 + 32];
        bytes[..6].copy_from_slice(&[0xff; 6]);
        bytes[6..12].copy_from_slice(&[2, 0, 0, 0, 0, 1]);
        bytes[12..14].copy_from_slice(&ether_type);
        bytes[14] = marker;
        EthernetFrame::try_from(bytes).unwrap()
    }

    #[derive(Default)]
    struct TxTarget {
        frames: Vec<Vec<u8>>,
        blocked: bool,
    }

    impl AssociatedSoftmacTx for TxTarget {
        type Error = ();
        fn transmit_ethernet(&mut self, frame: &[u8]) -> Result<(), Self::Error> {
            if self.blocked {
                Err(())
            } else {
                self.frames.push(frame.to_vec());
                Ok(())
            }
        }
    }

    #[derive(Default)]
    struct StackEndpoint(Vec<EthernetFrame>);

    impl StackEthernetEndpoint for StackEndpoint {
        fn receive_frame(&mut self, frame: EthernetFrame) -> Result<(), EthernetFrame> {
            self.0.push(frame);
            Ok(())
        }

        fn take_transmit(&mut self) -> Option<EthernetFrame> {
            None
        }
    }

    #[test]
    fn mlme_rx_enters_existing_netstack_device_boundary() {
        let (device, _, mut sink) = ethernet_port([2, 0, 0, 0, 0, 1], 2).unwrap();
        let mut runner = EthernetRunner::new(StackEndpoint::default(), device);
        sink.set_link(true);
        let ipv4 = frame([0x08, 0x00], 7);
        sink.deliver(ipv4.as_bytes()).unwrap();
        assert_eq!(
            runner.device_mut().take_event(),
            Some(EthernetDeviceEvent::LinkStateChanged(true))
        );
        assert_eq!(
            runner.device_mut().take_event(),
            Some(EthernetDeviceEvent::ReceiveReady)
        );
        assert_eq!(runner.pump().received, 1);
        assert_eq!(runner.stack().0, vec![ipv4]);
    }

    #[test]
    fn arp_dhcp_and_data_leave_through_softmac_tx_facade() {
        let (mut device, mut tx, mut sink) = ethernet_port([2, 0, 0, 0, 0, 1], 3).unwrap();
        sink.set_link(true);
        let expected = [
            frame([0x08, 0x06], 1),
            frame([0x08, 0x00], 2),
            frame([0x86, 0xdd], 3),
        ];
        for frame in expected.clone() {
            device.transmit(frame).unwrap()
        }
        let mut target = TxTarget::default();
        for _ in 0..3 {
            assert_eq!(tx.pump_one(&mut target), Ok(true));
        }
        assert_eq!(tx.pump_one(&mut target), Ok(false));
        assert_eq!(target.frames, expected.map(EthernetFrame::into_vec));
    }

    #[test]
    fn backpressure_link_lifecycle_and_teardown_are_bounded() {
        let (mut device, mut tx, mut sink) = ethernet_port([2, 0, 0, 0, 0, 1], 1).unwrap();
        assert_eq!(device.properties().unwrap().mtu, 1500);
        let arp = frame([0x08, 0x06], 1);
        assert_eq!(device.transmit(arp.clone()), Err(arp.clone()));
        sink.set_link(true);
        device.transmit(arp.clone()).unwrap();
        let data = frame([0x08, 0x00], 2);
        assert_eq!(device.transmit(data.clone()), Err(data));
        let mut target = TxTarget {
            blocked: true,
            ..Default::default()
        };
        assert_eq!(
            tx.pump_one(&mut target),
            Err(EthernetTxPumpError::Target(()))
        );
        target.blocked = false;
        assert_eq!(tx.pump_one(&mut target), Ok(true));
        sink.teardown();
        assert_eq!(device.properties(), None);
        assert_eq!(device.receive(), None);
        assert_eq!(device.transmit(arp.clone()), Err(arp));
        assert_eq!(tx.pump_one(&mut target), Err(EthernetTxPumpError::Closed));
    }

    #[test]
    fn link_down_discards_frames_and_stale_readiness_from_old_association() {
        let (mut device, mut tx, mut sink) = ethernet_port([2, 0, 0, 0, 0, 1], 2).unwrap();
        sink.set_link(true);
        sink.deliver(frame([0x08, 0x00], 1).as_bytes()).unwrap();
        device.transmit(frame([0x08, 0x06], 2)).unwrap();

        sink.set_link(false);

        assert_eq!(
            device.take_event(),
            Some(EthernetDeviceEvent::LinkStateChanged(false))
        );
        assert_eq!(device.take_event(), None);
        assert_eq!(device.receive(), None);
        assert_eq!(
            tx.pump_one(&mut TxTarget::default()),
            Err(EthernetTxPumpError::LinkDown)
        );
    }

    #[test]
    fn invalid_frames_and_addresses_do_not_cross_the_boundary() {
        assert!(matches!(
            ethernet_port([0; 6], 1),
            Err(EthernetPortConfigError::InvalidMacAddress)
        ));
        let (_, _, mut sink) = ethernet_port([2, 0, 0, 0, 0, 1], 1).unwrap();
        sink.set_link(true);
        assert_eq!(
            sink.deliver(&[0; 13]),
            Err(EthernetIngressError::InvalidFrame(
                FrameSizeError::TooShort { len: 13 }
            ))
        );
        assert_eq!(
            sink.deliver(&[0; 1515]),
            Err(EthernetIngressError::InvalidFrame(
                FrameSizeError::TooLong { len: 1515 }
            ))
        );
    }
}

#[cfg(test)]
#[path = "associated_runtime_test.rs"]
mod associated_runtime_test;
