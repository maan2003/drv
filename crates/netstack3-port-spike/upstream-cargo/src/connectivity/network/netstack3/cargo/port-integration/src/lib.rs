//! Small, authority-free native bindings for the pinned Netstack3 core.
//!
//! This is intentionally a synchronous embedding. The owner injects entropy
//! and time and explicitly drains timers, frames, events, and socket queues.

#![recursion_limit = "256"]

pub mod service;

use std::collections::{BTreeMap, HashMap, VecDeque};
use std::convert::Infallible;
use std::fmt::{self, Debug, Display};
use std::num::{NonZeroU16, NonZeroU64, NonZeroUsize};
use std::ops::Deref;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use net_types::UnicastAddr;
use net_types::ethernet::Mac;
use net_types::ip::{AddrSubnet, Ip, IpVersion, Ipv4, Ipv4Addr, Ipv6, Ipv6Addr, Mtu, Subnet};
use net_types::{SpecifiedAddr, ZonedAddr};
use netstack3_base::socket::ShutdownType;
use netstack3_base::sync::{DynDebugReferences, RcNotifier};
use netstack3_base::{
    AddressResolutionFailed, AtomicInstant, ChecksumOffloadResult, DeferredResourceRemovalContext,
    EventContext, Instant, InstantBindingsTypes, InstantContext, LinkDevice, MarkDomain,
    MatcherBindingsTypes, ReferenceNotifiers, RemoveResourceResultWithContext, RngContext,
    SettingsContext, SocketDiagnosticsSeed, TimerBindingsTypes, TimerContext,
    TxMetadataBindingsTypes,
};
use netstack3_core::PendingDatagramSocketError;
use netstack3_core::device::{
    BatchSize, DeviceId, EthernetCreationProperties, EthernetDeviceId, EthernetLinkDevice,
    EthernetWeakDeviceId, LoopbackDeviceId, MaxEthernetFrameSize, PureIpDeviceId,
    RecvEthernetFrameMeta, TransmitQueueConfiguration, WeakDeviceId,
};
use netstack3_core::device_socket::{
    DeviceSocketMetadata, EthernetHeaderParams, Protocol, TargetDevice,
};
use netstack3_core::ip::{
    IpDeviceConfigurationUpdate, Ipv4DeviceConfigurationUpdate, Ipv6DeviceConfigurationUpdate,
    RouteDiscoveryConfigurationUpdate,
};
use netstack3_core::routes::{AddableEntry, AddableMetric, Generation, RawMetric};
use netstack3_core::udp::UdpRemotePort;
use netstack3_core::{CoreTxMetadata, IpExt, StackState, StackStateBuilder, TimerId};
use netstack3_device::queue::{ReceiveQueueBindingsContext, TransmitQueueBindingsContext};
use netstack3_device::socket::{
    DeviceSocketBindingsContext, DeviceSocketTypes, ReceiveFrameError, SocketId,
};
use netstack3_device::{
    DeviceBufferBindingsTypes, DeviceClassMatcher, DeviceIdAndNameMatcher,
    DeviceLayerEventDispatcher, DeviceLayerStateTypes, DeviceSendFrameError,
};
use netstack3_filter::{
    FilterIpExt, FilterIpPacket, Marks, SocketEgressFilterResult, SocketInfo,
    SocketIngressFilterResult,
};
use netstack3_filter::{SocketOpsFilter, SocketOpsFilterBindingContext};
use netstack3_icmp_echo::{
    IcmpEchoBindingsContext, IcmpEchoBindingsTypes, IcmpEchoSettings, IcmpSocketId,
    ReceiveIcmpEchoError,
};
use netstack3_ip::nud::{LinkResolutionContext, LinkResolutionNotifier};
use netstack3_ip::raw::{
    RawIpSocketId, RawIpSocketsBindingsContext, RawIpSocketsBindingsTypes, ReceivePacketError,
};
use netstack3_ip::{IpRoutingBindingsTypes, MarksBindingsContext};
use netstack3_port_spike::control_plane::{DhcpOffer, MAX_CONTROL_DATAGRAM_LEN};
use netstack3_port_spike::{
    EthernetDeviceEvent, EthernetFrame, NetworkServiceEndpoint, StackEthernetEndpoint,
};
use netstack3_tcp::{Buffer, BufferLimits, IntoBuffers, ReceiveBuffer, SendBuffer};
use netstack3_tcp::{
    BufferSizes, ListenerNotifier, TcpBindingsTypes, TcpSettings, TcpSocketDestructionContext,
    TcpSocketDiagnostics, TcpSocketId,
};
use netstack3_udp::{
    ReceiveUdpError, UdpBindingsTypes, UdpPacketMeta, UdpReceiveBindingsContext, UdpSettings,
    UdpSocketId,
};
use packet::{Buf, BufferMut, FragmentedByteSlice, InnerPacketBuilder, NestableSerializer as _};
use packet_formats::ethernet::EtherType;
use packet_formats::ip::{IpProto, Ipv4Proto};
use packet_formats::ipv4::Ipv4PacketBuilder;
use packet_formats::udp::UdpPacketBuilder;
use rand::{CryptoRng, RngCore};
use zerocopy::SplitByteSlice;

/// A deterministic monotonic instant expressed as nanoseconds from an
/// embedding-defined epoch.
#[derive(Clone, Copy, Debug, Default, Eq, Ord, PartialEq, PartialOrd)]
pub struct NativeInstant(u64);

impl NativeInstant {
    pub const ZERO: Self = Self(0);
    pub fn from_nanos(nanos: u64) -> Self {
        Self(nanos)
    }
    pub fn as_nanos(self) -> u64 {
        self.0
    }
}

impl netstack3_base::InspectableValue for NativeInstant {
    fn record<I: netstack3_base::Inspector>(&self, name: &str, inspector: &mut I) {
        inspector.record_uint(name, self.0)
    }
}

impl Instant for NativeInstant {
    fn checked_duration_since(&self, earlier: Self) -> Option<Duration> {
        self.0.checked_sub(earlier.0).map(Duration::from_nanos)
    }
    fn checked_add(&self, duration: Duration) -> Option<Self> {
        u64::try_from(duration.as_nanos())
            .ok()
            .and_then(|d| self.0.checked_add(d))
            .map(Self)
    }
    fn saturating_add(&self, duration: Duration) -> Self {
        Self(
            self.0
                .saturating_add(u64::try_from(duration.as_nanos()).unwrap_or(u64::MAX)),
        )
    }
    fn checked_sub(&self, duration: Duration) -> Option<Self> {
        u64::try_from(duration.as_nanos())
            .ok()
            .and_then(|d| self.0.checked_sub(d))
            .map(Self)
    }
}

#[derive(Debug, Default)]
pub struct AtomicNativeInstant(AtomicU64);
impl AtomicInstant<NativeInstant> for AtomicNativeInstant {
    fn new(instant: NativeInstant) -> Self {
        Self(AtomicU64::new(instant.0))
    }
    fn load(&self, ordering: Ordering) -> NativeInstant {
        NativeInstant(self.0.load(ordering))
    }
    fn store(&self, instant: NativeInstant, ordering: Ordering) {
        self.0.store(instant.0, ordering)
    }
    fn store_max(&self, instant: NativeInstant, ordering: Ordering) {
        self.0.fetch_max(instant.0, ordering);
    }
}

/// Entropy consumed only from bytes explicitly supplied by the embedding.
#[derive(Debug, Default)]
pub struct InjectedEntropy(VecDeque<u8>);
impl InjectedEntropy {
    pub fn inject(&mut self, bytes: impl IntoIterator<Item = u8>) {
        self.0.extend(bytes)
    }
}
impl RngCore for InjectedEntropy {
    fn next_u32(&mut self) -> u32 {
        let mut b = [0; 4];
        self.fill_bytes(&mut b);
        u32::from_le_bytes(b)
    }
    fn next_u64(&mut self) -> u64 {
        let mut b = [0; 8];
        self.fill_bytes(&mut b);
        u64::from_le_bytes(b)
    }
    fn fill_bytes(&mut self, dst: &mut [u8]) {
        for byte in dst {
            *byte = self
                .0
                .pop_front()
                .expect("native Netstack3 entropy exhausted")
        }
    }
}
impl CryptoRng for InjectedEntropy {}

#[derive(Debug)]
pub struct NativeTimer {
    id: u64,
    dispatch: TimerId<NativeBindingsCtx>,
    scheduled: Arc<Mutex<Option<NativeInstant>>>,
}

/// An outbound frame copied into bindings-owned storage.
#[derive(Debug)]
pub enum TxFrame {
    Ethernet(EthernetWeakDeviceId<NativeBindingsCtx>, Vec<u8>),
    PureIp(IpVersion, Vec<u8>),
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub enum ReadinessEvent {
    UdpWritable(bool),
    TcpIncoming(usize),
    RxReady,
    TxReady,
}

#[derive(Debug, Default)]
struct Queues {
    tx: VecDeque<TxFrame>,
    events: VecDeque<String>,
    readiness: VecDeque<ReadinessEvent>,
}

/// Standalone production-core bindings with bounded externally visible queues.
pub struct NativeBindingsCtx {
    now: NativeInstant,
    next_timer: u64,
    timers: BTreeMap<
        (NativeInstant, u64),
        (
            TimerId<NativeBindingsCtx>,
            Arc<Mutex<Option<NativeInstant>>>,
        ),
    >,
    entropy: InjectedEntropy,
    capacity: usize,
    queues: Queues,
    udp_v4: HashMap<String, VecDeque<Vec<u8>>>,
    udp_v6: HashMap<String, VecDeque<Vec<u8>>>,
    udp_pending: usize,
    tcp_settings: TcpSettings,
    udp_settings: UdpSettings,
    icmp_settings: IcmpEchoSettings,
}

impl Debug for NativeBindingsCtx {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("NativeBindingsCtx")
            .field("now", &self.now)
            .field("capacity", &self.capacity)
            .finish_non_exhaustive()
    }
}

impl NativeBindingsCtx {
    pub fn new(queue_capacity: usize, entropy: impl IntoIterator<Item = u8>) -> Self {
        let mut rng = InjectedEntropy::default();
        rng.inject(entropy);
        let min = std::num::NonZeroUsize::new(4096).unwrap();
        let default = std::num::NonZeroUsize::new(64 * 1024).unwrap();
        let max = std::num::NonZeroUsize::new(4 * 1024 * 1024).unwrap();
        let sizes = netstack3_base::BufferSizeSettings::new(min, default, max).unwrap();
        Self {
            now: NativeInstant::ZERO,
            next_timer: 0,
            timers: BTreeMap::new(),
            entropy: rng,
            capacity: queue_capacity,
            queues: Queues::default(),
            udp_v4: HashMap::new(),
            udp_v6: HashMap::new(),
            udp_pending: 0,
            tcp_settings: TcpSettings {
                receive_buffer: sizes,
                send_buffer: sizes,
            },
            udp_settings: UdpSettings::default(),
            icmp_settings: IcmpEchoSettings::default(),
        }
    }
    pub fn set_now(&mut self, now: NativeInstant) {
        assert!(now >= self.now);
        self.now = now;
    }
    pub fn advance(&mut self, by: Duration) {
        self.now = self.now.saturating_add(by);
    }
    pub fn inject_entropy(&mut self, bytes: impl IntoIterator<Item = u8>) {
        self.entropy.inject(bytes);
    }
    pub fn take_tx(&mut self) -> Option<TxFrame> {
        self.queues.tx.pop_front()
    }
    pub fn take_event(&mut self) -> Option<String> {
        self.queues.events.pop_front()
    }
    pub fn take_readiness(&mut self) -> Option<ReadinessEvent> {
        self.queues.readiness.pop_front()
    }
    pub fn dispatch_due(&mut self, stack: &StackState<Self>, budget: usize) -> usize {
        let mut count = 0;
        while count < budget {
            let Some((&key, _)) = self.timers.first_key_value() else {
                break;
            };
            if key.0 > self.now {
                break;
            }
            let (dispatch, scheduled) = self.timers.remove(&key).unwrap();
            *scheduled.lock().unwrap() = None;
            stack.api(&mut *self).handle_timer(dispatch, key.1);
            count += 1;
        }
        count
    }
    pub fn take_udp<I: IpExt>(
        &mut self,
        id: &UdpSocketId<I, WeakDeviceId<Self>, Self>,
    ) -> Option<Vec<u8>> {
        let map = if I::VERSION == IpVersion::V4 {
            &mut self.udp_v4
        } else {
            &mut self.udp_v6
        };
        let packet = map
            .get_mut(&format!("{id:?}"))
            .and_then(VecDeque::pop_front);
        if packet.is_some() {
            self.udp_pending -= 1
        }
        packet
    }
    pub fn build_stack(&mut self) -> StackState<Self> {
        StackStateBuilder::default().build_with_ctx(self)
    }
    fn push_bounded<T>(capacity: usize, queue: &mut VecDeque<T>, item: T) -> Result<(), T> {
        if queue.len() >= capacity {
            Err(item)
        } else {
            queue.push_back(item);
            Ok(())
        }
    }
}

