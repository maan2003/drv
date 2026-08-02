//! Native adapters for Fuchsia's production DHCP client core.

use std::{cell::{Cell, RefCell}, future::poll_fn, io, net::{Ipv4Addr as StdIpv4Addr, SocketAddr}, num::{NonZeroU16, NonZeroU64}, rc::Rc, task::{Poll, Waker}, time::Duration};
use dhcp_client_core::{client::{AddressAssignmentState, AddressEvent, ClientConfig, DebugLogPrefix, State, Step, TransitionEffect}, deps::{Clock, DatagramInfo, Instant as DhcpInstant, PacketSocketProvider, RngProvider, Socket, SocketError, UdpSocketProvider}, inspect::Counters, parse::{OptionCodeMap, OptionRequested}};
use dhcp_protocol::{DhcpOption, OptionCode};
use futures::{channel::mpsc, executor::LocalPool, task::LocalSpawnExt as _};
use net_types::{ethernet::Mac, Witness as _};
use rand::Rng;
use netstack3_port_spike::{EthernetDeviceEvent, EthernetFrame, NetworkServiceEndpoint, StackEthernetEndpoint};
use crate::{NativeInstant, Runtime, RuntimeError, UdpSocketHandle};

impl diagnostics_traits::InspectableInstant for NativeInstant {
    fn record<I: diagnostics_traits::Inspector>(&self, name: diagnostics_traits::InstantPropertyName, inspector: &mut I) {
        inspector.record_uint(name.into(), self.as_nanos())
    }
}
impl DhcpInstant for NativeInstant {
    fn add(&self, d: Duration) -> Self { Self::from_nanos(self.as_nanos().saturating_add(u64::try_from(d.as_nanos()).unwrap_or(u64::MAX))) }
    fn average(&self, other: Self) -> Self {
        let (a,b)=(self.as_nanos().min(other.as_nanos()),self.as_nanos().max(other.as_nanos()));
        Self::from_nanos(a+(b-a)/2)
    }
}
#[derive(Default)] struct Wakes { packet: Option<Waker>, udp: Option<Waker>, timers: Vec<(NativeInstant,Waker)> }
#[derive(Clone)] struct NativeClock { now: Rc<Cell<NativeInstant>>, wakes: Rc<RefCell<Wakes>> }
impl Clock for NativeClock {
    type Instant=NativeInstant;
    async fn wait_until(&self, at: Self::Instant) {
        poll_fn(|cx| if self.now.get()>=at { Poll::Ready(()) } else {
            self.wakes.borrow_mut().timers.push((at,cx.waker().clone())); Poll::Pending
        }).await
    }
    fn now(&self)->Self::Instant { self.now.get() }
}
fn sock_err(e: RuntimeError)->SocketError { SocketError::Other(io::Error::other(format!("{e:?}"))) }

