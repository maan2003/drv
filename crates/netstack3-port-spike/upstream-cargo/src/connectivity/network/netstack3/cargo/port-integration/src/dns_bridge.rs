//! Send command-channel adapters from pinned Trust-DNS to single-owner Netstack3.
use crate::{Runtime, TcpSocketHandle, UdpSocketHandle};
use async_trait::async_trait;
use futures::{
    executor::LocalPool,
    future::{AbortHandle, Abortable, Either, select},
    task::LocalSpawnExt as _,
};
use futures_io::{AsyncRead, AsyncWrite};
use std::{
    cell::RefCell,
    collections::{HashMap, VecDeque},
    future::Future,
    io,
    net::{IpAddr, SocketAddr},
    num::{NonZeroU16, NonZeroUsize},
    pin::Pin,
    rc::Rc,
    sync::{
        Arc, Mutex,
        atomic::{AtomicU64, Ordering},
    },
    task::{Context, Poll, Waker},
    time::Duration,
};
use trust_dns_proto::{
    Time,
    error::ProtoError,
    tcp::{Connect, DnsTcpStream},
    udp::UdpSocket,
};
use trust_dns_resolver::{
    AsyncResolver,
    config::{NameServerConfigGroup, ResolverConfig, ResolverOpts},
    name_server::{GenericConnection, GenericConnectionProvider, RuntimeProvider, Spawn},
};

const DEFAULT_LIMIT: usize = 64;
const SOCKET_MESSAGE_LIMIT: usize = 8;
// DNS responses larger than this service-owned bound are discarded. It covers
// conventional UDP/EDNS responses and bounds each transport's retained queue;
// TCP delivery is already read in 2 KiB chunks.
const SOCKET_MESSAGE_BYTES_LIMIT: usize = 4096;

fn native_resolver_options() -> ResolverOpts {
    let mut options = ResolverOpts::default();
    // This resolver runs after the network service has entered its empty-root
    // sandbox. Host-file lookup would both be ineffective and violate the
    // runtime no-open capability boundary.
    options.use_hosts_file = false;
    options
}