impl InstantBindingsTypes for NativeBindingsCtx {
    type Instant = NativeInstant;
    type AtomicInstant = AtomicNativeInstant;
}
impl InstantContext for NativeBindingsCtx {
    fn now(&self) -> NativeInstant {
        self.now
    }
}
impl TimerBindingsTypes for NativeBindingsCtx {
    type Timer = NativeTimer;
    type DispatchId = TimerId<Self>;
    type UniqueTimerId = u64;
}
impl TimerContext for NativeBindingsCtx {
    fn new_timer(&mut self, dispatch: Self::DispatchId) -> Self::Timer {
        let id = self.next_timer;
        self.next_timer = self.next_timer.checked_add(1).expect("timer id exhausted");
        NativeTimer {
            id,
            dispatch,
            scheduled: Arc::new(Mutex::new(None)),
        }
    }
    fn schedule_timer_instant(
        &mut self,
        time: NativeInstant,
        timer: &mut NativeTimer,
    ) -> Option<NativeInstant> {
        let old = timer.scheduled.lock().unwrap().replace(time);
        if let Some(old) = old {
            self.timers.remove(&(old, timer.id));
        }
        self.timers.insert(
            (time, timer.id),
            (timer.dispatch.clone(), timer.scheduled.clone()),
        );
        old
    }
    fn cancel_timer(&mut self, timer: &mut NativeTimer) -> Option<NativeInstant> {
        let old = timer.scheduled.lock().unwrap().take();
        if let Some(old) = old {
            self.timers.remove(&(old, timer.id));
        }
        old
    }
    fn scheduled_instant(&self, timer: &mut NativeTimer) -> Option<NativeInstant> {
        *timer.scheduled.lock().unwrap()
    }
    fn unique_timer_id(&self, timer: &NativeTimer) -> u64 {
        timer.id
    }
}

impl RngContext for NativeBindingsCtx {
    type Rng<'a> = &'a mut InjectedEntropy;
    fn rng(&mut self) -> Self::Rng<'_> {
        &mut self.entropy
    }
}
impl TxMetadataBindingsTypes for NativeBindingsCtx {
    type TxMetadata = CoreTxMetadata<Self>;
}

#[derive(Debug)]
struct WritableState {
    capacity: usize,
    queue: VecDeque<ReadinessEvent>,
}
#[derive(Clone, Debug)]
pub struct NativeWritable(Arc<Mutex<WritableState>>);
impl Default for NativeWritable {
    fn default() -> Self {
        Self::new(64)
    }
}
impl NativeWritable {
    pub fn new(capacity: usize) -> Self {
        Self(Arc::new(Mutex::new(WritableState {
            capacity,
            queue: VecDeque::new(),
        })))
    }
    pub fn take(&self) -> Option<ReadinessEvent> {
        self.0.lock().unwrap().queue.pop_front()
    }
}
impl netstack3_base::socket::SocketWritableListener for NativeWritable {
    fn on_writable_changed(&mut self, writable: bool) {
        let mut s = self.0.lock().unwrap();
        if s.queue.len() < s.capacity {
            s.queue.push_back(ReadinessEvent::UdpWritable(writable));
        }
    }
}

#[derive(Debug, Default)]
struct TcpStorage {
    bytes: Vec<u8>,
    readable: usize,
    capacity: usize,
}
#[derive(Clone, Debug, Default)]
pub struct NativeReceiveBuffer(Arc<Mutex<TcpStorage>>);
#[derive(Clone, Debug, Default)]
pub struct NativeSendBuffer(Arc<Mutex<TcpStorage>>);
#[derive(Clone, Debug)]
pub struct NativeTcpBuffers {
    pub receive: NativeReceiveBuffer,
    pub send: NativeSendBuffer,
}

#[derive(Debug)]
pub struct NativePayload(Vec<u8>);
impl netstack3_base::PayloadLen for NativePayload {
    fn len(&self) -> usize {
        self.0.len()
    }
}
impl netstack3_base::Payload for NativePayload {
    fn slice(mut self, range: std::ops::Range<u32>) -> Self {
        self.0 = self.0[range.start as usize..range.end as usize].to_vec();
        self
    }
    fn partial_copy(&self, offset: usize, dst: &mut [u8]) {
        dst.copy_from_slice(&self.0[offset..offset + dst.len()])
    }
    fn partial_copy_uninit(&self, offset: usize, dst: &mut [std::mem::MaybeUninit<u8>]) {
        for (to, from) in dst.iter_mut().zip(&self.0[offset..]) {
            to.write(*from);
        }
    }
    fn new_empty() -> Self {
        Self(Vec::new())
    }
}
impl InnerPacketBuilder for NativePayload {
    fn bytes_len(&self) -> usize {
        self.0.len()
    }
    fn serialize(&self, dst: &mut [u8]) {
        dst.copy_from_slice(&self.0)
    }
}

impl Buffer for NativeReceiveBuffer {
    fn limits(&self) -> BufferLimits {
        let s = self.0.lock().unwrap();
        BufferLimits {
            capacity: s.capacity,
            len: s.readable,
        }
    }
    fn target_capacity(&self) -> usize {
        self.0.lock().unwrap().capacity
    }
    fn request_capacity(&mut self, size: usize) {
        self.0.lock().unwrap().capacity = size;
    }
}
impl ReceiveBuffer for NativeReceiveBuffer {
    fn write_at<P: netstack3_base::Payload>(&mut self, offset: usize, data: &P) -> usize {
        let mut s = self.0.lock().unwrap();
        let start = s.readable + offset;
        let count = data.len().min(s.capacity.saturating_sub(start));
        let len = s.bytes.len().max(start + count);
        s.bytes.resize(len, 0);
        data.partial_copy(0, &mut s.bytes[start..start + count]);
        count
    }
    fn make_readable(&mut self, count: usize, _has_outstanding: bool) {
        let mut s = self.0.lock().unwrap();
        assert!(s.readable + count <= s.capacity);
        s.readable += count;
    }
}
impl Buffer for NativeSendBuffer {
    fn limits(&self) -> BufferLimits {
        let s = self.0.lock().unwrap();
        BufferLimits {
            capacity: s.capacity,
            len: s.readable,
        }
    }
    fn target_capacity(&self) -> usize {
        self.0.lock().unwrap().capacity
    }
    fn request_capacity(&mut self, size: usize) {
        self.0.lock().unwrap().capacity = size;
    }
}
impl SendBuffer for NativeSendBuffer {
    type Payload<'a> = NativePayload;
    fn mark_read(&mut self, count: usize) {
        let mut s = self.0.lock().unwrap();
        assert!(count <= s.readable);
        s.bytes.drain(..count);
        s.readable -= count;
    }
    fn peek_with<'a, F, R>(&'a mut self, offset: usize, f: F) -> R
    where
        F: FnOnce(Self::Payload<'a>) -> R,
    {
        let s = self.0.lock().unwrap();
        assert!(offset <= s.readable);
        f(NativePayload(s.bytes[offset..s.readable].to_vec()))
    }
}
impl NativeTcpBuffers {
    pub fn new(sizes: BufferSizes) -> Self {
        Self {
            receive: NativeReceiveBuffer(Arc::new(Mutex::new(TcpStorage {
                capacity: sizes.receive,
                ..Default::default()
            }))),
            send: NativeSendBuffer(Arc::new(Mutex::new(TcpStorage {
                capacity: sizes.send,
                ..Default::default()
            }))),
        }
    }
    pub fn write(&self, bytes: &[u8]) -> usize {
        let mut s = self.send.0.lock().unwrap();
        let n = bytes.len().min(s.capacity - s.readable);
        s.bytes.extend_from_slice(&bytes[..n]);
        s.readable += n;
        n
    }
    pub fn read(&self, out: &mut [u8]) -> usize {
        let mut s = self.receive.0.lock().unwrap();
        let n = out.len().min(s.readable);
        out[..n].copy_from_slice(&s.bytes[..n]);
        s.bytes.drain(..n);
        s.readable -= n;
        n
    }
}

#[derive(Clone, Debug, Default)]
pub struct NativeTcpSocketData {
    buffers: Option<NativeTcpBuffers>,
    incoming: Arc<Mutex<usize>>,
}
impl NativeTcpSocketData {
    pub fn buffers(sizes: BufferSizes) -> Self {
        Self {
            buffers: Some(NativeTcpBuffers::new(sizes)),
            incoming: Default::default(),
        }
    }
}
impl NativeTcpSocketData {
    pub fn client_buffers(&self) -> Option<NativeTcpBuffers> {
        self.buffers.clone()
    }
    pub fn pending_connections(&self) -> usize {
        *self.incoming.lock().unwrap()
    }
}
impl IntoBuffers<NativeReceiveBuffer, NativeSendBuffer> for NativeTcpSocketData {
    fn into_buffers(self, sizes: BufferSizes) -> (NativeReceiveBuffer, NativeSendBuffer) {
        let b = self.buffers.unwrap_or_else(|| NativeTcpBuffers::new(sizes));
        (b.receive, b.send)
    }
}
impl ListenerNotifier for NativeTcpSocketData {
    fn new_incoming_connections(&mut self, n: usize) {
        *self.incoming.lock().unwrap() = n;
    }
}
impl TcpBindingsTypes for NativeBindingsCtx {
    type ReceiveBuffer = NativeReceiveBuffer;
    type SendBuffer = NativeSendBuffer;
    type ReturnedBuffers = NativeTcpBuffers;
    type ListenerNotifierOrProvidedBuffers = NativeTcpSocketData;
    fn new_passive_open_buffers(
        s: BufferSizes,
    ) -> (NativeReceiveBuffer, NativeSendBuffer, NativeTcpBuffers) {
        let b = NativeTcpBuffers::new(s);
        (b.receive.clone(), b.send.clone(), b)
    }
}

