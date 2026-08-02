//! Portable Ethernet and control-plane boundaries for a Netstack3 binding.
//!
//! The crate itself does not link Netstack3. The companion pinned-source Cargo
//! overlay executes upstream core against these authority-free contracts.
pub mod control_plane;

use std::collections::VecDeque;
use std::error::Error;
use std::fmt;

/// Ethernet II header length, excluding an FCS.
pub const MIN_FRAME_LEN: usize = 14;

/// Ethernet II header plus the project's initial 1500-byte IP MTU, excluding
/// an FCS and VLAN tags.
pub const MAX_FRAME_LEN: usize = 1514;

/// An owned Ethernet frame whose allocation cannot expose bytes beyond the
/// validated frame length.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct EthernetFrame(Box<[u8]>);

impl EthernetFrame {
    /// Copies a frame into boundary-owned storage after validating its size.
    pub fn copy_from_slice(bytes: &[u8]) -> Result<Self, FrameSizeError> {
        Self::try_from(bytes.to_vec())
    }

    pub fn as_bytes(&self) -> &[u8] {
        &self.0
    }

    pub fn into_vec(self) -> Vec<u8> {
        self.0.into_vec()
    }
}

impl TryFrom<Vec<u8>> for EthernetFrame {
    type Error = FrameSizeError;

    fn try_from(bytes: Vec<u8>) -> Result<Self, Self::Error> {
        match bytes.len() {
            len if len < MIN_FRAME_LEN => Err(FrameSizeError::TooShort { len }),
            len if len > MAX_FRAME_LEN => Err(FrameSizeError::TooLong { len }),
            _ => Ok(Self(bytes.into_boxed_slice())),
        }
    }
}

impl AsRef<[u8]> for EthernetFrame {
    fn as_ref(&self) -> &[u8] {
        self.as_bytes()
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FrameSizeError {
    TooShort { len: usize },
    TooLong { len: usize },
}

impl fmt::Display for FrameSizeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::TooShort { len } => write!(
                f,
                "Ethernet frame length {len} is below the {MIN_FRAME_LEN}-byte minimum"
            ),
            Self::TooLong { len } => write!(
                f,
                "Ethernet frame length {len} exceeds the {MAX_FRAME_LEN}-byte maximum"
            ),
        }
    }
}

impl Error for FrameSizeError {}

/// The complete data-plane authority presented to a same-process network
/// stack. Time, randomness, configuration, and socket readiness belong in
/// separate explicit bindings capabilities.
pub trait EthernetDevice {
    /// Removes the oldest frame supplied by the device, if any.
    fn receive(&mut self) -> Option<EthernetFrame>;

    /// Transfers an outbound frame to the device, returning ownership when
    /// its bounded queue cannot accept the frame.
    fn transmit(&mut self, frame: EthernetFrame) -> Result<(), EthernetFrame>;
}

/// Edge-triggered notifications needed by a single-owner stack event loop.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum EthernetDeviceEvent {
    LinkStateChanged(bool),
    ReceiveReady,
    TransmitReady,
}

/// Companion notification channel for [`EthernetDevice`]. A real Wi-Fi
/// adapter can wake its owner when this source becomes readable; no OS handle
/// is exposed to the stack itself.
pub trait EthernetEventSource {
    fn take_event(&mut self) -> Option<EthernetDeviceEvent>;
}

/// The frame-only side of a userspace network stack. Implementations enqueue
/// ingress and dequeue egress without acquiring device, clock, or filesystem
/// authority.
pub trait StackEthernetEndpoint {
    /// Accepts one inbound frame, returning ownership under backpressure.
    fn receive_frame(&mut self, frame: EthernetFrame) -> Result<(), EthernetFrame>;

    /// Removes one frame emitted by the stack, if available.
    fn take_transmit(&mut self) -> Option<EthernetFrame>;
}

/// Result of one bounded device/stack pump operation.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct PumpReport {
    pub received: usize,
    pub transmitted: usize,
    pub ingress_blocked: bool,
    pub egress_blocked: bool,
}

/// Lossless, bounded adapter between a stack endpoint and an Ethernet device.
///
/// At most one frame is retained when either side reports backpressure. This
/// makes retries explicit and prevents an event loop from draining an
/// unbounded number of frames in one turn.
pub struct EthernetRunner<S, D> {
    stack: S,
    device: D,
    pending_ingress: Option<EthernetFrame>,
    pending_egress: Option<EthernetFrame>,
}

