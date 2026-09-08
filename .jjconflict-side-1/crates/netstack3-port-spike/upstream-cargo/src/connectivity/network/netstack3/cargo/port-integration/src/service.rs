//! Native adapters for Fuchsia's production DHCP client core.

use crate::dns_bridge::{DnsLookupHandle, DnsLookupResult, NativeDnsBridge};
use crate::socket_provider::NativeSocketProvider;
use crate::{NativeInstant, NativeIpAddress, Runtime, RuntimeError, UdpSocketHandle};
use dhcp_client_core::{
    client::{
        AddressAssignmentState, AddressEvent, ClientConfig, DebugLogPrefix, State, Step,
        TransitionEffect,
    },
    deps::{
        Clock, DatagramInfo, Instant as DhcpInstant, PacketSocketProvider, RngProvider, Socket,
        SocketError, UdpSocketProvider,
    },
    inspect::Counters,
    parse::{OptionCodeMap, OptionRequested},
};
use dhcp_protocol::{DhcpOption, OptionCode};
use futures::{StreamExt as _, channel::mpsc, executor::LocalPool, task::LocalSpawnExt as _};
use net_types::{Witness as _, ethernet::Mac};
use netstack3_port_spike::{
    EthernetDeviceEvent, EthernetFrame, NetworkServiceEndpoint, StackEthernetEndpoint,
};
use rand::Rng;
use std::{
    cell::{Cell, RefCell},
    future::poll_fn,
    io,
    net::{IpAddr, Ipv4Addr as StdIpv4Addr, SocketAddr},
    num::{NonZeroU16, NonZeroU64},
    rc::Rc,
    task::{Poll, Waker},
    time::Duration,
};

impl diagnostics_traits::InspectableInstant for NativeInstant {
    fn record<I: diagnostics_traits::Inspector>(
        &self,
        name: diagnostics_traits::InstantPropertyName,
        inspector: &mut I,
    ) {
        inspector.record_uint(name.into(), self.as_nanos())
    }
}
impl DhcpInstant for NativeInstant {
    fn add(&self, d: Duration) -> Self {
        Self::from_nanos(
            self.as_nanos()
                .saturating_add(u64::try_from(d.as_nanos()).unwrap_or(u64::MAX)),
        )
    }
    fn average(&self, other: Self) -> Self {
        let (a, b) = (
            self.as_nanos().min(other.as_nanos()),
            self.as_nanos().max(other.as_nanos()),
        );
        Self::from_nanos(a + (b - a) / 2)
    }
}
#[derive(Default)]
struct Wakes {
    packet: Option<Waker>,
    udp: Option<Waker>,
    timers: Vec<(NativeInstant, Waker)>,
}
#[derive(Clone)]
struct NativeClock {
    now: Rc<Cell<NativeInstant>>,
    wakes: Rc<RefCell<Wakes>>,
}
impl Clock for NativeClock {
    type Instant = NativeInstant;
    async fn wait_until(&self, at: Self::Instant) {
        poll_fn(|cx| {
            if self.now.get() >= at {
                Poll::Ready(())
            } else {
                self.wakes
                    .borrow_mut()
                    .timers
                    .push((at, cx.waker().clone()));
                Poll::Pending
            }
        })
        .await
    }
    fn now(&self) -> Self::Instant {
        self.now.get()
    }
}
fn sock_err(e: RuntimeError) -> SocketError {
    SocketError::Other(io::Error::other(format!("{e:?}")))
}