#[derive(Clone, Copy, Debug, Default)]
pub struct NativeDeviceState;
impl DeviceClassMatcher<()> for NativeDeviceState {
    fn device_class_matches(&self, _: &()) -> bool {
        true
    }
}
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct NativeDeviceIdentifier(pub NonZeroU64);
impl Display for NativeDeviceIdentifier {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        Display::fmt(&self.0, f)
    }
}
impl DeviceIdAndNameMatcher for NativeDeviceIdentifier {
    fn id_matches(&self, id: &NonZeroU64) -> bool {
        self.0 == *id
    }
    fn name_matches(&self, _: &str) -> bool {
        false
    }
}

impl MatcherBindingsTypes for NativeBindingsCtx {
    type DeviceClass = ();
    type BindingsPacketMatcher = Infallible;
}
impl DeviceBufferBindingsTypes for NativeBindingsCtx {
    type TxBuffer = Buf<Vec<u8>>;
    type TxAllocator = netstack3_device::queue::BufVecU8Allocator;
}
impl DeviceLayerStateTypes for NativeBindingsCtx {
    type LoopbackDeviceState = NativeDeviceState;
    type EthernetDeviceState = NativeDeviceState;
    type BlackholeDeviceState = NativeDeviceState;
    type PureIpDeviceState = NativeDeviceState;
    type DeviceIdentifier = NativeDeviceIdentifier;
}
impl DeviceSocketTypes for NativeBindingsCtx {
    type SocketState<D: Send + Sync + Debug> = Mutex<VecDeque<(WeakDeviceId<Self>, Vec<u8>)>>;
}
impl RawIpSocketsBindingsTypes for NativeBindingsCtx {
    type RawIpSocketState<I: Ip> = ();
}
impl IcmpEchoBindingsTypes for NativeBindingsCtx {
    type ExternalData<I: Ip> = ();
    type SocketWritableListener = NativeWritable;
}
impl UdpBindingsTypes for NativeBindingsCtx {
    type ExternalData<I: Ip> = ();
    type SocketWritableListener = NativeWritable;
}
impl IpRoutingBindingsTypes for NativeBindingsCtx {
    type RoutingTableId = u32;
}

#[derive(Debug)]
pub struct NativeLinkNotifier;
impl<D: LinkDevice> LinkResolutionNotifier<D> for NativeLinkNotifier {
    type Observer = ();
    fn new() -> (Self, ()) {
        (Self, ())
    }
    fn notify(self, _result: Result<UnicastAddr<D::Address>, AddressResolutionFailed>) {}
}
impl<D: LinkDevice> LinkResolutionContext<D> for NativeBindingsCtx {
    type Notifier = NativeLinkNotifier;
}

#[derive(Debug)]
pub struct NativeReferenceNotifier<T>(Arc<Mutex<Option<T>>>);
#[derive(Debug)]
pub struct NativeReferenceReceiver<T>(Arc<Mutex<Option<T>>>);
impl<T> NativeReferenceReceiver<T> {
    pub fn take(&self) -> Option<T> {
        self.0.lock().unwrap().take()
    }
    pub fn is_ready(&self) -> bool {
        self.0.lock().unwrap().is_some()
    }
}
impl<T: Send> RcNotifier<T> for NativeReferenceNotifier<T> {
    fn notify(&mut self, data: T) {
        *self.0.lock().unwrap() = Some(data);
    }
}
impl ReferenceNotifiers for NativeBindingsCtx {
    type ReferenceReceiver<T: 'static> = NativeReferenceReceiver<T>;
    type ReferenceNotifier<T: Send + 'static> = NativeReferenceNotifier<T>;
    fn new_reference_notifier<T: Send + 'static>(
        _refs: DynDebugReferences,
    ) -> (NativeReferenceNotifier<T>, NativeReferenceReceiver<T>) {
        let inner = Arc::new(Mutex::new(None));
        (
            NativeReferenceNotifier(inner.clone()),
            NativeReferenceReceiver(inner),
        )
    }
}
impl DeferredResourceRemovalContext for NativeBindingsCtx {
    fn defer_removal<T: Send + 'static>(&mut self, receiver: NativeReferenceReceiver<T>) {
        // Keeping the receiver alive is not necessary for correctness; the
        // notifier still owns the shared cell until core releases its refs.
        drop(receiver)
    }
}

impl SettingsContext<TcpSettings> for NativeBindingsCtx {
    fn settings(&self) -> impl Deref<Target = TcpSettings> + '_ {
        &self.tcp_settings
    }
}
impl SettingsContext<UdpSettings> for NativeBindingsCtx {
    fn settings(&self) -> impl Deref<Target = UdpSettings> + '_ {
        &self.udp_settings
    }
}
impl SettingsContext<IcmpEchoSettings> for NativeBindingsCtx {
    fn settings(&self) -> impl Deref<Target = IcmpEchoSettings> + '_ {
        &self.icmp_settings
    }
}

impl MarksBindingsContext for NativeBindingsCtx {
    fn marks_to_keep_on_egress() -> &'static [MarkDomain] {
        &[]
    }
    fn marks_to_set_on_ingress() -> &'static [MarkDomain] {
        &[]
    }
}

pub struct PassAllSocketFilter;
impl<D> SocketOpsFilter<D> for PassAllSocketFilter {
    fn on_egress<I: FilterIpExt, P: FilterIpPacket<I>>(
        &self,
        _packet: &P,
        _device: &D,
        _info: SocketInfo,
        _marks: &Marks,
    ) -> SocketEgressFilterResult {
        SocketEgressFilterResult::Pass { congestion: false }
    }
    fn on_ingress(
        &self,
        _version: IpVersion,
        _packet: FragmentedByteSlice<'_, &[u8]>,
        _header_len: usize,
        _device: &D,
        _info: SocketInfo,
        _marks: &Marks,
    ) -> SocketIngressFilterResult {
        SocketIngressFilterResult::Accept
    }
}
impl SocketOpsFilterBindingContext<DeviceId<Self>> for NativeBindingsCtx {
    fn socket_ops_filter(&self) -> impl SocketOpsFilter<DeviceId<Self>> {
        PassAllSocketFilter
    }
}

impl<T: Debug> EventContext<T> for NativeBindingsCtx {
    fn on_event(&mut self, event: T) {
        let event = format!("{event:?}");
        let _ = Self::push_bounded(self.capacity, &mut self.queues.events, event);
    }
}

impl<I: IpExt> UdpReceiveBindingsContext<I, DeviceId<Self>> for NativeBindingsCtx {
    fn receive_udp(
        &mut self,
        id: &UdpSocketId<I, WeakDeviceId<Self>, Self>,
        _device: &DeviceId<Self>,
        _meta: UdpPacketMeta<I>,
        body: &[u8],
    ) -> Result<(), ReceiveUdpError> {
        if self.udp_pending >= self.capacity {
            return Err(ReceiveUdpError::QueueFull);
        }
        let map = if I::VERSION == IpVersion::V4 {
            &mut self.udp_v4
        } else {
            &mut self.udp_v6
        };
        let queue = map.entry(format!("{id:?}")).or_default();
        queue.push_back(body.to_vec());
        self.udp_pending += 1;
        Ok(())
    }
    fn on_socket_error(
        &mut self,
        id: &UdpSocketId<I, WeakDeviceId<Self>, Self>,
        err: PendingDatagramSocketError,
    ) {
        let event = format!("UDP {id:?}: {err:?}");
        let _ = Self::push_bounded(self.capacity, &mut self.queues.events, event);
    }
}
impl<I: IpExt> IcmpEchoBindingsContext<I, DeviceId<Self>> for NativeBindingsCtx {
    fn receive_icmp_echo_reply<B: BufferMut>(
        &mut self,
        _conn: &IcmpSocketId<I, WeakDeviceId<Self>, Self>,
        _device: &DeviceId<Self>,
        _src: I::Addr,
        _dst: I::Addr,
        _id: u16,
        _data: B,
    ) -> Result<(), ReceiveIcmpEchoError> {
        Ok(())
    }
}
impl<I: IpExt> RawIpSocketsBindingsContext<I, DeviceId<Self>> for NativeBindingsCtx {
    fn receive_packet<B: SplitByteSlice>(
        &self,
        _socket: &RawIpSocketId<I, WeakDeviceId<Self>, Self>,
        _packet: &I::Packet<B>,
        _device: &DeviceId<Self>,
    ) -> Result<(), ReceivePacketError> {
        Ok(())
    }
}
impl DeviceSocketBindingsContext<DeviceId<Self>> for NativeBindingsCtx {
    fn receive_frame(
        &self,
        socket: &SocketId<Self>,
        device: &DeviceId<Self>,
        _frame: netstack3_device::socket::Frame<&[u8]>,
        raw: &[u8],
    ) -> Result<(), ReceiveFrameError> {
        let mut q = socket.socket_state().lock().unwrap();
        if q.len() >= self.capacity {
            return Err(ReceiveFrameError::QueueFull);
        }
        q.push_back((device.downgrade(), raw.to_vec()));
        Ok(())
    }
}
impl ReceiveQueueBindingsContext<LoopbackDeviceId<Self>> for NativeBindingsCtx {
    fn wake_rx_task(&mut self, _device: &LoopbackDeviceId<Self>) {
        let _ = Self::push_bounded(
            self.capacity,
            &mut self.queues.readiness,
            ReadinessEvent::RxReady,
        );
    }
}
impl<D: Clone + Into<DeviceId<Self>>> TransmitQueueBindingsContext<D> for NativeBindingsCtx {
    fn wake_tx_task(&mut self, _device: &D) {
        let _ = Self::push_bounded(
            self.capacity,
            &mut self.queues.readiness,
            ReadinessEvent::TxReady,
        );
    }
}
impl DeviceLayerEventDispatcher for NativeBindingsCtx {
    type DequeueContext = ();
    fn send_ethernet_frame(
        &mut self,
        device: &EthernetDeviceId<Self>,
        frame: Buf<Vec<u8>>,
        _ctx: Option<&mut ()>,
        _csum: Option<ChecksumOffloadResult>,
    ) -> Result<(), DeviceSendFrameError> {
        Self::push_bounded(
            self.capacity,
            &mut self.queues.tx,
            TxFrame::Ethernet(device.downgrade(), frame.into_inner()),
        )
        .map_err(|_| DeviceSendFrameError::NoBuffers)
    }
    fn send_ip_packet(
        &mut self,
        _device: &PureIpDeviceId<Self>,
        packet: Buf<Vec<u8>>,
        version: IpVersion,
        _ctx: Option<&mut ()>,
        _csum: Option<ChecksumOffloadResult>,
    ) -> Result<(), DeviceSendFrameError> {
        Self::push_bounded(
            self.capacity,
            &mut self.queues.tx,
            TxFrame::PureIp(version, packet.into_inner()),
        )
        .map_err(|_| DeviceSendFrameError::NoBuffers)
    }
}
impl TcpSocketDestructionContext for NativeBindingsCtx {
    fn defer_tcp_socket_destruction<I, S>(&self, result: RemoveResourceResultWithContext<S, Self>)
    where
        I: Ip,
        S: SocketDiagnosticsSeed<Output = TcpSocketDiagnostics<I, NativeInstant>> + Send + 'static,
    {
        drop(result)
    }
}

/// An opaque UDP socket owned by one [`Runtime`].
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct UdpSocketHandle(u64);

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct TcpSocketHandle(u64);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TcpShutdown {
    Send,
    Receive,
    SendAndReceive,
}

/// Errors at the deliberately small native runtime boundary.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RuntimeError {
    InvalidCapacity,
    InvalidMac,
    InvalidMtu,
    InvalidAddress,
    AddressInUse,
    SocketLimit,
    UnknownSocket,
    InvalidState,
    WouldBlock,
    SendFailed,
    PayloadTooLarge,
    InvalidLease,
}

