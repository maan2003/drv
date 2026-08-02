//! Small, authority-free native bindings for the pinned Netstack3 core.
//!
//! This is intentionally a synchronous embedding. The owner injects entropy
//! and time and explicitly drains timers, frames, events, and socket queues.

#![recursion_limit = "256"]

use std::collections::{BTreeMap, HashMap, VecDeque};
use std::convert::Infallible;
use std::fmt::{self, Debug, Display};
use std::num::NonZeroU64;
use std::ops::Deref;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use net_types::UnicastAddr;
use net_types::ip::{Ip, IpVersion};
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
    DeviceId, EthernetDeviceId, EthernetWeakDeviceId, LoopbackDeviceId, PureIpDeviceId,
    WeakDeviceId,
};
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
use netstack3_tcp::{Buffer, BufferLimits, IntoBuffers, ReceiveBuffer, SendBuffer};
use netstack3_tcp::{
    BufferSizes, ListenerNotifier, TcpBindingsTypes, TcpSettings, TcpSocketDestructionContext,
    TcpSocketDiagnostics,
};
use netstack3_udp::{
    ReceiveUdpError, UdpBindingsTypes, UdpPacketMeta, UdpReceiveBindingsContext, UdpSettings,
    UdpSocketId,
};
use packet::{Buf, BufferMut, FragmentedByteSlice, InnerPacketBuilder};
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

#[cfg(test)]
mod tests {
    use super::*;
    use netstack3_base::socket::SocketWritableListener as _;

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
}