impl<S, D> EthernetRunner<S, D>
where
    S: StackEthernetEndpoint,
    D: EthernetDevice,
{
    pub const fn new(stack: S, device: D) -> Self {
        Self {
            stack,
            device,
            pending_ingress: None,
            pending_egress: None,
        }
    }

    /// Performs at most one ingress and one egress transfer.
    pub fn pump(&mut self) -> PumpReport {
        let mut report = PumpReport::default();

        let ingress = self
            .pending_ingress
            .take()
            .or_else(|| self.device.receive());
        if let Some(frame) = ingress {
            match self.stack.receive_frame(frame) {
                Ok(()) => report.received = 1,
                Err(frame) => {
                    self.pending_ingress = Some(frame);
                    report.ingress_blocked = true;
                }
            }
        }

        let egress = self
            .pending_egress
            .take()
            .or_else(|| self.stack.take_transmit());
        if let Some(frame) = egress {
            match self.device.transmit(frame) {
                Ok(()) => report.transmitted = 1,
                Err(frame) => {
                    self.pending_egress = Some(frame);
                    report.egress_blocked = true;
                }
            }
        }

        report
    }

    pub fn stack(&self) -> &S {
        &self.stack
    }

    pub fn stack_mut(&mut self) -> &mut S {
        &mut self.stack
    }

    pub fn device(&self) -> &D {
        &self.device
    }

    pub fn device_mut(&mut self) -> &mut D {
        &mut self.device
    }

    pub fn into_parts(self) -> (S, D) {
        (self.stack, self.device)
    }
}

/// Deterministic, bounded fake for contract tests. It has no hardware,
/// filesystem, network, clock, or random-number access.
#[derive(Debug)]
pub struct FakeEthernetDevice {
    queue_capacity: usize,
    ingress: VecDeque<EthernetFrame>,
    transmitted: VecDeque<EthernetFrame>,
    link_event: Option<bool>,
    receive_ready: bool,
    transmit_ready: bool,
}

impl FakeEthernetDevice {
    pub fn new(queue_capacity: usize) -> Self {
        Self {
            queue_capacity,
            ingress: VecDeque::with_capacity(queue_capacity),
            transmitted: VecDeque::with_capacity(queue_capacity),
            link_event: Some(true),
            receive_ready: false,
            transmit_ready: false,
        }
    }

    /// Supplies a frame as if it had arrived from the eventual Wi-Fi Ethernet
    /// boundary. Ownership is returned when the ingress queue is full.
    pub fn inject(&mut self, frame: EthernetFrame) -> Result<(), EthernetFrame> {
        if self.ingress.len() == self.queue_capacity {
            return Err(frame);
        }
        self.ingress.push_back(frame);
        self.receive_ready = true;
        Ok(())
    }

    /// Removes the oldest frame emitted by the stack.
    pub fn take_transmitted(&mut self) -> Option<EthernetFrame> {
        let was_full = self.transmitted.len() == self.queue_capacity;
        let frame = self.transmitted.pop_front();
        self.transmit_ready |= was_full && frame.is_some();
        frame
    }

    pub fn pending_ingress(&self) -> usize {
        self.ingress.len()
    }

    pub fn pending_transmitted(&self) -> usize {
        self.transmitted.len()
    }
}

impl EthernetDevice for FakeEthernetDevice {
    fn receive(&mut self) -> Option<EthernetFrame> {
        let frame = self.ingress.pop_front();
        self.receive_ready = !self.ingress.is_empty();
        frame
    }

    fn transmit(&mut self, frame: EthernetFrame) -> Result<(), EthernetFrame> {
        if self.transmitted.len() == self.queue_capacity {
            return Err(frame);
        }
        self.transmitted.push_back(frame);
        Ok(())
    }
}

impl EthernetEventSource for FakeEthernetDevice {
    fn take_event(&mut self) -> Option<EthernetDeviceEvent> {
        if let Some(up) = self.link_event.take() {
            return Some(EthernetDeviceEvent::LinkStateChanged(up));
        }
        if core::mem::take(&mut self.receive_ready) {
            self.receive_ready = !self.ingress.is_empty();
            return Some(EthernetDeviceEvent::ReceiveReady);
        }
        if core::mem::take(&mut self.transmit_ready) {
            return Some(EthernetDeviceEvent::TransmitReady);
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Default)]
    struct TestEndpoint {
        accept_ingress: bool,
        received: VecDeque<EthernetFrame>,
        outbound: VecDeque<EthernetFrame>,
    }