type NativeUdpV4 = UdpSocketId<Ipv4, WeakDeviceId<NativeBindingsCtx>, NativeBindingsCtx>;
type NativeUdpV6 = UdpSocketId<Ipv6, WeakDeviceId<NativeBindingsCtx>, NativeBindingsCtx>;
type NativeTcpV4 = TcpSocketId<Ipv4, WeakDeviceId<NativeBindingsCtx>, NativeBindingsCtx>;
type NativeTcpV6 = TcpSocketId<Ipv6, WeakDeviceId<NativeBindingsCtx>, NativeBindingsCtx>;
struct RuntimeTcpSocket {
    id: NativeTcpV4,
    buffers: NativeTcpBuffers,
}
struct RuntimeTcpSocketV6 {
    id: NativeTcpV6,
    buffers: NativeTcpBuffers,
}

/// Single-owner facade over one Netstack3 core and one Ethernet interface.
///
/// All externally visible queues are bounded by `queue_capacity`. The runtime
/// has no worker threads: its owner moves frames and drains timers explicitly.
pub struct Runtime {
    // External strong IDs must be dropped before core's primary resources.
    udp: HashMap<UdpSocketHandle, NativeUdpV4>,
    udp_v6: HashMap<UdpSocketHandle, NativeUdpV6>,
    tcp: HashMap<TcpSocketHandle, RuntimeTcpSocket>,
    tcp_v6: HashMap<TcpSocketHandle, RuntimeTcpSocketV6>,
    dhcp_socket: SocketId<NativeBindingsCtx>,
    device: EthernetDeviceId<NativeBindingsCtx>,
    ipv4_address: Option<AddrSubnet<Ipv4Addr>>,
    ipv6_address: Option<AddrSubnet<Ipv6Addr>>,
    dns_servers: [Option<std::net::Ipv4Addr>; 2],
    next_socket: u64,
    stack: StackState<NativeBindingsCtx>,
    bindings: NativeBindingsCtx,
}

impl Runtime {
    /// Creates and IPv4-enables one Ethernet interface with explicit identity.
    ///
    /// IPv6 is enabled when an IPv6 address is first applied, preserving the
    /// IPv4-only runtime's frame behavior.
    pub fn new(
        queue_capacity: usize,
        entropy: impl IntoIterator<Item = u8>,
        interface_id: NonZeroU64,
        mac: [u8; 6],
        mtu: u32,
    ) -> Result<Self, RuntimeError> {
        let mac = UnicastAddr::new(Mac::new(mac)).ok_or(RuntimeError::InvalidMac)?;
        if queue_capacity == 0 {
            return Err(RuntimeError::InvalidCapacity);
        }
        if mtu > 1500 {
            return Err(RuntimeError::InvalidMtu);
        }
        let max_frame_size =
            MaxEthernetFrameSize::from_mtu(Mtu::new(mtu)).ok_or(RuntimeError::InvalidMtu)?;
        let mut bindings = NativeBindingsCtx::new(queue_capacity, entropy);
        let stack = bindings.build_stack();
        let device = stack
            .api(&mut bindings)
            .device::<EthernetLinkDevice>()
            .add_device(
                NativeDeviceIdentifier(interface_id),
                EthernetCreationProperties {
                    mac,
                    max_frame_size,
                    tx_offload_spec: netstack3_base::ChecksumOffloadSpec::none(),
                },
                RawMetric(0),
                NativeDeviceState,
                netstack3_device::queue::BufVecU8Allocator::default(),
            );
        let device_id = device.clone().into();
        stack
            .api(&mut bindings)
            .device_ip::<Ipv4>()
            .update_configuration(
                &device_id,
                Ipv4DeviceConfigurationUpdate {
                    ip_config: IpDeviceConfigurationUpdate {
                        ip_enabled: Some(true),
                        ..Default::default()
                    },
                    ..Default::default()
                },
            )
            .expect("new Ethernet device accepts IPv4 enablement");
        stack
            .api(&mut bindings)
            .transmit_queue::<EthernetLinkDevice>()
            .set_configuration(&device, TransmitQueueConfiguration::Fifo);
        let dhcp_socket = stack
            .api(&mut bindings)
            .device_socket()
            .create(Mutex::new(VecDeque::<(
                WeakDeviceId<NativeBindingsCtx>,
                Vec<u8>,
            )>::new()));
        stack
            .api(&mut bindings)
            .device_socket()
            .set_device_and_protocol(
                &dhcp_socket,
                TargetDevice::SpecificDevice(&device_id),
                Protocol::Specific(NonZeroU16::new(0x0800).unwrap()),
            );
        Ok(Self {
            udp: HashMap::new(),
            udp_v6: HashMap::new(),
            tcp: HashMap::new(),
            tcp_v6: HashMap::new(),
            dhcp_socket,
            device,
            ipv4_address: None,
            ipv6_address: None,
            dns_servers: [None, None],
            next_socket: 0,
            stack,
            bindings,
        })
    }

    /// Replaces the IPv4 address and the complete main routing table.
    pub fn apply_ipv4(
        &mut self,
        address: [u8; 4],
        prefix: u8,
        default_gateway: Option<[u8; 4]>,
    ) -> Result<(), RuntimeError> {
        let address = AddrSubnet::new(Ipv4Addr::new(address), prefix)
            .map_err(|_| RuntimeError::InvalidAddress)?;
        let gateway = default_gateway
            .map(|a| SpecifiedAddr::new(Ipv4Addr::new(a)).ok_or(RuntimeError::InvalidAddress))
            .transpose()?;
        self.revoke_ipv4();
        self.stack
            .api(&mut self.bindings)
            .device_ip::<Ipv4>()
            .add_ip_addr_subnet(&self.device.clone().into(), address)
            .map_err(|_| RuntimeError::AddressInUse)?;

        let metric = AddableMetric::ExplicitMetric(RawMetric(0));
        let mut generation = Generation::initial();
        let mut routes = vec![
            AddableEntry::without_gateway(address.subnet(), self.device.clone().into(), metric)
                .resolve_metric(RawMetric(0))
                .with_generation(generation),
        ];
        if let Some(gateway) = gateway {
            generation = generation.next();
            routes.push(
                AddableEntry::with_gateway(
                    Subnet::new(Ipv4Addr::new([0, 0, 0, 0]), 0).unwrap(),
                    self.device.clone().into(),
                    gateway,
                    metric,
                )
                .resolve_metric(RawMetric(0))
                .with_generation(generation),
            );
        }
        let mut api = self.stack.api(&mut self.bindings).routes::<Ipv4>();
        let table = api.main_table_id();
        api.set_routes(&table, routes);
        self.ipv4_address = Some(address);
        Ok(())
    }

    /// Removes the configured IPv4 address and all IPv4 routes.
    pub fn revoke_ipv4(&mut self) {
        let mut api = self.stack.api(&mut self.bindings).routes::<Ipv4>();
        let table = api.main_table_id();
        api.set_routes(&table, Vec::new());
        if let Some(address) = self.ipv4_address.take() {
            let _ = self
                .stack
                .api(&mut self.bindings)
                .device_ip::<Ipv4>()
                .del_ip_addr(&self.device.clone().into(), address.addr());
        }
        self.dns_servers = [None, None];
    }

    /// Replaces the IPv6 address and the complete IPv6 main routing table.
    pub fn apply_ipv6(
        &mut self,
        address: [u8; 16],
        prefix: u8,
        default_gateway: Option<[u8; 16]>,
    ) -> Result<(), RuntimeError> {
        let address = AddrSubnet::new(Ipv6Addr::from_bytes(address), prefix)
            .map_err(|_| RuntimeError::InvalidAddress)?;
        let gateway = default_gateway
            .map(|a| {
                SpecifiedAddr::new(Ipv6Addr::from_bytes(a)).ok_or(RuntimeError::InvalidAddress)
            })
            .transpose()?;
        self.revoke_ipv6();
        self.stack
            .api(&mut self.bindings)
            .device_ip::<Ipv6>()
            .update_configuration(
                &self.device.clone().into(),
                Ipv6DeviceConfigurationUpdate {
                    max_router_solicitations: Some(None),
                    route_discovery_config: RouteDiscoveryConfigurationUpdate {
                        allow_default_route: Some(false),
                    },
                    ip_config: IpDeviceConfigurationUpdate {
                        ip_enabled: Some(true),
                        ..Default::default()
                    },
                    ..Default::default()
                },
            )
            .map_err(|_| RuntimeError::InvalidState)?;
        self.stack
            .api(&mut self.bindings)
            .device_ip::<Ipv6>()
            .add_ip_addr_subnet(&self.device.clone().into(), address)
            .map_err(|_| RuntimeError::AddressInUse)?;

        let metric = AddableMetric::ExplicitMetric(RawMetric(0));
        let mut generation = Generation::initial();
        let mut routes = vec![
            AddableEntry::without_gateway(address.subnet(), self.device.clone().into(), metric)
                .resolve_metric(RawMetric(0))
                .with_generation(generation),
        ];
        if let Some(gateway) = gateway {
            generation = generation.next();
            routes.push(
                AddableEntry::with_gateway(
                    Subnet::new(Ipv6Addr::from_bytes([0; 16]), 0).unwrap(),
                    self.device.clone().into(),
                    gateway,
                    metric,
                )
                .resolve_metric(RawMetric(0))
                .with_generation(generation),
            );
        }
        let mut api = self.stack.api(&mut self.bindings).routes::<Ipv6>();
        let table = api.main_table_id();
        api.set_routes(&table, routes);
        self.ipv6_address = Some(address);
        Ok(())
    }

    /// Removes the configured IPv6 address and all IPv6 routes.
    pub fn revoke_ipv6(&mut self) {
        let mut api = self.stack.api(&mut self.bindings).routes::<Ipv6>();
        let table = api.main_table_id();
        api.set_routes(&table, Vec::new());
        if let Some(address) = self.ipv6_address.take() {
            let _ = self
                .stack
                .api(&mut self.bindings)
                .device_ip::<Ipv6>()
                .del_ip_addr(&self.device.clone().into(), address.addr());
        }
    }

    /// Applies all data-plane parts of an accepted DHCP lease together.
    pub fn apply_dhcp_lease(&mut self, lease: DhcpOffer) -> Result<(), RuntimeError> {
        let prefix = lease.prefix_len().ok_or(RuntimeError::InvalidLease)?;
        self.apply_ipv4(
            lease.address.octets(),
            prefix,
            lease.gateway.map(|address| address.octets()),
        )?;
        self.dns_servers = lease.dns_servers;
        Ok(())
    }

    pub fn set_dns_servers(&mut self, servers: [Option<std::net::Ipv4Addr>; 2]) { self.dns_servers = servers; }

    pub fn dns_servers(&self) -> [Option<std::net::Ipv4Addr>; 2] {
        self.dns_servers
    }

    /// Sends an upstream DHCP core AF_PACKET payload through the private device socket.
    pub fn dhcp_packet_send(&mut self, packet: &[u8]) -> Result<(), RuntimeError> {
        if packet.len() > 1500 {
            return Err(RuntimeError::PayloadTooLarge);
        }
        self.stack
            .api(&mut self.bindings)
            .device_socket()
            .send_frame::<_, EthernetLinkDevice>(
                &self.dhcp_socket,
                DeviceSocketMetadata {
                    device_id: self.device.clone(),
                    metadata: Some(EthernetHeaderParams {
                        dest_addr: Mac::BROADCAST,
                        protocol: EtherType::Ipv4,
                    }),
                },
                Buf::new(packet.to_vec(), ..),
            )
            .map_err(|_| RuntimeError::SendFailed)
    }