#[derive(Clone)] struct PacketSock { rt:Rc<RefCell<Runtime>>, wakes:Rc<RefCell<Wakes>> }
impl Socket<Mac> for PacketSock {
    async fn send_to(&self,b:&[u8],_:Mac)->Result<(),SocketError>{self.rt.borrow_mut().dhcp_packet_send(b).map_err(sock_err)}
    async fn recv_from(&self,b:&mut[u8])->Result<DatagramInfo<Mac>,SocketError>{
        poll_fn(|cx| if let Some(p)=self.rt.borrow_mut().dhcp_packet_receive(){
            if p.len()>b.len(){return Poll::Ready(Err(SocketError::Other(io::Error::other("oversize DHCP packet"))))}
            b[..p.len()].copy_from_slice(&p); Poll::Ready(Ok(DatagramInfo{length:p.len(),address:Mac::BROADCAST}))
        }else{self.wakes.borrow_mut().packet=Some(cx.waker().clone());Poll::Pending}).await
    }
}
#[derive(Clone)] struct PacketProvider(PacketSock);
impl PacketSocketProvider for PacketProvider {
    type Sock=PacketSock;
    async fn get_packet_socket(&self)->Result<Self::Sock,SocketError>{Ok(self.0.clone())}
}
#[derive(Clone)] struct UdpSock{rt:Rc<RefCell<Runtime>>,wakes:Rc<RefCell<Wakes>>,handle:UdpSocketHandle}
impl Socket<SocketAddr> for UdpSock {
    async fn send_to(&self,b:&[u8],a:SocketAddr)->Result<(),SocketError>{
        let SocketAddr::V4(a)=a else{return Err(SocketError::AddrNotAvailable)};
        self.rt.borrow_mut().udp_send_to(self.handle,a.ip().octets(),NonZeroU16::new(a.port()).ok_or(SocketError::AddrNotAvailable)?,b).map_err(sock_err)
    }
    async fn recv_from(&self,b:&mut[u8])->Result<DatagramInfo<SocketAddr>,SocketError>{
        poll_fn(|cx|match self.rt.borrow_mut().udp_receive(self.handle){
            Ok(Some(p))=>{if p.len()>b.len(){return Poll::Ready(Err(SocketError::Other(io::Error::other("oversize DHCP datagram"))))}
                b[..p.len()].copy_from_slice(&p);Poll::Ready(Ok(DatagramInfo{length:p.len(),address:SocketAddr::from((StdIpv4Addr::UNSPECIFIED,67))}))},
            Ok(None)=>{self.wakes.borrow_mut().udp=Some(cx.waker().clone());Poll::Pending},Err(e)=>Poll::Ready(Err(sock_err(e))) }).await
    }
}
#[derive(Clone)] struct UdpProvider{rt:Rc<RefCell<Runtime>>,wakes:Rc<RefCell<Wakes>>}
impl UdpSocketProvider for UdpProvider {
    type Sock=UdpSock;
    async fn bind_new_udp_socket(&self,a:SocketAddr)->Result<Self::Sock,SocketError>{
        let SocketAddr::V4(a)=a else{return Err(SocketError::AddrNotAvailable)};
        let mut rt=self.rt.borrow_mut();let h=rt.udp_socket().map_err(sock_err)?;
        rt.udp_bind(h,Some(a.ip().octets()),NonZeroU16::new(a.port()).ok_or(SocketError::AddrNotAvailable)?).map_err(sock_err)?;
        drop(rt);Ok(UdpSock{rt:self.rt.clone(),wakes:self.wakes.clone(),handle:h})
    }
}
struct NativeRng<R>(R); impl<R:Rng> RngProvider for NativeRng<R>{type RNG=R;fn get_rng(&mut self)->&mut R{&mut self.0}}
#[derive(Clone,Copy,Debug,Eq,PartialEq)] pub enum DhcpStatus{Acquiring,Bound,Failed}
enum Effect{Transition(TransitionEffect<NativeInstant>),Failed}