type Task = Pin<Box<dyn Future<Output = Result<(), ProtoError>> + Send>>;
struct End {
    limit: usize,
    peer: Mutex<Option<SocketAddr>>,
    open: Mutex<Option<io::Result<()>>>,
    rx: Mutex<VecDeque<Vec<u8>>>,
    wake: Mutex<Option<Waker>>,
}
impl End {
    fn new(limit: usize) -> Self {
        Self {
            limit,
            peer: Mutex::new(None),
            open: Mutex::new(None),
            rx: Mutex::new(VecDeque::new()),
            wake: Mutex::new(None),
        }
    }
    fn opened(&self, r: io::Result<()>) {
        *self.open.lock().unwrap() = Some(r);
        self.wake()
    }
    fn push(&self, b: Vec<u8>) {
        if b.len() > SOCKET_MESSAGE_BYTES_LIMIT {
            return;
        }
        let mut q = self.rx.lock().unwrap();
        if q.len() == self.limit {
            q.pop_front();
        }
        q.push_back(b);
        drop(q);
        self.wake()
    }
    fn wake(&self) {
        if let Some(w) = self.wake.lock().unwrap().take() {
            w.wake()
        }
    }
}
enum Cmd {
    OU(u64, SocketAddr, Option<SocketAddr>, Arc<End>),
    SU(u64, SocketAddr, Vec<u8>),
    OT(u64, SocketAddr, Arc<End>),
    WT(u64, Vec<u8>),
    CT(u64),
}
struct Bus {
    limit: usize,
    now: AtomicU64,
    next: AtomicU64,
    cmd: Mutex<VecDeque<Cmd>>,
    tasks: Mutex<VecDeque<Task>>,
    timers: Mutex<Vec<(u64, Waker)>>,
}
impl Bus {
    fn new(limit: usize) -> Self {
        Self {
            limit,
            now: AtomicU64::new(0),
            next: AtomicU64::new(1),
            cmd: Mutex::new(VecDeque::new()),
            tasks: Mutex::new(VecDeque::new()),
            timers: Mutex::new(Vec::new()),
        }
    }
    fn send(&self, c: Cmd) -> io::Result<()> {
        if matches!(&c, Cmd::SU(_, _, bytes) | Cmd::WT(_, bytes) if bytes.len() > SOCKET_MESSAGE_BYTES_LIMIT)
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "DNS command payload exceeds bridge budget",
            ));
        }
        let mut q = self.cmd.lock().unwrap();
        if q.len() == self.limit {
            return Err(io::Error::new(
                io::ErrorKind::WouldBlock,
                "DNS command queue full",
            ));
        }
        q.push_back(c);
        Ok(())
    }
}
thread_local! {static ACTIVE:RefCell<Option<Arc<Bus>>>=const{RefCell::new(None)}}
fn bus() -> io::Result<Arc<Bus>> {
    ACTIVE.with(|a| {
        a.borrow()
            .clone()
            .ok_or_else(|| io::Error::other("DNS bridge inactive"))
    })
}
struct Guard;
impl Guard {
    fn enter(b: Arc<Bus>) -> Self {
        ACTIVE.with(|a| *a.borrow_mut() = Some(b));
        Self
    }
}
impl Drop for Guard {
    fn drop(&mut self) {
        ACTIVE.with(|a| *a.borrow_mut() = None)
    }
}
struct Delay(Arc<Bus>, u64);
impl Future for Delay {
    type Output = ();
    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<()> {
        if self.0.now.load(Ordering::Relaxed) >= self.1 {
            Poll::Ready(())
        } else {
            self.0
                .timers
                .lock()
                .unwrap()
                .push((self.1, cx.waker().clone()));
            Poll::Pending
        }
    }
}
#[derive(Clone, Copy, Debug)]
pub struct NativeDnsTime;
#[async_trait]
impl Time for NativeDnsTime {
    async fn delay_for(d: Duration) {
        let b = bus().expect("DNS time outside bridge");
        let at = b
            .now
            .load(Ordering::Relaxed)
            .saturating_add(u64::try_from(d.as_nanos()).unwrap_or(u64::MAX));
        Delay(b, at).await
    }
    async fn timeout<F: 'static + Future + Send>(d: Duration, f: F) -> io::Result<F::Output> {
        match select(Box::pin(f), Box::pin(Self::delay_for(d))).await {
            Either::Left((v, _)) => Ok(v),
            Either::Right(_) => Err(io::Error::new(io::ErrorKind::TimedOut, "DNS timeout")),
        }
    }
}
#[derive(Clone)]
pub struct NativeSpawn(Arc<Bus>);
impl Spawn for NativeSpawn {
    fn spawn_bg<F>(&mut self, f: F)
    where
        F: Future<Output = Result<(), ProtoError>> + Send + 'static,
    {
        self.0.tasks.lock().unwrap().push_back(Box::pin(f))
    }
}
#[derive(Clone)]
pub struct NativeDnsRuntime;
impl RuntimeProvider for NativeDnsRuntime {
    type Handle = NativeSpawn;
    type Timer = NativeDnsTime;
    type Udp = NativeUdp;
    type Tcp = NativeTcp;
}