    /// Takes one full IPv4 packet for the upstream DHCP core AF_PACKET adapter.
    pub fn dhcp_packet_receive(&mut self) -> Option<Vec<u8>> {
        while let Some((_, frame)) = self.dhcp_socket.socket_state().lock().unwrap().pop_front() {
            let Some(packet) = frame.get(14..) else { continue };
            if packet.len() >= 20 && packet[0] >> 4 == 4 {
                return Some(packet.to_vec());
            }
        }
        None
    }

    /// Sends a pre-lease DHCP datagram through core's private device socket.
    pub fn dhcp_send(&mut self, payload: &[u8]) -> Result<(), RuntimeError> {
        if payload.len() > MAX_CONTROL_DATAGRAM_LEN {
            return Err(RuntimeError::PayloadTooLarge);
        }
        let src = Ipv4Addr::new([0, 0, 0, 0]);
        let dst = Ipv4Addr::new([255, 255, 255, 255]);
        let body = Buf::new(payload.to_vec(), ..)
            .wrap_in(UdpPacketBuilder::new(
                src,
                dst,
                NonZeroU16::new(68),
                NonZeroU16::new(67).unwrap(),
            ))
            .wrap_in(Ipv4PacketBuilder::new(
                src,
                dst,
                64,
                Ipv4Proto::Proto(IpProto::Udp),
            ));
        self.stack
            .api(&mut self.bindings)
            .device_socket()
            .send_frame::<_, EthernetLinkDevice>(
                &self.dhcp_socket,
                DeviceSocketMetadata {
                    device_id: self.device.clone(),
                    metadata: Some(EthernetHeaderParams {
                        dest_addr: Mac::new([0xff; 6]),
                        protocol: EtherType::Ipv4,
                    }),
                },
                body,
            )
            .map_err(|_| RuntimeError::SendFailed)
    }

    /// Takes one DHCP server datagram received by the private device socket.
    pub fn dhcp_receive(&mut self) -> Option<Vec<u8>> {
        while let Some((_, frame)) = self.dhcp_socket.socket_state().lock().unwrap().pop_front() {
            let Some(ip) = frame.get(14..) else {
                continue;
            };
            if ip.len() < 28 || ip[0] >> 4 != 4 || ip[9] != 17 {
                continue;
            }
            let header_len = usize::from(ip[0] & 0x0f) * 4;
            let total_len = usize::from(u16::from_be_bytes([ip[2], ip[3]]));
            let fragment = u16::from_be_bytes([ip[6], ip[7]]);
            if header_len < 20
                || total_len > ip.len()
                || total_len < header_len + 8
                || fragment & 0x3fff != 0
            {
                continue;
            }
            let udp = &ip[header_len..total_len];
            if udp[..4] != [0, 67, 0, 68] {
                continue;
            }
            let udp_len = usize::from(u16::from_be_bytes([udp[4], udp[5]]));
            if !(8..=udp.len()).contains(&udp_len) {
                continue;
            }
            let payload = &udp[8..udp_len];
            if payload.len() <= MAX_CONTROL_DATAGRAM_LEN {
                return Some(payload.to_vec());
            }
        }
        None
    }

    /// Delivers one owned Ethernet frame into core.
    pub fn receive_frame(&mut self, frame: EthernetFrame) {
        self.stack
            .api(&mut self.bindings)
            .device::<EthernetLinkDevice>()
            .receive_frame(
                RecvEthernetFrameMeta {
                    device_id: self.device.clone(),
                    parsing_context: netstack3_base::NetworkParsingContext::default(),
                },
                Buf::new(frame.into_vec(), ..),
            );
    }

    fn service_tx(&mut self, budget: usize) {
        if budget == 0 || self.bindings.queues.tx.len() >= self.bindings.capacity {
            return;
        }
        let available = (self.bindings.capacity - self.bindings.queues.tx.len()).min(budget);
        let _ = self
            .stack
            .api(&mut self.bindings)
            .transmit_queue::<EthernetLinkDevice>()
            .transmit_queued_frames(&self.device, BatchSize::new_saturating(available), &mut ());
    }

    /// Takes one outbound frame, servicing core's TX queue as capacity opens.
    pub fn take_tx(&mut self) -> Option<EthernetFrame> {
        self.service_tx(1);
        let frame = match self.bindings.take_tx()? {
            TxFrame::Ethernet(_, bytes) => EthernetFrame::try_from(bytes).ok(),
            TxFrame::PureIp(_, _) => None,
        };
        self.service_tx(1);
        frame
    }

    pub fn set_now(&mut self, now: NativeInstant) {
        self.bindings.set_now(now);
    }

    pub fn dispatch_due(&mut self, budget: usize) -> usize {
        self.bindings.dispatch_due(&self.stack, budget)
    }

    fn socket_count(&self) -> usize {
        self.udp.len() + self.udp_v6.len() + self.tcp.len() + self.tcp_v6.len()
    }

    pub fn udp_socket(&mut self) -> Result<UdpSocketHandle, RuntimeError> {
        if self.socket_count() >= self.bindings.capacity {
            return Err(RuntimeError::SocketLimit);
        }
        let id = self.stack.api(&mut self.bindings).udp::<Ipv4>().create();
        let handle = UdpSocketHandle(self.next_socket);
        self.next_socket = self
            .next_socket
            .checked_add(1)
            .expect("UDP handle space exhausted");
        assert!(self.udp.insert(handle, id).is_none());
        Ok(handle)
    }

    pub fn udp_bind(
        &mut self,
        handle: UdpSocketHandle,
        address: Option<[u8; 4]>,
        port: NonZeroU16,
    ) -> Result<(), RuntimeError> {
        let id = self.udp.get(&handle).ok_or(RuntimeError::UnknownSocket)?;
        let address = address
            .map(|a| SpecifiedAddr::new(Ipv4Addr::new(a)).ok_or(RuntimeError::InvalidAddress))
            .transpose()?
            .map(|a| ZonedAddr::Unzoned(a).into());
        self.stack
            .api(&mut self.bindings)
            .udp::<Ipv4>()
            .listen(id, address, Some(port))
            .map_err(|_| RuntimeError::AddressInUse)
    }

    pub fn udp_send_to(
        &mut self,
        handle: UdpSocketHandle,
        remote_address: [u8; 4],
        remote_port: NonZeroU16,
        payload: &[u8],
    ) -> Result<(), RuntimeError> {
        if payload.len() > 1472 {
            return Err(RuntimeError::PayloadTooLarge);
        }
        let id = self.udp.get(&handle).ok_or(RuntimeError::UnknownSocket)?;
        let address = SpecifiedAddr::new(Ipv4Addr::new(remote_address))
            .ok_or(RuntimeError::InvalidAddress)?;
        self.stack
            .api(&mut self.bindings)
            .udp::<Ipv4>()
            .send_to(
                id,
                Some(ZonedAddr::Unzoned(address).into()),
                UdpRemotePort::Set(remote_port),
                Buf::new(payload.to_vec(), ..),
            )
            .map_err(|_| RuntimeError::SendFailed)
    }

    pub fn udp_connect(
        &mut self,
        handle: UdpSocketHandle,
        remote_address: [u8; 4],
        remote_port: NonZeroU16,
    ) -> Result<(), RuntimeError> {
        let id = self.udp.get(&handle).ok_or(RuntimeError::UnknownSocket)?;
        let address = SpecifiedAddr::new(Ipv4Addr::new(remote_address))
            .ok_or(RuntimeError::InvalidAddress)?;
        self.stack
            .api(&mut self.bindings)
            .udp::<Ipv4>()
            .connect(
                id,
                Some(ZonedAddr::Unzoned(address)),
                UdpRemotePort::Set(remote_port),
            )
            .map_err(|_| RuntimeError::InvalidState)
    }

    pub fn udp_send(
        &mut self,
        handle: UdpSocketHandle,
        payload: &[u8],
    ) -> Result<(), RuntimeError> {
        if payload.len() > 1472 {
            return Err(RuntimeError::PayloadTooLarge);
        }
        let id = self.udp.get(&handle).ok_or(RuntimeError::UnknownSocket)?;
        self.stack
            .api(&mut self.bindings)
            .udp::<Ipv4>()
            .send(id, Buf::new(payload.to_vec(), ..))
            .map_err(|_| RuntimeError::SendFailed)
    }

    pub fn udp_receive(
        &mut self,
        handle: UdpSocketHandle,
    ) -> Result<Option<Vec<u8>>, RuntimeError> {
        let id = self.udp.get(&handle).ok_or(RuntimeError::UnknownSocket)?;
        Ok(self.bindings.take_udp(id))
    }

    pub fn udp_close(&mut self, handle: UdpSocketHandle) -> Result<(), RuntimeError> {
        let id = self.udp.remove(&handle).ok_or(RuntimeError::UnknownSocket)?;
        drop(self.stack.api(&mut self.bindings).udp::<Ipv4>().close(id));
        Ok(())
    }

    pub fn udp_socket_ipv6(&mut self) -> Result<UdpSocketHandle, RuntimeError> {
        if self.socket_count() >= self.bindings.capacity {
            return Err(RuntimeError::SocketLimit);
        }
        let id = self.stack.api(&mut self.bindings).udp::<Ipv6>().create();
        let handle = UdpSocketHandle(self.next_socket);
        self.next_socket = self
            .next_socket
            .checked_add(1)
            .expect("UDP handle space exhausted");
        assert!(self.udp_v6.insert(handle, id).is_none());
        Ok(handle)
    }

    pub fn udp_bind_ipv6(
        &mut self,
        handle: UdpSocketHandle,
        address: Option<[u8; 16]>,
        port: NonZeroU16,
    ) -> Result<(), RuntimeError> {
        let id = self
            .udp_v6
            .get(&handle)
            .ok_or(RuntimeError::UnknownSocket)?;
        let address = address
            .map(|a| {
                SpecifiedAddr::new(Ipv6Addr::from_bytes(a)).ok_or(RuntimeError::InvalidAddress)
            })
            .transpose()?
            .map(|a| ZonedAddr::Unzoned(a).into());
        self.stack
            .api(&mut self.bindings)
            .udp::<Ipv6>()
            .listen(id, address, Some(port))
            .map_err(|_| RuntimeError::AddressInUse)
    }

    pub fn udp_send_to_ipv6(
        &mut self,
        handle: UdpSocketHandle,
        remote_address: [u8; 16],
        remote_port: NonZeroU16,
        payload: &[u8],
    ) -> Result<(), RuntimeError> {
        // 1500-byte Ethernet MTU minus the fixed IPv6 and UDP headers.
        if payload.len() > 1452 {
            return Err(RuntimeError::PayloadTooLarge);
        }
        let id = self
            .udp_v6
            .get(&handle)
            .ok_or(RuntimeError::UnknownSocket)?;
        let address = SpecifiedAddr::new(Ipv6Addr::from_bytes(remote_address))
            .ok_or(RuntimeError::InvalidAddress)?;
        self.stack
            .api(&mut self.bindings)
            .udp::<Ipv6>()
            .send_to(
                id,
                Some(ZonedAddr::Unzoned(address).into()),
                UdpRemotePort::Set(remote_port),
                Buf::new(payload.to_vec(), ..),
            )
            .map_err(|_| RuntimeError::SendFailed)
    }