/// Owns only native capabilities; all DHCP behavior is the pinned upstream state machine.
pub struct DhcpService{rt:Rc<RefCell<Runtime>>,pool:LocalPool,now:Rc<Cell<NativeInstant>>,wakes:Rc<RefCell<Wakes>>,effects:mpsc::UnboundedReceiver<Effect>,address:mpsc::UnboundedSender<AddressEvent<()>>,status:DhcpStatus}
impl DhcpService{
 pub fn new<R:Rng+'static>(runtime:Runtime,rng:R,mac:[u8;6])->Self{
  let rt=Rc::new(RefCell::new(runtime));let now=Rc::new(Cell::new(NativeInstant::ZERO));let wakes=Rc::new(RefCell::new(Wakes::default()));
  let packet=PacketProvider(PacketSock{rt:rt.clone(),wakes:wakes.clone()});let udp=UdpProvider{rt:rt.clone(),wakes:wakes.clone()};let clock=NativeClock{now:now.clone(),wakes:wakes.clone()};
  let mut req=OptionCodeMap::new();req.put(OptionCode::SubnetMask,OptionRequested::Required);req.put(OptionCode::Router,OptionRequested::Optional);req.put(OptionCode::DomainNameServer,OptionRequested::Optional);
  let config=ClientConfig{client_hardware_address:Mac::new(mac),client_identifier:None,requested_parameters:req,preferred_lease_time_secs:None,requested_ip_address:None,debug_log_prefix:DebugLogPrefix{interface_id:NonZeroU64::new(1).unwrap()}};
  let(etx,effects)=mpsc::unbounded();let(address,mut arx)=mpsc::unbounded();let pool=LocalPool::new();
  pool.spawner().spawn_local(async move{let counters=Counters::default();let(_stop,mut srx)=mpsc::unbounded();let mut rng=NativeRng(rng);let mut state=State::default();
   loop{match state.run(&config,&packet,&udp,&mut rng,&clock,&mut srx,&mut arx,&counters).await{
    Ok(Step::NextState(t))=>{let(next,effect)=state.apply(&config,t);state=next;if let Some(e)=effect{if etx.unbounded_send(Effect::Transition(e)).is_err(){break}}},
    _=>{let _=etx.unbounded_send(Effect::Failed);break}
   }}
  }).expect("spawn DHCP core");
  Self{rt,pool,now,wakes,effects,address,status:DhcpStatus::Acquiring}
 }
 pub fn status(&self)->DhcpStatus{self.status}
 pub fn runtime(&self)->std::cell::Ref<'_,Runtime>{self.rt.borrow()}
 fn effects(&mut self)->usize{let mut n=0;while let Ok(e)=self.effects.try_recv(){n+=1;match e{
  Effect::Transition(TransitionEffect::DropLease{..})=>{self.rt.borrow_mut().revoke_ipv4();self.status=DhcpStatus::Acquiring},
  Effect::Transition(TransitionEffect::HandleNewLease(l))=>{self.status=if apply_lease(&mut self.rt.borrow_mut(),l.ip_address.get().ipv4_bytes(),&l.parameters).is_ok(){let _=self.address.unbounded_send(AddressEvent::AssignmentStateChanged(AddressAssignmentState::Assigned));DhcpStatus::Bound}else{DhcpStatus::Failed}},
  Effect::Transition(TransitionEffect::HandleRenewedLease(l))=>{apply_dns(&mut self.rt.borrow_mut(),&l.parameters);self.status=DhcpStatus::Bound},
  Effect::Failed=>self.status=DhcpStatus::Failed}}n}
}
fn apply_lease(rt:&mut Runtime,address:[u8;4],p:&[DhcpOption])->Result<(),RuntimeError>{let(mut prefix,mut gw,mut dns)=(None,None,[None,None]);for o in p{match o{DhcpOption::SubnetMask(v)=>prefix=Some(u8::from(*v)),DhcpOption::Router(v)=>gw=v.first().map(|v|v.octets()),DhcpOption::DomainNameServer(v)=>for(s,v)in dns.iter_mut().zip(v.iter()){*s=Some(StdIpv4Addr::from(v.octets()))},_=>{}}}rt.apply_ipv4(address,prefix.ok_or(RuntimeError::InvalidLease)?,gw)?;rt.set_dns_servers(dns);Ok(())}
fn apply_dns(rt:&mut Runtime,p:&[DhcpOption]){for o in p{if let DhcpOption::DomainNameServer(v)=o{let mut d=[None,None];for(s,v)in d.iter_mut().zip(v.iter()){*s=Some(StdIpv4Addr::from(v.octets()))}rt.set_dns_servers(d)}}}
impl StackEthernetEndpoint for DhcpService{
 fn receive_frame(&mut self,f:EthernetFrame)->Result<(),EthernetFrame>{self.rt.borrow_mut().receive_frame(f);let mut w=self.wakes.borrow_mut();if let Some(x)=w.packet.take(){x.wake()}if let Some(x)=w.udp.take(){x.wake()}Ok(())}
 fn take_transmit(&mut self)->Option<EthernetFrame>{self.rt.borrow_mut().take_transmit()}
}
impl NetworkServiceEndpoint for DhcpService{
 fn poll_at(&mut self,d:Duration,budget:usize)->usize{let now=NativeInstant::from_nanos(u64::try_from(d.as_nanos()).unwrap_or(u64::MAX));self.now.set(now);{let mut w=self.wakes.borrow_mut();let mut p=Vec::new();for(d,x)in w.timers.drain(..){if d<=now{x.wake()}else{p.push((d,x))}}w.timers=p}self.rt.borrow_mut().set_now(now);let n=self.rt.borrow_mut().dispatch_due(budget);self.pool.run_until_stalled();n+self.effects()}
 fn on_device_event(&mut self,e:EthernetDeviceEvent){if e==EthernetDeviceEvent::TransmitReady{self.rt.borrow_mut().service_tx(1);}}
}

#[cfg(test)]
mod tests {
    use super::*;
    use rand::SeedableRng as _;

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
}
