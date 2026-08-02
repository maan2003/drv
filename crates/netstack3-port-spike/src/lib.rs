//! Portable Ethernet boundary scaffold for a future Netstack3 binding.
//!
//! This crate does **not** execute Netstack3 protocol code. It isolates the
//! authority-free packet/device contract that a host binding will implement.

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

/// Deterministic, bounded fake for contract tests. It has no hardware,
/// filesystem, network, clock, or random-number access.
#[derive(Debug)]
pub struct FakeEthernetDevice {
    queue_capacity: usize,
    ingress: VecDeque<EthernetFrame>,
    transmitted: VecDeque<EthernetFrame>,
}

impl FakeEthernetDevice {
    pub fn new(queue_capacity: usize) -> Self {
        Self {
            queue_capacity,
            ingress: VecDeque::with_capacity(queue_capacity),
            transmitted: VecDeque::with_capacity(queue_capacity),
        }
    }

    /// Supplies a frame as if it had arrived from the eventual Wi-Fi Ethernet
    /// boundary. Ownership is returned when the ingress queue is full.
    pub fn inject(&mut self, frame: EthernetFrame) -> Result<(), EthernetFrame> {
        if self.ingress.len() == self.queue_capacity {
            return Err(frame);
        }
        self.ingress.push_back(frame);
        Ok(())
    }

    /// Removes the oldest frame emitted by the stack.
    pub fn take_transmitted(&mut self) -> Option<EthernetFrame> {
        self.transmitted.pop_front()
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
        self.ingress.pop_front()
    }

    fn transmit(&mut self, frame: EthernetFrame) -> Result<(), EthernetFrame> {
        if self.transmitted.len() == self.queue_capacity {
            return Err(frame);
        }
        self.transmitted.push_back(frame);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
}