    pub fn udp_receive_ipv6(
        &mut self,
        handle: UdpSocketHandle,
    ) -> Result<Option<Vec<u8>>, RuntimeError> {
        let id = self
            .udp_v6
            .get(&handle)
            .ok_or(RuntimeError::UnknownSocket)?;
        Ok(self.bindings.take_udp(id))
    }

    pub fn udp_close_ipv6(&mut self, handle: UdpSocketHandle) -> Result<(), RuntimeError> {
        let id = self
            .udp_v6
            .remove(&handle)
            .ok_or(RuntimeError::UnknownSocket)?;
        drop(self.stack.api(&mut self.bindings).udp::<Ipv6>().close(id));
        Ok(())
    }

    pub fn tcp_socket(&mut self) -> Result<TcpSocketHandle, RuntimeError> {
        if self.socket_count() >= self.bindings.capacity {
            return Err(RuntimeError::SocketLimit);
        }
        let socket_data = NativeTcpSocketData::buffers(BufferSizes {
            send: self.bindings.tcp_settings.send_buffer.default().get(),
            receive: self.bindings.tcp_settings.receive_buffer.default().get(),
        });
        let buffers = socket_data.client_buffers().unwrap();
        let id = self
            .stack
            .api(&mut self.bindings)
            .tcp::<Ipv4>()
            .create(socket_data);
        let handle = TcpSocketHandle(self.next_socket);
        self.next_socket = self
            .next_socket
            .checked_add(1)
            .expect("socket handle space exhausted");
        assert!(
            self.tcp
                .insert(handle, RuntimeTcpSocket { id, buffers })
                .is_none()
        );
        Ok(handle)
    }

    pub fn tcp_bind(
        &mut self,
        handle: TcpSocketHandle,
        address: Option<[u8; 4]>,
        port: NonZeroU16,
    ) -> Result<(), RuntimeError> {
        let id = &self.tcp.get(&handle).ok_or(RuntimeError::UnknownSocket)?.id;
        let address = address
            .map(|a| SpecifiedAddr::new(Ipv4Addr::new(a)).ok_or(RuntimeError::InvalidAddress))
            .transpose()?
            .map(ZonedAddr::Unzoned);
        self.stack
            .api(&mut self.bindings)
            .tcp::<Ipv4>()
            .bind(id, address, Some(port))
            .map_err(|_| RuntimeError::AddressInUse)
    }

    pub fn tcp_listen(
        &mut self,
        handle: TcpSocketHandle,
        backlog: NonZeroUsize,
    ) -> Result<(), RuntimeError> {
        if backlog.get() > self.bindings.capacity {
            return Err(RuntimeError::SocketLimit);
        }
        let id = &self.tcp.get(&handle).ok_or(RuntimeError::UnknownSocket)?.id;
        self.stack
            .api(&mut self.bindings)
            .tcp::<Ipv4>()
            .listen(id, backlog)
            .map_err(|_| RuntimeError::InvalidState)
    }

    pub fn tcp_connect(
        &mut self,
        handle: TcpSocketHandle,
        remote_address: [u8; 4],
        remote_port: NonZeroU16,
    ) -> Result<(), RuntimeError> {
        let id = &self.tcp.get(&handle).ok_or(RuntimeError::UnknownSocket)?.id;
        let address = SpecifiedAddr::new(Ipv4Addr::new(remote_address))
            .ok_or(RuntimeError::InvalidAddress)?;
        self.stack
            .api(&mut self.bindings)
            .tcp::<Ipv4>()
            .connect(id, Some(ZonedAddr::Unzoned(address)), remote_port)
            .map_err(|_| RuntimeError::SendFailed)
    }

    pub fn tcp_accept(
        &mut self,
        listener: TcpSocketHandle,
    ) -> Result<TcpSocketHandle, RuntimeError> {
        if self.socket_count() >= self.bindings.capacity {
            return Err(RuntimeError::SocketLimit);
        }
        let listener = &self
            .tcp
            .get(&listener)
            .ok_or(RuntimeError::UnknownSocket)?
            .id;
        let (id, _remote, buffers) = self
            .stack
            .api(&mut self.bindings)
            .tcp::<Ipv4>()
            .accept(listener)
            .map_err(|_| RuntimeError::WouldBlock)?;
        let handle = TcpSocketHandle(self.next_socket);
        self.next_socket = self
            .next_socket
            .checked_add(1)
            .expect("socket handle space exhausted");
        assert!(
            self.tcp
                .insert(handle, RuntimeTcpSocket { id, buffers })
                .is_none()
        );
        Ok(handle)
    }

    pub fn tcp_write(
        &mut self,
        handle: TcpSocketHandle,
        payload: &[u8],
    ) -> Result<usize, RuntimeError> {
        let socket = self.tcp.get(&handle).ok_or(RuntimeError::UnknownSocket)?;
        let written = socket.buffers.write(payload);
        if written != 0 {
            self.stack
                .api(&mut self.bindings)
                .tcp::<Ipv4>()
                .do_send(&socket.id);
        }
        Ok(written)
    }

    pub fn tcp_read(
        &mut self,
        handle: TcpSocketHandle,
        out: &mut [u8],
    ) -> Result<usize, RuntimeError> {
        let socket = self.tcp.get(&handle).ok_or(RuntimeError::UnknownSocket)?;
        let read = socket.buffers.read(out);
        if read != 0 {
            self.stack
                .api(&mut self.bindings)
                .tcp::<Ipv4>()
                .on_receive_buffer_read(&socket.id);
        }
        Ok(read)
    }

    pub fn tcp_shutdown(
        &mut self,
        handle: TcpSocketHandle,
        how: TcpShutdown,
    ) -> Result<(), RuntimeError> {
        let id = &self.tcp.get(&handle).ok_or(RuntimeError::UnknownSocket)?.id;
        let how = match how {
            TcpShutdown::Send => ShutdownType::Send,
            TcpShutdown::Receive => ShutdownType::Receive,
            TcpShutdown::SendAndReceive => ShutdownType::SendAndReceive,
        };
        self.stack
            .api(&mut self.bindings)
            .tcp::<Ipv4>()
            .shutdown(id, how)
            .map(|_| ())
            .map_err(|_| RuntimeError::InvalidState)
    }

    pub fn tcp_close(&mut self, handle: TcpSocketHandle) -> Result<(), RuntimeError> {
        let socket = self
            .tcp
            .remove(&handle)
            .ok_or(RuntimeError::UnknownSocket)?;
        self.stack
            .api(&mut self.bindings)
            .tcp::<Ipv4>()
            .close(socket.id);
        Ok(())
    }

    pub fn tcp_socket_ipv6(&mut self) -> Result<TcpSocketHandle, RuntimeError> {
        if self.socket_count() >= self.bindings.capacity {
            return Err(RuntimeError::SocketLimit);
        }
        let socket_data = NativeTcpSocketData::buffers(BufferSizes {
            send: self.bindings.tcp_settings.send_buffer.default().get(),
            receive: self.bindings.tcp_settings.receive_buffer.default().get(),
        });
        let buffers = socket_data.client_buffers().unwrap();
        let id = self
            .stack
            .api(&mut self.bindings)
            .tcp::<Ipv6>()
            .create(socket_data);
        let handle = TcpSocketHandle(self.next_socket);
        self.next_socket = self
            .next_socket
            .checked_add(1)
            .expect("socket handle space exhausted");
        assert!(
            self.tcp_v6
                .insert(handle, RuntimeTcpSocketV6 { id, buffers })
                .is_none()
        );
        Ok(handle)
    }

    pub fn tcp_bind_ipv6(
        &mut self,
        handle: TcpSocketHandle,
        address: Option<[u8; 16]>,
        port: NonZeroU16,
    ) -> Result<(), RuntimeError> {
        let id = &self
            .tcp_v6
            .get(&handle)
            .ok_or(RuntimeError::UnknownSocket)?
            .id;
        let address = address
            .map(|a| {
                SpecifiedAddr::new(Ipv6Addr::from_bytes(a)).ok_or(RuntimeError::InvalidAddress)
            })
            .transpose()?
            .map(ZonedAddr::Unzoned);
        self.stack
            .api(&mut self.bindings)
            .tcp::<Ipv6>()
            .bind(id, address, Some(port))
            .map_err(|_| RuntimeError::AddressInUse)
    }

    pub fn tcp_listen_ipv6(
        &mut self,
        handle: TcpSocketHandle,
        backlog: NonZeroUsize,
    ) -> Result<(), RuntimeError> {
        if backlog.get() > self.bindings.capacity {
            return Err(RuntimeError::SocketLimit);
        }
        let id = &self
            .tcp_v6
            .get(&handle)
            .ok_or(RuntimeError::UnknownSocket)?
            .id;
        self.stack
            .api(&mut self.bindings)
            .tcp::<Ipv6>()
            .listen(id, backlog)
            .map_err(|_| RuntimeError::InvalidState)
    }

    pub fn tcp_connect_ipv6(
        &mut self,
        handle: TcpSocketHandle,
        remote_address: [u8; 16],
        remote_port: NonZeroU16,
    ) -> Result<(), RuntimeError> {
        let id = &self
            .tcp_v6
            .get(&handle)
            .ok_or(RuntimeError::UnknownSocket)?
            .id;
        let address = SpecifiedAddr::new(Ipv6Addr::from_bytes(remote_address))
            .ok_or(RuntimeError::InvalidAddress)?;
        self.stack
            .api(&mut self.bindings)
            .tcp::<Ipv6>()
            .connect(id, Some(ZonedAddr::Unzoned(address)), remote_port)
            .map_err(|_| RuntimeError::SendFailed)
    }

    pub fn tcp_accept_ipv6(
        &mut self,
        listener: TcpSocketHandle,
    ) -> Result<TcpSocketHandle, RuntimeError> {
        if self.socket_count() >= self.bindings.capacity {
            return Err(RuntimeError::SocketLimit);
        }
        let listener = &self
            .tcp_v6
            .get(&listener)
            .ok_or(RuntimeError::UnknownSocket)?
            .id;
        let (id, _remote, buffers) = self
            .stack
            .api(&mut self.bindings)
            .tcp::<Ipv6>()
            .accept(listener)
            .map_err(|_| RuntimeError::WouldBlock)?;
        let handle = TcpSocketHandle(self.next_socket);
        self.next_socket = self
            .next_socket
            .checked_add(1)
            .expect("socket handle space exhausted");
        assert!(
            self.tcp_v6
                .insert(handle, RuntimeTcpSocketV6 { id, buffers })
                .is_none()
        );
        Ok(handle)
    }

    pub fn tcp_write_ipv6(
        &mut self,
        handle: TcpSocketHandle,
        payload: &[u8],
    ) -> Result<usize, RuntimeError> {
        let socket = self
            .tcp_v6
            .get(&handle)
            .ok_or(RuntimeError::UnknownSocket)?;
        let written = socket.buffers.write(payload);
        if written != 0 {
            self.stack
                .api(&mut self.bindings)
                .tcp::<Ipv6>()
                .do_send(&socket.id);
        }
        Ok(written)
    }

    pub fn tcp_read_ipv6(
        &mut self,
        handle: TcpSocketHandle,
        out: &mut [u8],
    ) -> Result<usize, RuntimeError> {
        let socket = self
            .tcp_v6
            .get(&handle)
            .ok_or(RuntimeError::UnknownSocket)?;
        let read = socket.buffers.read(out);
        if read != 0 {
            self.stack
                .api(&mut self.bindings)
                .tcp::<Ipv6>()
                .on_receive_buffer_read(&socket.id);
        }
        Ok(read)
    }