pub struct NativeUdp {
    b: Arc<Bus>,
    id: u64,
    remote: SocketAddr,
    e: Arc<End>,
}
impl Unpin for NativeUdp {}
async fn wait(e: &Arc<End>) -> io::Result<()> {
    std::future::poll_fn(|cx| {
        if let Some(r) = e.open.lock().unwrap().take() {
            Poll::Ready(r)
        } else {
            *e.wake.lock().unwrap() = Some(cx.waker().clone());
            Poll::Pending
        }
    })
    .await
}
impl NativeUdp {
    async fn open(remote: SocketAddr, bind: Option<SocketAddr>) -> io::Result<Self> {
        let b = bus()?;
        let id = b.next.fetch_add(1, Ordering::Relaxed);
        let e = Arc::new(End::new(SOCKET_MESSAGE_LIMIT));
        b.send(Cmd::OU(id, remote, bind, e.clone()))?;
        Ok(Self { b, id, remote, e })
    }
}
#[async_trait]
impl UdpSocket for NativeUdp {
    type Time = NativeDnsTime;
    async fn connect(a: SocketAddr) -> io::Result<Self> {
        Self::open(a, None).await
    }
    async fn connect_with_bind(a: SocketAddr, l: SocketAddr) -> io::Result<Self> {
        Self::open(a, Some(l)).await
    }
    async fn bind(a: SocketAddr) -> io::Result<Self> {
        Self::open(a, Some(a)).await
    }
    fn poll_recv_from(
        &self,
        cx: &mut Context<'_>,
        out: &mut [u8],
    ) -> Poll<io::Result<(usize, SocketAddr)>> {
        if let Some(b) = self.e.rx.lock().unwrap().pop_front() {
            let n = b.len().min(out.len());
            out[..n].copy_from_slice(&b[..n]);
            Poll::Ready(Ok((n, self.e.peer.lock().unwrap().unwrap_or(self.remote))))
        } else {
            *self.e.wake.lock().unwrap() = Some(cx.waker().clone());
            Poll::Pending
        }
    }
    fn poll_send_to(
        &self,
        _: &mut Context<'_>,
        b: &[u8],
        a: SocketAddr,
    ) -> Poll<io::Result<usize>> {
        *self.e.peer.lock().unwrap() = Some(a);
        Poll::Ready(
            self.b
                .send(Cmd::SU(self.id, a, b.to_vec()))
                .map(|_| b.len()),
        )
    }
}
pub struct NativeTcp {
    b: Arc<Bus>,
    id: u64,
    e: Arc<End>,
    cur: Option<(Vec<u8>, usize)>,
}
impl Unpin for NativeTcp {}
impl DnsTcpStream for NativeTcp {
    type Time = NativeDnsTime;
}
#[async_trait]
impl Connect for NativeTcp {
    async fn connect_with_bind(a: SocketAddr, _: Option<SocketAddr>) -> io::Result<Self> {
        let b = bus()?;
        let id = b.next.fetch_add(1, Ordering::Relaxed);
        let e = Arc::new(End::new(SOCKET_MESSAGE_LIMIT));
        b.send(Cmd::OT(id, a, e.clone()))?;
        wait(&e).await?;
        Ok(Self {
            b,
            id,
            e,
            cur: None,
        })
    }
}
impl AsyncRead for NativeTcp {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        out: &mut [u8],
    ) -> Poll<io::Result<usize>> {
        if self.cur.is_none() {
            let next = self.e.rx.lock().unwrap().pop_front();
            self.cur = next.map(|b| (b, 0));
        }
        if let Some((b, p)) = self.cur.as_mut() {
            let n = (b.len() - *p).min(out.len());
            out[..n].copy_from_slice(&b[*p..*p + n]);
            *p += n;
            if *p == b.len() {
                self.cur = None
            }
            Poll::Ready(Ok(n))
        } else {
            *self.e.wake.lock().unwrap() = Some(cx.waker().clone());
            Poll::Pending
        }
    }
}
impl AsyncWrite for NativeTcp {
    fn poll_write(self: Pin<&mut Self>, _: &mut Context<'_>, b: &[u8]) -> Poll<io::Result<usize>> {
        Poll::Ready(self.b.send(Cmd::WT(self.id, b.to_vec())).map(|_| b.len()))
    }
    fn poll_flush(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<io::Result<()>> {
        Poll::Ready(Ok(()))
    }
    fn poll_close(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<io::Result<()>> {
        Poll::Ready(self.b.send(Cmd::CT(self.id)))
    }
}
type Resolver = AsyncResolver<GenericConnection, GenericConnectionProvider<NativeDnsRuntime>>;
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct DnsLookupHandle(u64);
pub type DnsLookupResult = Result<Vec<IpAddr>, String>;
struct DnsLookupSlot {
    result: Option<DnsLookupResult>,
    abort: AbortHandle,
}
pub struct NativeDnsBridge {
    b: Arc<Bus>,
    pool: LocalPool,
    resolver: Option<Resolver>,
    next_query: u64,
    limit: usize,
    results: Rc<RefCell<HashMap<u64, DnsLookupSlot>>>,
    udp: HashMap<u64, (UdpSocketHandle, SocketAddr, NonZeroU16, Arc<End>)>,
    tcp: HashMap<u64, (TcpSocketHandle, Arc<End>)>,
}
impl NativeDnsBridge {
    pub fn new() -> Self {
        Self::with_capacity(NonZeroUsize::new(DEFAULT_LIMIT).unwrap())
    }
    pub fn with_capacity(limit: NonZeroUsize) -> Self {
        Self {
            b: Arc::new(Bus::new(limit.get())),
            pool: LocalPool::new(),
            resolver: None,
            next_query: 0,
            limit: limit.get(),
            results: Rc::new(RefCell::new(HashMap::new())),
            udp: HashMap::new(),
            tcp: HashMap::new(),
        }
    }
    pub fn configure(
        &mut self,
        servers: &[IpAddr],
    ) -> Result<(), trust_dns_resolver::error::ResolveError> {
        let c = ResolverConfig::from_parts(
            None,
            vec![],
            NameServerConfigGroup::from_ips_clear(servers, 53, true),
        );
        self.resolver = Some(AsyncResolver::new(
            c,
            native_resolver_options(),
            NativeSpawn(self.b.clone()),
        )?);
        Ok(())
    }
    pub fn resolver(&self) -> Option<&Resolver> {
        self.resolver.as_ref()
    }
    pub fn clear(&mut self) {
        self.resolver = None;
    }
    pub fn lookup_ip(&mut self, name: impl Into<String>) -> io::Result<DnsLookupHandle> {
        let resolver = self.resolver.clone().ok_or_else(|| {
            io::Error::new(io::ErrorKind::NotConnected, "DNS servers unavailable")
        })?;
        let id = self.next_query;
        self.next_query = self
            .next_query
            .checked_add(1)
            .expect("DNS query id exhausted");
        let results = self.results.clone();
        if results.borrow().len() == self.limit {
            return Err(io::Error::new(
                io::ErrorKind::WouldBlock,
                "DNS lookup table full",
            ));
        }
        let (abort, registration) = AbortHandle::new_pair();
        results.borrow_mut().insert(
            id,
            DnsLookupSlot {
                result: None,
                abort,
            },
        );
        let name = name.into();
        if let Err(error) = self.pool.spawner().spawn_local(async move {
            let result = Abortable::new(resolver.lookup_ip(name), registration).await;
            if let (Ok(result), Some(slot)) = (result, results.borrow_mut().get_mut(&id)) {
                slot.result = Some(
                    result
                        .map(|addresses| addresses.iter().collect())
                        .map_err(|error| error.to_string()),
                );
            }
        }) {
            self.results.borrow_mut().remove(&id);
            return Err(io::Error::other(error.to_string()));
        }
        Ok(DnsLookupHandle(id))
    }
    pub fn take_result(&mut self, h: DnsLookupHandle) -> Option<DnsLookupResult> {
        let mut results = self.results.borrow_mut();
        if results.get(&h.0)?.result.is_none() {
            return None;
        }
        results.remove(&h.0).and_then(|slot| slot.result)
    }
    pub fn cancel_lookup(&mut self, h: DnsLookupHandle) {
        if let Some(slot) = self.results.borrow_mut().remove(&h.0) {
            slot.abort.abort();
        }
    }
    pub(crate) fn next_timer_deadline(&self) -> Option<Duration> {
        self.b
            .timers
            .lock()
            .unwrap()
            .iter()
            .map(|(deadline, _)| Duration::from_nanos(*deadline))
            .min()
    }
    pub fn spawner(&self) -> futures::executor::LocalSpawner {
        self.pool.spawner()
    }
    pub fn pump(&mut self, rt: &mut Runtime, now: Duration, budget: usize) -> usize {
        let now = u64::try_from(now.as_nanos()).unwrap_or(u64::MAX);
        self.b.now.store(now, Ordering::Relaxed);
        let mut n = 0;
        {
            let mut t = self.b.timers.lock().unwrap();
            let mut p = vec![];
            for (d, w) in t.drain(..) {
                if d <= now && n < budget {
                    w.wake();
                    n += 1;
                } else {
                    p.push((d, w));
                }
            }
            *t = p
        }
        while n < budget {
            let Some(t) = self.b.tasks.lock().unwrap().pop_front() else {
                break;
            };
            self.pool
                .spawner()
                .spawn_local(async move {
                    let _ = t.await;
                })
                .unwrap();
            n += 1;
        }
        for _ in 0..2 {
            if n >= budget {
                break;
            }
            // DNS futures only wake for bounded bus commands, socket input,
            // or timers. Drain the finite ready chain as one work quantum.
            let g = Guard::enter(self.b.clone());
            self.pool.run_until_stalled();
            drop(g);
            while n < budget {
                let c = { self.b.cmd.lock().unwrap().pop_front() };
                let Some(c) = c else { break };
                n += 1;
                self.apply(rt, c)
            }
        }
        for (h, _, _, e) in self.udp.values().take(budget.saturating_sub(n)) {
            if let Ok(Some(b)) = rt.udp_receive(*h) {
                e.push(b);
                n += 1
            }
        }
        for (h, e) in self.tcp.values().take(budget.saturating_sub(n)) {
            let mut b = vec![0; 2048];
            if let Ok(x) = rt.tcp_read(*h, &mut b) {
                if x > 0 {
                    b.truncate(x);
                    e.push(b);
                    n += 1
                }
            }
        }
        n.min(budget)
    }
    fn apply(&mut self, rt: &mut Runtime, c: Cmd) {
        match c {
            Cmd::OU(id, remote, bind, e) => {
                let r = (|| {
                    let SocketAddr::V4(_) = remote else {
                        return Err(io::Error::other("IPv6 DNS unsupported"));
                    };
                    let h = rt.udp_socket().map_err(err)?;
                    let local = bind.and_then(|a| match a {
                        SocketAddr::V4(v) if !v.ip().is_unspecified() => Some(v.ip().octets()),
                        _ => None,
                    });
                    let port = bind
                        .and_then(|a| NonZeroU16::new(a.port()))
                        .unwrap_or(NonZeroU16::new(49152 + (id % 16383) as u16).unwrap());
                    rt.udp_bind(h, local, port).map_err(err)?;
                    self.udp.insert(id, (h, remote, port, e.clone()));
                    Ok(())
                })();
                e.opened(r)
            }
            Cmd::SU(id, a, b) => {
                if let (Some((h, _, _, _)), SocketAddr::V4(a)) = (self.udp.get(&id), a) {
                    let _ =
                        rt.udp_send_to(*h, a.ip().octets(), NonZeroU16::new(a.port()).unwrap(), &b);
                }
            }
            Cmd::OT(id, a, e) => {
                let r = (|| {
                    let SocketAddr::V4(a) = a else {
                        return Err(io::Error::other("IPv6 DNS unsupported"));
                    };
                    let h = rt.tcp_socket().map_err(err)?;
                    rt.tcp_connect(h, a.ip().octets(), NonZeroU16::new(a.port()).unwrap())
                        .map_err(err)?;
                    self.tcp.insert(id, (h, e.clone()));
                    Ok(())
                })();
                e.opened(r)
            }
            Cmd::WT(id, b) => {
                if let Some((h, _)) = self.tcp.get(&id) {
                    let _ = rt.tcp_write(*h, &b);
                }
            }
            Cmd::CT(id) => {
                if let Some((h, _)) = self.tcp.remove(&id) {
                    let _ = rt.tcp_close(h);
                }
            }
        }
    }
}
fn err(e: crate::RuntimeError) -> io::Error {
    io::Error::other(format!("{e:?}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::num::{NonZeroU16, NonZeroU64};
    use trust_dns_proto::op::Message;
    use trust_dns_proto::rr::{Name, RData, Record};

    #[test]
    fn native_resolver_never_reads_the_host_filesystem() {
        let mut expected = ResolverOpts::default();
        expected.use_hosts_file = false;
        assert_eq!(native_resolver_options(), expected);
    }

    #[test]
    fn pinned_resolver_reaches_native_udp_bridge() {
        let mut runtime = Runtime::new(
            64,
            [9; 8192],
            NonZeroU64::new(1).unwrap(),
            [0x02, 0, 0, 0, 0, 2],
            1500,
        )
        .unwrap();
        runtime.apply_ipv4([192, 0, 2, 2], 24, None).unwrap();
        let mut bridge = NativeDnsBridge::new();
        bridge.configure(&[IpAddr::from([192, 0, 2, 53])]).unwrap();
        let resolver = bridge.resolver().unwrap().clone();
        bridge
            .spawner()
            .spawn_local(async move {
                let _ = resolver.lookup_ip("native.test.").await;
            })
            .unwrap();

        let mut work = 0;
        for _ in 0..8 {
            work += bridge.pump(&mut runtime, Duration::ZERO, DEFAULT_LIMIT);
        }
        assert!(work > 0);
        let frame = runtime
            .take_tx()
            .expect("resolver initiates ARP for DNS server");
        assert_eq!(&frame.as_bytes()[12..14], &[0x08, 0x06]);
        assert_eq!(&frame.as_bytes()[38..42], &[192, 0, 2, 53]);
    }

    fn exchange(a: &mut Runtime, b: &mut Runtime) {
        for _ in 0..16 {
            let mut work = 0;
            while let Some(frame) = a.take_tx() {
                b.receive_frame(frame);
                work += 1;
            }
            while let Some(frame) = b.take_tx() {
                a.receive_frame(frame);
                work += 1;
            }
            if work == 0 {
                return;
            }
        }
    }

    #[test]
    fn pinned_resolver_transaction_trace_returns_answer() {
        let mut client = Runtime::new(
            16,
            [3; 8192],
            NonZeroU64::new(1).unwrap(),
            [2, 0, 0, 0, 0, 1],
            1500,
        )
        .unwrap();
        let mut server = Runtime::new(
            16,
            [4; 8192],
            NonZeroU64::new(2).unwrap(),
            [2, 0, 0, 0, 0, 2],
            1500,
        )
        .unwrap();
        client.apply_ipv4([192, 0, 2, 2], 24, None).unwrap();
        server.apply_ipv4([192, 0, 2, 53], 24, None).unwrap();
        let server_socket = server.udp_socket().unwrap();
        server
            .udp_bind(
                server_socket,
                Some([192, 0, 2, 53]),
                NonZeroU16::new(53).unwrap(),
            )
            .unwrap();

        let mut bridge = NativeDnsBridge::new();
        bridge.configure(&[IpAddr::from([192, 0, 2, 53])]).unwrap();
        let query = bridge.lookup_ip("native.test.").unwrap();
        assert!(bridge.take_result(query).is_none());
        assert!(bridge.take_result(query).is_none());
        for _ in 0..8 {
            bridge.pump(&mut client, Duration::ZERO, DEFAULT_LIMIT);
            exchange(&mut client, &mut server);
        }
        let request = server
            .udp_receive(server_socket)
            .unwrap()
            .expect("Trust-DNS query");
        let request = Message::from_vec(&request).unwrap();
        let mut response = Message::new();
        response
            .set_id(request.id())
            .set_message_type(trust_dns_proto::op::MessageType::Response);
        response.add_query(request.queries()[0].clone());
        response.add_answer(Record::from_rdata(
            Name::from_ascii("native.test.").unwrap(),
            60,
            RData::A(std::net::Ipv4Addr::new(192, 0, 2, 99)),
        ));
        let client_port = bridge.udp.values().next().unwrap().2;
        server
            .udp_send_to(
                server_socket,
                [192, 0, 2, 2],
                client_port,
                &response.to_vec().unwrap(),
            )
            .unwrap();
        for _ in 0..8 {
            exchange(&mut client, &mut server);
            bridge.pump(&mut client, Duration::ZERO, DEFAULT_LIMIT);
        }
        assert_eq!(
            bridge.take_result(query).unwrap().unwrap(),
            [IpAddr::from([192, 0, 2, 99])]
        );
    }

    #[test]
    fn configured_capacity_applies_beyond_legacy_dns_limit() {
        let bridge = NativeDnsBridge::with_capacity(NonZeroUsize::new(65).unwrap());
        for id in 0..65 {
            bridge.b.send(Cmd::CT(id)).unwrap();
        }
        assert_eq!(
            bridge.b.send(Cmd::CT(65)).unwrap_err().kind(),
            io::ErrorKind::WouldBlock
        );
        assert_eq!(
            bridge
                .b
                .send(Cmd::WT(0, vec![0; SOCKET_MESSAGE_BYTES_LIMIT + 1]))
                .unwrap_err()
                .kind(),
            io::ErrorKind::InvalidData
        );
        let end = End::new(SOCKET_MESSAGE_LIMIT);
        end.push(vec![0; SOCKET_MESSAGE_BYTES_LIMIT + 1]);
        assert!(end.rx.lock().unwrap().is_empty());
    }
}