    impl StackEthernetEndpoint for TestEndpoint {
        fn receive_frame(&mut self, frame: EthernetFrame) -> Result<(), EthernetFrame> {
            if !self.accept_ingress {
                return Err(frame);
            }
            self.received.push_back(frame);
            Ok(())
        }

        fn take_transmit(&mut self) -> Option<EthernetFrame> {
            self.outbound.pop_front()
        }
    }

    fn frame(marker: u8) -> EthernetFrame {
        let mut bytes = vec![0; 60];
        bytes[12..14].copy_from_slice(&[0x08, marker]);
        EthernetFrame::try_from(bytes).unwrap()
    }

    #[test]
    fn owns_and_bounds_frames() {
        let mut source = vec![0x5a; 60];
        let owned = EthernetFrame::copy_from_slice(&source).unwrap();
        source[0] = 0;
        assert_eq!(owned.as_bytes()[0], 0x5a);

        assert_eq!(
            EthernetFrame::try_from(vec![0; MIN_FRAME_LEN - 1]),
            Err(FrameSizeError::TooShort {
                len: MIN_FRAME_LEN - 1
            })
        );
        assert_eq!(
            EthernetFrame::try_from(vec![0; MAX_FRAME_LEN + 1]),
            Err(FrameSizeError::TooLong {
                len: MAX_FRAME_LEN + 1
            })
        );
    }

    #[test]
    fn fake_device_is_fifo_and_lossless() {
        let mut device = FakeEthernetDevice::new(2);
        let arp = frame(0x06);
        let ipv4 = frame(0x00);
        device.inject(arp.clone()).unwrap();
        device.inject(ipv4.clone()).unwrap();

        let first = device.receive().unwrap();
        let second = device.receive().unwrap();
        assert_eq!(first, arp);
        assert_eq!(second, ipv4);

        device.transmit(first).unwrap();
        device.transmit(second).unwrap();
        assert_eq!(device.take_transmitted(), Some(arp));
        assert_eq!(device.take_transmitted(), Some(ipv4));
    }

    #[test]
    fn queues_return_ownership_at_the_bound() {
        let mut device = FakeEthernetDevice::new(1);
        let first = frame(0x06);
        let rejected = frame(0x00);
        device.inject(first).unwrap();
        assert_eq!(device.inject(rejected.clone()), Err(rejected.clone()));

        device.transmit(rejected.clone()).unwrap();
        assert_eq!(device.transmit(frame(0x06)), Err(frame(0x06)));
        assert_eq!(device.take_transmitted(), Some(rejected));
    }

    #[test]
    fn fake_device_reports_link_rx_and_returned_tx_credit() {
        let mut device = FakeEthernetDevice::new(1);
        assert_eq!(
            device.take_event(),
            Some(EthernetDeviceEvent::LinkStateChanged(true))
        );
        device.inject(frame(0x06)).unwrap();
        assert_eq!(device.take_event(), Some(EthernetDeviceEvent::ReceiveReady));
        assert!(device.receive().is_some());

        device.transmit(frame(0x00)).unwrap();
        assert!(device.take_transmitted().is_some());
        assert_eq!(
            device.take_event(),
            Some(EthernetDeviceEvent::TransmitReady)
        );
        assert_eq!(device.take_event(), None);
    }

    #[test]
    fn runner_retries_both_directions_without_loss() {
        let ingress = frame(0x06);
        let outbound = frame(0x00);
        let mut endpoint = TestEndpoint::default();
        endpoint.outbound.push_back(outbound.clone());
        let mut device = FakeEthernetDevice::new(1);
        device.inject(ingress.clone()).unwrap();
        device.transmit(frame(0x06)).unwrap();
        let mut runner = EthernetRunner::new(endpoint, device);

        assert_eq!(
            runner.pump(),
            PumpReport {
                ingress_blocked: true,
                egress_blocked: true,
                ..Default::default()
            }
        );

        runner.stack_mut().accept_ingress = true;
        assert!(runner.device_mut().take_transmitted().is_some());
        assert_eq!(
            runner.pump(),
            PumpReport {
                received: 1,
                transmitted: 1,
                ..Default::default()
            }
        );
        assert_eq!(runner.stack().received.front(), Some(&ingress));
        assert_eq!(runner.device_mut().take_transmitted(), Some(outbound));
    }
}