    pub fn tcp_shutdown_ipv6(
        &mut self,
        handle: TcpSocketHandle,
        how: TcpShutdown,
    ) -> Result<(), RuntimeError> {
        let id = &self
            .tcp_v6
            .get(&handle)
            .ok_or(RuntimeError::UnknownSocket)?
            .id;
        let how = match how {
            TcpShutdown::Send => ShutdownType::Send,
            TcpShutdown::Receive => ShutdownType::Receive,
            TcpShutdown::SendAndReceive => ShutdownType::SendAndReceive,
        };
        self.stack
            .api(&mut self.bindings)
            .tcp::<Ipv6>()
            .shutdown(id, how)
            .map(|_| ())
            .map_err(|_| RuntimeError::InvalidState)
    }

    pub fn tcp_close_ipv6(&mut self, handle: TcpSocketHandle) -> Result<(), RuntimeError> {
        let socket = self
            .tcp_v6
            .remove(&handle)
            .ok_or(RuntimeError::UnknownSocket)?;
        self.stack
            .api(&mut self.bindings)
            .tcp::<Ipv6>()
            .close(socket.id);
        Ok(())
    }
}

impl StackEthernetEndpoint for Runtime {
    fn receive_frame(&mut self, frame: EthernetFrame) -> Result<(), EthernetFrame> {
        Runtime::receive_frame(self, frame);
        Ok(())
    }

    fn take_transmit(&mut self) -> Option<EthernetFrame> {
        self.take_tx()
    }
}

impl NetworkServiceEndpoint for Runtime {
    fn poll_at(&mut self, now: Duration, budget: usize) -> usize {
        let nanos = u64::try_from(now.as_nanos()).unwrap_or(u64::MAX);
        self.set_now(NativeInstant::from_nanos(nanos));
        self.dispatch_due(budget)
    }