#[derive(Clone)]
struct PacketSock {
    rt: Rc<RefCell<Runtime>>,
    wakes: Rc<RefCell<Wakes>>,
}
impl Socket<Mac> for PacketSock {
    async fn send_to(&self, b: &[u8], _: Mac) -> Result<(), SocketError> {
        self.rt.borrow_mut().dhcp_packet_send(b).map_err(sock_err)
    }
    async fn recv_from(&self, b: &mut [u8]) -> Result<DatagramInfo<Mac>, SocketError> {
        poll_fn(|cx| {
            if let Some(p) = self.rt.borrow_mut().dhcp_packet_receive() {
                if p.len() > b.len() {
                    return Poll::Ready(Err(SocketError::Other(io::Error::other(
                        "oversize DHCP packet",
                    ))));
                }
                b[..p.len()].copy_from_slice(&p);
                Poll::Ready(Ok(DatagramInfo {
                    length: p.len(),
                    address: Mac::BROADCAST,
                }))
            } else {
                self.wakes.borrow_mut().packet = Some(cx.waker().clone());
                Poll::Pending
            }
        })
        .await
    }
}
#[derive(Clone)]
struct PacketProvider(PacketSock);
impl PacketSocketProvider for PacketProvider {
    type Sock = PacketSock;
    async fn get_packet_socket(&self) -> Result<Self::Sock, SocketError> {
        Ok(self.0.clone())
    }
}
#[derive(Clone)]
struct UdpSock {
    rt: Rc<RefCell<Runtime>>,
    wakes: Rc<RefCell<Wakes>>,
    handle: UdpSocketHandle,
}
impl Socket<SocketAddr> for UdpSock {
    async fn send_to(&self, b: &[u8], a: SocketAddr) -> Result<(), SocketError> {
        let SocketAddr::V4(a) = a else {
            return Err(SocketError::AddrNotAvailable);
        };
        self.rt
            .borrow_mut()
            .udp_send_to(
                self.handle,
                a.ip().octets(),
                NonZeroU16::new(a.port()).ok_or(SocketError::AddrNotAvailable)?,
                b,
            )
            .map_err(sock_err)
    }
    async fn recv_from(&self, b: &mut [u8]) -> Result<DatagramInfo<SocketAddr>, SocketError> {
        poll_fn(
            |cx| match self.rt.borrow_mut().udp_receive_msg(self.handle) {
                Ok(Some(p)) => {
                    if p.body.len() > b.len() {
                        return Poll::Ready(Err(SocketError::Other(io::Error::other(
                            "oversize DHCP datagram",
                        ))));
                    }
                    let NativeIpAddress::V4(address) = p.source.address else {
                        return Poll::Ready(Err(SocketError::AddrNotAvailable));
                    };
                    b[..p.body.len()].copy_from_slice(&p.body);
                    Poll::Ready(Ok(DatagramInfo {
                        length: p.body.len(),
                        address: SocketAddr::from((StdIpv4Addr::from(address), p.source.port)),
                    }))
                }
                Ok(None) => {
                    self.wakes.borrow_mut().udp = Some(cx.waker().clone());
                    Poll::Pending
                }
                Err(e) => Poll::Ready(Err(sock_err(e))),
            },
        )
        .await
    }
}
#[derive(Clone)]
struct UdpProvider {
    rt: Rc<RefCell<Runtime>>,
    wakes: Rc<RefCell<Wakes>>,
}
impl UdpSocketProvider for UdpProvider {
    type Sock = UdpSock;
    async fn bind_new_udp_socket(&self, a: SocketAddr) -> Result<Self::Sock, SocketError> {
        let SocketAddr::V4(a) = a else {
            return Err(SocketError::AddrNotAvailable);
        };
        let mut rt = self.rt.borrow_mut();
        let h = rt.udp_socket().map_err(sock_err)?;
        rt.udp_bind(
            h,
            Some(a.ip().octets()),
            NonZeroU16::new(a.port()).ok_or(SocketError::AddrNotAvailable)?,
        )
        .map_err(sock_err)?;
        drop(rt);
        Ok(UdpSock {
            rt: self.rt.clone(),
            wakes: self.wakes.clone(),
            handle: h,
        })
    }
}
struct NativeRng<R>(R);
impl<R: Rng> RngProvider for NativeRng<R> {
    type RNG = R;
    fn get_rng(&mut self) -> &mut R {
        &mut self.0
    }
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DhcpStatus {
    Acquiring,
    Bound,
    Failed,
}
enum Effect {
    Transition(TransitionEffect<NativeInstant>),
    Failed,
}

/// Owns only native capabilities; all DHCP behavior is the pinned upstream state machine.
pub struct DhcpService {
    sockets: NativeSocketProvider,
    dns: NativeDnsBridge,
    rt: Rc<RefCell<Runtime>>,
    pool: LocalPool,
    now: Rc<Cell<NativeInstant>>,
    wakes: Rc<RefCell<Wakes>>,
    effects: mpsc::UnboundedReceiver<Effect>,
    address: mpsc::UnboundedSender<AddressEvent<()>>,
    stop: mpsc::UnboundedSender<()>,
    resume: mpsc::UnboundedSender<()>,
    dhcp_enabled: bool,
    link_up: bool,
    status: DhcpStatus,
}
impl DhcpService {
    pub fn new<R: Rng + 'static>(runtime: Runtime, rng: R, mac: [u8; 6]) -> Self {
        let rt = Rc::new(RefCell::new(runtime));
        let now = Rc::new(Cell::new(NativeInstant::ZERO));
        let wakes = Rc::new(RefCell::new(Wakes::default()));
        let packet = PacketProvider(PacketSock {
            rt: rt.clone(),
            wakes: wakes.clone(),
        });
        let udp = UdpProvider {
            rt: rt.clone(),
            wakes: wakes.clone(),
        };
        let clock = NativeClock {
            now: now.clone(),
            wakes: wakes.clone(),
        };
        let mut req = OptionCodeMap::new();
        req.put(OptionCode::SubnetMask, OptionRequested::Required);
        req.put(OptionCode::Router, OptionRequested::Optional);
        req.put(OptionCode::DomainNameServer, OptionRequested::Optional);
        let config = ClientConfig {
            client_hardware_address: Mac::new(mac),
            client_identifier: None,
            requested_parameters: req,
            preferred_lease_time_secs: None,
            requested_ip_address: None,
            debug_log_prefix: DebugLogPrefix {
                interface_id: NonZeroU64::new(1).unwrap(),
            },
        };
        let (etx, effects) = mpsc::unbounded();
        let (address, mut arx) = mpsc::unbounded();
        let pool = LocalPool::new();
        let (stop, mut stop_receiver) = mpsc::unbounded();
        let (resume, mut resume_receiver) = mpsc::unbounded();
        pool.spawner()
            .spawn_local(async move {
                let counters = Counters::default();
                let mut rng = NativeRng(rng);
                loop {
                    let mut state = State::default();
                    loop {
                        match state
                            .run(
                                &config,
                                &packet,
                                &udp,
                                &mut rng,
                                &clock,
                                &mut stop_receiver,
                                &mut arx,
                                &counters,
                            )
                            .await
                        {
                            Ok(Step::NextState(t)) => {
                                let (next, effect) = state.apply(&config, t);
                                state = next;
                                if let Some(e) = effect {
                                    if etx.unbounded_send(Effect::Transition(e)).is_err() {
                                        return;
                                    }
                                }
                            }
                            Ok(Step::Exit(_)) => break,
                            Err(_) => {
                                let _ = etx.unbounded_send(Effect::Failed);
                                return;
                            }
                        }
                    }
                    if resume_receiver.next().await.is_none() {
                        return;
                    }
                }
            })
            .expect("spawn DHCP core");
        let sockets = NativeSocketProvider::new(rt.clone());
        Self {
            sockets,
            dns: NativeDnsBridge::new(),
            rt,
            pool,
            now,
            wakes,
            effects,
            address,
            stop,
            resume,
            dhcp_enabled: true,
            link_up: true,
            status: DhcpStatus::Acquiring,
        }
    }
    pub fn status(&self) -> DhcpStatus {
        self.status
    }
    pub fn lookup_ip(&mut self, name: impl Into<String>) -> io::Result<DnsLookupHandle> {
        self.dns.lookup_ip(name)
    }
    pub fn take_lookup(&mut self, h: DnsLookupHandle) -> Option<DnsLookupResult> {
        self.dns.take_result(h)
    }
    pub fn socket_provider(&self) -> NativeSocketProvider {
        self.sockets.clone()
    }
    pub fn runtime(&self) -> std::cell::Ref<'_, Runtime> {
        self.rt.borrow()
    }
    pub fn configure_static(
        &mut self,
        address: [u8; 4],
        prefix: u8,
        gateway: Option<[u8; 4]>,
        dns: &[[u8; 4]],
    ) -> Result<(), RuntimeError> {
        self.dhcp_enabled = false;
        let mut servers = [None, None];
        for (slot, address) in servers.iter_mut().zip(dns) {
            *slot = Some(StdIpv4Addr::from(*address));
        }
        self.rt.borrow_mut().apply_ipv4(address, prefix, gateway)?;
        self.rt.borrow_mut().set_dns_servers(servers);
        if self.configure_dns() {
            self.status = DhcpStatus::Bound;
            Ok(())
        } else {
            self.clear_configuration(DhcpStatus::Failed);
            Err(RuntimeError::InvalidLease)
        }
    }
    fn clear_configuration(&mut self, status: DhcpStatus) {
        self.rt.borrow_mut().revoke_ipv4();
        self.dns.clear();
        self.status = status;
    }
    fn configure_dns(&mut self) -> bool {
        let servers: Vec<_> = self
            .rt
            .borrow()
            .dns_servers()
            .into_iter()
            .flatten()
            .map(IpAddr::V4)
            .collect();
        self.dns.configure(&servers).is_ok()
    }
    fn effects(&mut self) -> usize {
        let mut n = 0;
        while let Ok(e) = self.effects.try_recv() {
            n += 1;
            match e {
                Effect::Transition(TransitionEffect::DropLease { .. }) => {
                    self.clear_configuration(DhcpStatus::Acquiring);
                }
                Effect::Transition(TransitionEffect::HandleNewLease(l)) => {
                    let applied = apply_lease(
                        &mut self.rt.borrow_mut(),
                        l.ip_address.get().ipv4_bytes(),
                        &l.parameters,
                    )
                    .is_ok();
                    if applied && self.configure_dns() {
                        let _ = self
                            .address
                            .unbounded_send(AddressEvent::AssignmentStateChanged(
                                AddressAssignmentState::Assigned,
                            ));
                        self.status = DhcpStatus::Bound;
                    } else {
                        self.clear_configuration(DhcpStatus::Failed);
                    }
                }
                Effect::Transition(TransitionEffect::HandleRenewedLease(l)) => {
                    apply_dns(&mut self.rt.borrow_mut(), &l.parameters);
                    if self.configure_dns() {
                        self.status = DhcpStatus::Bound;
                    } else {
                        self.clear_configuration(DhcpStatus::Failed);
                    }
                }
                Effect::Failed => self.clear_configuration(DhcpStatus::Failed),
            }
        }
        n
    }
}
fn apply_lease(rt: &mut Runtime, address: [u8; 4], p: &[DhcpOption]) -> Result<(), RuntimeError> {
    let (mut prefix, mut gw, mut dns) = (None, None, [None, None]);
    for o in p {
        match o {
            DhcpOption::SubnetMask(v) => prefix = Some(u8::from(*v)),
            DhcpOption::Router(v) => gw = v.first().map(|v| v.octets()),
            DhcpOption::DomainNameServer(v) => {
                for (s, v) in dns.iter_mut().zip(v.iter()) {
                    *s = Some(StdIpv4Addr::from(v.octets()))
                }
            }
            _ => {}
        }
    }
    rt.apply_ipv4(address, prefix.ok_or(RuntimeError::InvalidLease)?, gw)?;
    rt.set_dns_servers(dns);
    Ok(())
}
fn apply_dns(rt: &mut Runtime, p: &[DhcpOption]) {
    for o in p {
        if let DhcpOption::DomainNameServer(v) = o {
            let mut d = [None, None];
            for (s, v) in d.iter_mut().zip(v.iter()) {
                *s = Some(StdIpv4Addr::from(v.octets()))
            }
            rt.set_dns_servers(d)
        }
    }
}
impl StackEthernetEndpoint for DhcpService {
    fn receive_frame(&mut self, f: EthernetFrame) -> Result<(), EthernetFrame> {
        self.rt.borrow_mut().receive_frame(f);
        let mut w = self.wakes.borrow_mut();
        if let Some(x) = w.packet.take() {
            x.wake()
        }
        if let Some(x) = w.udp.take() {
            x.wake()
        }
        Ok(())
    }
    fn take_transmit(&mut self) -> Option<EthernetFrame> {
        self.rt.borrow_mut().take_transmit()
    }
}
impl NetworkServiceEndpoint for DhcpService {
    fn poll_at(&mut self, d: Duration, budget: usize) -> usize {
        let now = NativeInstant::from_nanos(u64::try_from(d.as_nanos()).unwrap_or(u64::MAX));
        self.now.set(now);
        {
            let mut w = self.wakes.borrow_mut();
            let mut p = Vec::new();
            for (d, x) in w.timers.drain(..) {
                if d <= now { x.wake() } else { p.push((d, x)) }
            }
            w.timers = p
        }
        self.rt.borrow_mut().set_now(now);
        let n = self.rt.borrow_mut().dispatch_due(budget);
        let effects = if self.dhcp_enabled {
            self.pool.run_until_stalled();
            self.effects()
        } else {
            0
        };
        let dns = self.dns.pump(&mut self.rt.borrow_mut(), d);
        n + effects + dns
    }
    fn on_device_event(&mut self, e: EthernetDeviceEvent) {
        match e {
            EthernetDeviceEvent::TransmitReady => self.rt.borrow_mut().service_tx(1),
            EthernetDeviceEvent::ReceiveReady => {}
            EthernetDeviceEvent::LinkStateChanged(up) if up == self.link_up => {}
            EthernetDeviceEvent::LinkStateChanged(false) => {
                self.link_up = false;
                self.clear_configuration(if self.dhcp_enabled {
                    DhcpStatus::Acquiring
                } else {
                    DhcpStatus::Failed
                });
                if self.dhcp_enabled {
                    let _ = self.stop.unbounded_send(());
                    self.pool.run_until_stalled();
                }
                while self.rt.borrow_mut().take_tx().is_some() {}
            }
            EthernetDeviceEvent::LinkStateChanged(true) => {
                self.link_up = true;
                if self.dhcp_enabled {
                    self.status = DhcpStatus::Acquiring;
                    let _ = self.resume.unbounded_send(());
                    self.pool.run_until_stalled();
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rand::SeedableRng as _;

    #[test]
    fn failed_configuration_atomically_revokes_address_routes_and_dns() {
        let runtime = Runtime::new(
            8,
            [7; 1024],
            NonZeroU64::new(1).unwrap(),
            [0x02, 0, 0, 0, 0, 1],
            1500,
        )
        .unwrap();
        let mut service = DhcpService::new(
            runtime,
            rand::rngs::StdRng::seed_from_u64(7),
            [0x02, 0, 0, 0, 0, 1],
        );
        service
            .rt
            .borrow_mut()
            .apply_ipv4([192, 0, 2, 10], 24, Some([192, 0, 2, 1]))
            .unwrap();
        service
            .rt
            .borrow_mut()
            .set_dns_servers([Some(StdIpv4Addr::new(192, 0, 2, 53)), None]);
        assert!(service.configure_dns());
        service.status = DhcpStatus::Bound;

        service.clear_configuration(DhcpStatus::Failed);

        assert_eq!(service.status(), DhcpStatus::Failed);
        assert_eq!(service.runtime().ipv4_address(), None);
        assert_eq!(service.runtime().dns_servers(), [None, None]);
        assert!(service.dns.resolver().is_none());
    }

    #[test]
    fn upstream_client_emits_discover_through_native_packet_adapter() {
        let runtime = Runtime::new(
            8,
            [7; 1024],
            NonZeroU64::new(1).unwrap(),
            [0x02, 0, 0, 0, 0, 1],
            1500,
        )
        .unwrap();
        let mut service = DhcpService::new(
            runtime,
            rand::rngs::StdRng::seed_from_u64(7),
            [0x02, 0, 0, 0, 0, 1],
        );
        service.poll_at(Duration::ZERO, 8);
        let frame = service.take_transmit().expect("upstream DHCPDISCOVER");
        let bytes = frame.as_bytes();
        assert_eq!(&bytes[12..14], &[0x08, 0x00]);
        assert_eq!(bytes[23], 17);
        let ihl = usize::from(bytes[14] & 0x0f) * 4;
        assert_eq!(&bytes[14 + ihl..14 + ihl + 4], &[0, 68, 0, 67]);
        assert_eq!(service.status(), DhcpStatus::Acquiring);
    }

    #[test]
    fn link_loss_revokes_configuration_and_link_return_restarts_dhcp() {
        let runtime = Runtime::new(
            8,
            [7; 1024],
            NonZeroU64::new(1).unwrap(),
            [0x02, 0, 0, 0, 0, 1],
            1500,
        )
        .unwrap();
        let mut service = DhcpService::new(
            runtime,
            rand::rngs::StdRng::seed_from_u64(7),
            [0x02, 0, 0, 0, 0, 1],
        );
        service.poll_at(Duration::ZERO, 8);
        let _first_discover = service.take_transmit().unwrap();
        service
            .rt
            .borrow_mut()
            .apply_ipv4([192, 0, 2, 10], 24, Some([192, 0, 2, 1]))
            .unwrap();
        service
            .rt
            .borrow_mut()
            .set_dns_servers([Some(StdIpv4Addr::new(192, 0, 2, 53)), None]);
        assert!(service.configure_dns());
        service.status = DhcpStatus::Bound;

        service.on_device_event(EthernetDeviceEvent::LinkStateChanged(false));
        assert_eq!(service.status(), DhcpStatus::Acquiring);
        assert_eq!(service.runtime().ipv4_address(), None);
        assert_eq!(service.runtime().dns_servers(), [None, None]);
        assert!(service.dns.resolver().is_none());

        service.on_device_event(EthernetDeviceEvent::LinkStateChanged(true));
        service.poll_at(Duration::from_secs(1), 8);
        let restarted = service.take_transmit().expect("restarted DHCPDISCOVER");
        let bytes = restarted.as_bytes();
        let ihl = usize::from(bytes[14] & 0x0f) * 4;
        assert_eq!(&bytes[14 + ihl..14 + ihl + 4], &[0, 68, 0, 67]);
    }

    #[test]
    fn static_configuration_advances_runtime_without_starting_dhcp() {
        let runtime = Runtime::new(
            8,
            [7; 1024],
            NonZeroU64::new(1).unwrap(),
            [0x02, 0, 0, 0, 0, 1],
            1500,
        )
        .unwrap();
        let mut service = DhcpService::new(
            runtime,
            rand::rngs::StdRng::seed_from_u64(7),
            [0x02, 0, 0, 0, 0, 1],
        );
        service
            .configure_static([192, 0, 2, 10], 24, None, &[[192, 0, 2, 2]])
            .unwrap();

        service.poll_at(Duration::from_secs(1), 8);

        assert_eq!(service.status(), DhcpStatus::Bound);
        assert_eq!(service.runtime().ipv4_address(), Some([192, 0, 2, 10]));
        assert_eq!(
            service.take_transmit(),
            None,
            "static mode emits no DHCP discovery"
        );
    }
}