    fn on_device_event(&mut self, event: EthernetDeviceEvent) {
        if event == EthernetDeviceEvent::TransmitReady {
            self.service_tx(1);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use edge_dhcp::{MessageType as DhcpMessageType, Options, Packet};
    use hickory_proto::op::{Message, OpCode};
    use hickory_proto::rr::rdata::A;
    use hickory_proto::rr::{Name, RData, Record, RecordType};
    use netstack3_base::socket::SocketWritableListener as _;
    use netstack3_port_spike::control_plane::{Dhcpv4Client, DnsCodec};
    use packet::Serializer as _;

    #[derive(Clone, Copy)]
    struct FixedRandom(u32);
    impl rand_core_06::RngCore for FixedRandom {
        fn next_u32(&mut self) -> u32 {
            self.0
        }
        fn next_u64(&mut self) -> u64 {
            u64::from(self.next_u32())
        }
        fn fill_bytes(&mut self, dest: &mut [u8]) {
            for chunk in dest.chunks_mut(4) {
                let bytes = self.next_u32().to_ne_bytes();
                chunk.copy_from_slice(&bytes[..chunk.len()]);
            }
        }
        fn try_fill_bytes(&mut self, dest: &mut [u8]) -> Result<(), rand_core_06::Error> {
            self.fill_bytes(dest);
            Ok(())
        }
    }

    fn encode_dhcp(packet: &Packet<'_>) -> Vec<u8> {
        let mut bytes = [0; MAX_CONTROL_DATAGRAM_LEN];
        packet.encode(&mut bytes).unwrap().to_vec()
    }

    fn dhcp_server_frame(payload: &[u8]) -> EthernetFrame {
        let server_ip = Ipv4Addr::new([192, 0, 2, 1]);
        let broadcast = Ipv4Addr::new([255, 255, 255, 255]);
        let bytes = Buf::new(payload.to_vec(), ..)
            .wrap_in(UdpPacketBuilder::new(
                server_ip,
                broadcast,
                NonZeroU16::new(67),
                NonZeroU16::new(68).unwrap(),
            ))
            .wrap_in(Ipv4PacketBuilder::new(
                server_ip,
                broadcast,
                64,
                Ipv4Proto::Proto(IpProto::Udp),
            ))
            .wrap_in(packet_formats::ethernet::EthernetFrameBuilder::new(
                Mac::new([0x02, 0, 0, 0, 0, 1]),
                Mac::new([0xff; 6]),
                EtherType::Ipv4,
                packet_formats::ethernet::ETHERNET_MIN_BODY_LEN_NO_TAG,
            ))
            .serialize_vec_outer(&mut netstack3_base::NetworkSerializationContext::default())
            .unwrap()
            .unwrap_b()
            .into_inner();
        EthernetFrame::try_from(bytes).unwrap()
    }

    #[test]
    fn injected_clock_entropy_and_readiness_are_deterministic_and_bounded() {
        let mut ctx = NativeBindingsCtx::new(1, 0u8..16);
        assert_eq!(ctx.now(), NativeInstant::ZERO);
        ctx.advance(Duration::from_nanos(7));
        assert_eq!(ctx.now().as_nanos(), 7);
        assert_eq!(ctx.rng().next_u32(), u32::from_le_bytes([0, 1, 2, 3]));

        let mut writable = NativeWritable::new(1);
        writable.on_writable_changed(false);
        writable.on_writable_changed(true);
        assert_eq!(writable.take(), Some(ReadinessEvent::UdpWritable(false)));
        assert_eq!(writable.take(), None);
    }

    #[test]
    fn tcp_buffers_transfer_application_bytes() {
        let sizes = BufferSizes {
            send: 16,
            receive: 16,
        };
        let app = NativeTcpBuffers::new(sizes);
        assert_eq!(app.write(b"hello"), 5);
        let mut core_send = app.send.clone();
        assert_eq!(core_send.peek_with(0, |p| p.0), b"hello");
        core_send.mark_read(5);

        let mut core_receive = app.receive.clone();
        assert_eq!(core_receive.write_at(0, &&b"world"[..]), 5);
        core_receive.make_readable(5, false);
        let mut out = [0; 8];
        assert_eq!(app.read(&mut out), 5);
        assert_eq!(&out[..5], b"world");
    }

    #[test]
    fn externally_visible_queues_enforce_capacity() {
        let mut ctx = NativeBindingsCtx::new(1, []);
        assert!(
            NativeBindingsCtx::push_bounded(ctx.capacity, &mut ctx.queues.events, "one".into())
                .is_ok()
        );
        assert!(
            NativeBindingsCtx::push_bounded(ctx.capacity, &mut ctx.queues.events, "two".into())
                .is_err()
        );
        assert_eq!(ctx.take_event().as_deref(), Some("one"));
    }

    fn runtime(id: u64, mac: [u8; 6], address: [u8; 4]) -> Runtime {
        let entropy = (0u8..=255).cycle().take(8192);
        let mut runtime = Runtime::new(2, entropy, NonZeroU64::new(id).unwrap(), mac, 1500)
            .expect("valid interface");
        runtime
            .apply_ipv4(address, 24, Some([192, 0, 2, 254]))
            .expect("valid static configuration");
        runtime
    }

    const CLIENT_V6: [u8; 16] = [0x20, 0x01, 0x0d, 0xb8, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 1];
    const SERVER_V6: [u8; 16] = [0x20, 0x01, 0x0d, 0xb8, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 2];

    fn runtime_ipv6(
        id: u64,
        mac: [u8; 6],
        address: [u8; 16],
        prefix: u8,
        default_gateway: Option<[u8; 16]>,
    ) -> Runtime {
        let entropy = (0u8..=255).cycle().take(8192);
        let mut runtime = Runtime::new(4, entropy, NonZeroU64::new(id).unwrap(), mac, 1500)
            .expect("valid interface");
        runtime
            .apply_ipv6(address, prefix, default_gateway)
            .expect("valid static IPv6 configuration");
        runtime
    }

    fn exchange(a: &mut Runtime, b: &mut Runtime) -> usize {
        let mut count = 0;
        while let Some(frame) = a.take_tx() {
            b.receive_frame(frame);
            count += 1;
        }
        while let Some(frame) = b.take_tx() {
            a.receive_frame(frame);
            count += 1;
        }
        count
    }

    fn finish_ipv6_dad(a: &mut Runtime, b: &mut Runtime) {
        for _ in 0..16 {
            if exchange(a, b) == 0 {
                break;
            }
        }
        let after_dad = NativeInstant::from_nanos(2_000_000_000);
        a.set_now(after_dad);
        b.set_now(after_dad);
        let _ = a.dispatch_due(64);
        let _ = b.dispatch_due(64);
        for _ in 0..16 {
            if exchange(a, b) == 0 {
                return;
            }
        }
        panic!("IPv6 DAD traffic did not quiesce");
    }

    fn udp_payload(frame: &EthernetFrame) -> &[u8] {
        let bytes = frame.as_bytes();
        let ip_header_len = usize::from(bytes[14] & 0x0f) * 4;
        &bytes[14 + ip_header_len + 8..]
    }

    #[test]
    fn native_runtime_bootstraps_dhcp_then_resolves_dns_over_udp() {
        let entropy = (0u8..=255).cycle().take(8192);
        let mut client = Runtime::new(
            4,
            entropy,
            NonZeroU64::new(10).unwrap(),
            [0x02, 0, 0, 0, 0, 10],
            1500,
        )
        .unwrap();
        let mut dhcp = Dhcpv4Client::new(FixedRandom(0x1234_5678), [0x02, 0, 0, 0, 0, 10]);
        let (discover_tx, discover) = dhcp.discover(0).unwrap();
        client.dhcp_send(discover.as_bytes()).unwrap();
        let discover_frame = client.take_tx().unwrap();
        assert_eq!(&discover_frame.as_bytes()[0..6], &[0xff; 6]);
        let discover = Packet::decode(udp_payload(&discover_frame)).unwrap();

        let gateways = [std::net::Ipv4Addr::new(192, 0, 2, 1)];
        let dns = [std::net::Ipv4Addr::new(192, 0, 2, 1)];
        let mut options = Options::buf();
        let offer_options = discover.options.reply(
            DhcpMessageType::Offer,
            gateways[0],
            3600,
            &gateways,
            Some(std::net::Ipv4Addr::new(255, 255, 255, 0)),
            &dns,
            None,
            &mut options,
        );
        let offer = discover.new_reply(Some(std::net::Ipv4Addr::new(192, 0, 2, 10)), offer_options);
        client.receive_frame(dhcp_server_frame(&encode_dhcp(&offer)));
        let offer = dhcp
            .accept_offer(discover_tx, &client.dhcp_receive().unwrap())
            .unwrap()
            .unwrap();

        let (request_tx, request) = dhcp.request(1, offer).unwrap();
        client.dhcp_send(request.as_bytes()).unwrap();
        let request = client.take_tx().unwrap();
        let request = Packet::decode(udp_payload(&request)).unwrap();
        let mut options = Options::buf();
        let ack_options = request.options.reply(
            DhcpMessageType::Ack,
            gateways[0],
            3600,
            &gateways,
            Some(std::net::Ipv4Addr::new(255, 255, 255, 0)),
            &dns,
            None,
            &mut options,
        );
        let ack = request.new_reply(Some(offer.address), ack_options);
        client.receive_frame(dhcp_server_frame(&encode_dhcp(&ack)));
        let lease = dhcp
            .accept_ack(request_tx, &client.dhcp_receive().unwrap())
            .unwrap()
            .unwrap();
        client.apply_dhcp_lease(lease).unwrap();
        assert_eq!(client.dns_servers()[0], Some(dns[0]));

        let mut server = runtime(11, [0x02, 0, 0, 0, 0, 1], [192, 0, 2, 1]);
        let client_socket = client.udp_socket().unwrap();
        let server_socket = server.udp_socket().unwrap();
        client
            .udp_bind(
                client_socket,
                Some([192, 0, 2, 10]),
                NonZeroU16::new(53000).unwrap(),
            )
            .unwrap();
        server
            .udp_bind(
                server_socket,
                Some([192, 0, 2, 1]),
                NonZeroU16::new(53).unwrap(),
            )
            .unwrap();
        let query = DnsCodec::query(7, "native.test.", RecordType::A).unwrap();
        client
            .udp_send_to(
                client_socket,
                [192, 0, 2, 1],
                NonZeroU16::new(53).unwrap(),
                query.datagram().as_bytes(),
            )
            .unwrap();
        for _ in 0..16 {
            if exchange(&mut client, &mut server) == 0 {
                break;
            }
        }
        let request = server.udp_receive(server_socket).unwrap().unwrap();
        let request = Message::from_vec(&request).unwrap();
        let mut response = Message::response(7, OpCode::Query);
        response.add_query(request.queries[0].clone());
        response.answers.push(Record::from_rdata(
            Name::from_ascii("native.test.").unwrap(),
            60,
            RData::A(A(std::net::Ipv4Addr::new(192, 0, 2, 1))),
        ));
        server
            .udp_send_to(
                server_socket,
                [192, 0, 2, 10],
                NonZeroU16::new(53000).unwrap(),
                &response.to_vec().unwrap(),
            )
            .unwrap();
        for _ in 0..16 {
            if exchange(&mut client, &mut server) == 0 {
                break;
            }
        }
        let response = client.udp_receive(client_socket).unwrap().unwrap();
        assert_eq!(
            DnsCodec::response(&query, &response).unwrap(),
            [std::net::IpAddr::V4(dns[0])]
        );
    }

    #[test]
    fn two_native_runtimes_resolve_arp_and_exchange_udp() {
        let mut client = runtime(1, [0x02, 0, 0, 0, 0, 1], [192, 0, 2, 1]);
        let mut server = runtime(2, [0x02, 0, 0, 0, 0, 2], [192, 0, 2, 2]);
        let client_socket = client.udp_socket().unwrap();
        let server_socket = server.udp_socket().unwrap();
        client
            .udp_bind(
                client_socket,
                Some([192, 0, 2, 1]),
                NonZeroU16::new(10001).unwrap(),
            )
            .unwrap();
        server
            .udp_bind(
                server_socket,
                Some([192, 0, 2, 2]),
                NonZeroU16::new(10002).unwrap(),
            )
            .unwrap();

        client
            .udp_send_to(
                client_socket,
                [192, 0, 2, 2],
                NonZeroU16::new(10002).unwrap(),
                b"bounded native UDP",
            )
            .unwrap();
        let arp = client.take_tx().expect("send starts address resolution");
        assert_eq!(&arp.as_bytes()[12..14], &[0x08, 0x06]);
        server.receive_frame(arp);

        for _ in 0..16 {
            if exchange(&mut client, &mut server) == 0 {
                break;
            }
        }
        assert_eq!(
            server.udp_receive(server_socket).unwrap().as_deref(),
            Some(&b"bounded native UDP"[..])
        );

        server
            .udp_send_to(
                server_socket,
                [192, 0, 2, 1],
                NonZeroU16::new(10001).unwrap(),
                b"reply",
            )
            .unwrap();
        for _ in 0..16 {
            if exchange(&mut client, &mut server) == 0 {
                break;
            }
        }
        assert_eq!(
            client.udp_receive(client_socket).unwrap().as_deref(),
            Some(&b"reply"[..])
        );

        assert!(client.udp_socket().is_ok());
        assert_eq!(client.udp_socket(), Err(RuntimeError::SocketLimit));
    }
    #[test]
    fn two_native_runtimes_resolve_arp_and_exchange_tcp() {
        let mut client = runtime(11, [0x02, 0, 0, 0, 1, 1], [192, 0, 2, 11]);
        let mut server = runtime(12, [0x02, 0, 0, 0, 1, 2], [192, 0, 2, 12]);
        let port = NonZeroU16::new(4040).unwrap();
        let listener = server.tcp_socket().unwrap();
        server
            .tcp_bind(listener, Some([192, 0, 2, 12]), port)
            .unwrap();
        server
            .tcp_listen(listener, NonZeroUsize::new(1).unwrap())
            .unwrap();

        let connection = client.tcp_socket().unwrap();
        client
            .tcp_connect(connection, [192, 0, 2, 12], port)
            .unwrap();
        let arp = client.take_tx().expect("connect starts address resolution");
        assert_eq!(&arp.as_bytes()[12..14], &[0x08, 0x06]);
        server.receive_frame(arp);
        for _ in 0..32 {
            if exchange(&mut client, &mut server) == 0 {
                break;
            }
        }
        let accepted = server.tcp_accept(listener).expect("handshake completed");
        assert_eq!(server.tcp_socket(), Err(RuntimeError::SocketLimit));

        assert_eq!(
            client.tcp_write(connection, b"native TCP request").unwrap(),
            18
        );
        for _ in 0..32 {
            if exchange(&mut client, &mut server) == 0 {
                break;
            }
        }
        let mut request = [0; 32];
        let n = server.tcp_read(accepted, &mut request).unwrap();
        assert_eq!(&request[..n], b"native TCP request");

        assert_eq!(server.tcp_write(accepted, b"native TCP reply").unwrap(), 16);
        for _ in 0..32 {
            if exchange(&mut client, &mut server) == 0 {
                break;
            }
        }
        let mut reply = [0; 32];
        let n = client.tcp_read(connection, &mut reply).unwrap();
        assert_eq!(&reply[..n], b"native TCP reply");

        client.tcp_shutdown(connection, TcpShutdown::Send).unwrap();
        let mut close_frames = 0;
        for _ in 0..32 {
            close_frames += exchange(&mut client, &mut server);
        }
        assert!(close_frames > 0, "shutdown emitted and acknowledged FIN");
        server.tcp_shutdown(accepted, TcpShutdown::Send).unwrap();
        for _ in 0..32 {
            exchange(&mut client, &mut server);
        }
        client.tcp_close(connection).unwrap();
        server.tcp_close(accepted).unwrap();
        server.tcp_close(listener).unwrap();
        assert_eq!(
            client.tcp_close(connection),
            Err(RuntimeError::UnknownSocket)
        );
        let replacement = client.tcp_socket().unwrap();
        assert_ne!(replacement, connection);
        client.tcp_close(replacement).unwrap();
    }

    #[test]
    fn two_native_runtimes_resolve_ndp_and_exchange_ipv6_udp() {
        let mut client = runtime_ipv6(21, [0x02, 0, 0, 0, 2, 1], CLIENT_V6, 128, Some(SERVER_V6));
        let mut server = runtime_ipv6(22, [0x02, 0, 0, 0, 2, 2], SERVER_V6, 64, None);
        finish_ipv6_dad(&mut client, &mut server);

        let client_socket = client.udp_socket_ipv6().unwrap();
        let server_socket = server.udp_socket_ipv6().unwrap();
        client
            .udp_bind_ipv6(
                client_socket,
                Some(CLIENT_V6),
                NonZeroU16::new(20001).unwrap(),
            )
            .unwrap();
        server
            .udp_bind_ipv6(
                server_socket,
                Some(SERVER_V6),
                NonZeroU16::new(20002).unwrap(),
            )
            .unwrap();
        client
            .udp_send_to_ipv6(
                client_socket,
                SERVER_V6,
                NonZeroU16::new(20002).unwrap(),
                b"bounded native IPv6 UDP",
            )
            .unwrap();

        let solicitation = client.take_tx().expect("send starts NDP resolution");
        assert_eq!(&solicitation.as_bytes()[12..14], &[0x86, 0xdd]);
        assert_eq!(solicitation.as_bytes()[54], 135);
        server.receive_frame(solicitation);
        for _ in 0..32 {
            if exchange(&mut client, &mut server) == 0 {
                break;
            }
        }
        assert_eq!(
            server.udp_receive_ipv6(server_socket).unwrap().as_deref(),
            Some(&b"bounded native IPv6 UDP"[..])
        );

        // IPv4 and IPv6 sockets consume the same four-socket capability.
        assert!(client.udp_socket().is_ok());
        assert!(client.tcp_socket_ipv6().is_ok());
        assert!(client.tcp_socket().is_ok());
        assert_eq!(client.udp_socket_ipv6(), Err(RuntimeError::SocketLimit));

        client.revoke_ipv6();
        assert_eq!(
            client.udp_send_to_ipv6(
                client_socket,
                SERVER_V6,
                NonZeroU16::new(20002).unwrap(),
                b"no route",
            ),
            Err(RuntimeError::SendFailed)
        );
    }

    #[test]
    fn two_native_runtimes_exchange_and_close_ipv6_tcp() {
        let mut client = runtime_ipv6(31, [0x02, 0, 0, 0, 3, 1], CLIENT_V6, 128, Some(SERVER_V6));
        let mut server = runtime_ipv6(32, [0x02, 0, 0, 0, 3, 2], SERVER_V6, 64, None);
        finish_ipv6_dad(&mut client, &mut server);
        let port = NonZeroU16::new(6060).unwrap();
        let listener = server.tcp_socket_ipv6().unwrap();
        server
            .tcp_bind_ipv6(listener, Some(SERVER_V6), port)
            .unwrap();
        server
            .tcp_listen_ipv6(listener, NonZeroUsize::new(1).unwrap())
            .unwrap();

        let connection = client.tcp_socket_ipv6().unwrap();
        client
            .tcp_connect_ipv6(connection, SERVER_V6, port)
            .unwrap();
        let solicitation = client.take_tx().expect("connect starts NDP resolution");
        assert_eq!(&solicitation.as_bytes()[12..14], &[0x86, 0xdd]);
        assert_eq!(solicitation.as_bytes()[54], 135);
        server.receive_frame(solicitation);
        for _ in 0..32 {
            if exchange(&mut client, &mut server) == 0 {
                break;
            }
        }
        let accepted = server
            .tcp_accept_ipv6(listener)
            .expect("IPv6 handshake completed");

        assert_eq!(
            client
                .tcp_write_ipv6(connection, b"native IPv6 TCP request")
                .unwrap(),
            23
        );
        for _ in 0..32 {
            if exchange(&mut client, &mut server) == 0 {
                break;
            }
        }
        let mut request = [0; 32];
        let n = server.tcp_read_ipv6(accepted, &mut request).unwrap();
        assert_eq!(&request[..n], b"native IPv6 TCP request");

        assert_eq!(
            server
                .tcp_write_ipv6(accepted, b"native IPv6 TCP reply")
                .unwrap(),
            21
        );
        for _ in 0..32 {
            if exchange(&mut client, &mut server) == 0 {
                break;
            }
        }
        let mut reply = [0; 32];
        let n = client.tcp_read_ipv6(connection, &mut reply).unwrap();
        assert_eq!(&reply[..n], b"native IPv6 TCP reply");

        client
            .tcp_shutdown_ipv6(connection, TcpShutdown::Send)
            .unwrap();
        for _ in 0..32 {
            exchange(&mut client, &mut server);
        }
        client.tcp_close_ipv6(connection).unwrap();
        server.tcp_close_ipv6(accepted).unwrap();
        server.tcp_close_ipv6(listener).unwrap();
        assert_eq!(
            client.tcp_close_ipv6(connection),
            Err(RuntimeError::UnknownSocket)
        );
    }
}
