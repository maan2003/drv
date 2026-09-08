// SPDX-License-Identifier: GPL-2.0-only

//! Bounded Ethernet-II handoff between the pinned client MLME and Netstack3.
//!
//! The client MLME, not this module, owns 802.11 encapsulation/decapsulation,
//! controlled-port policy, and selection of protected data frames.

use netstack3_port_spike::{EthernetDeviceEvent, EthernetFrame, FrameSizeError};
#[cfg(any(test, feature = "conformance"))]
use netstack3_port_spike::{EthernetDevice, EthernetEventSource};
use std::collections::VecDeque;
use std::fmt;
use std::io::ErrorKind;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};
use std::sync::{Arc, Mutex};

pub const SOFTMAC_ETHERNET_MTU: u16 = 1500;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct EthernetPortProperties {
    pub mac_address: [u8; 6],
    pub mtu: u16,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum EthernetPortConfigError {
    ZeroQueueCapacity,
    InvalidMacAddress,
    SocketPair,
}

impl fmt::Display for EthernetPortConfigError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "invalid SoftMAC Ethernet port configuration: {self:?}")
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

/// The entire contract between the associated driver and Netstack3.
///
/// Implementations carry Ethernet II frames in both directions. Link policy,
/// association state, controlled-port state, and every other control verb stay
/// on the driver side of this interface.
pub trait EthernetFrameSeam {
    type Error;

    /// Attempt to send exactly one whole frame without blocking.
    fn try_send_frame(&mut self, frame: EthernetFrame) -> Result<(), (Self::Error, EthernetFrame)>;
    /// Attempt to receive exactly one whole frame without blocking.
    fn try_receive_frame(&mut self) -> Result<Option<EthernetFrame>, Self::Error>;
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum SeqpacketFrameError {
    Closed,
    Backpressure,
    InvalidFrame(usize),
}

struct PortLifecycleState {
    properties: Option<EthernetPortProperties>,
    link_up: bool,
    events: VecDeque<EthernetDeviceEvent>,
}

struct SeqpacketFrameEndpoint {
    fd: Option<OwnedFd>,
    receive_notified: bool,
    transmit_blocked: bool,
}

impl SeqpacketFrameEndpoint {
    fn discard_frames(&mut self) {
        let mut bytes = [0u8; 1515];
        while unsafe { recv(self.raw_fd(), bytes.as_mut_ptr(), bytes.len(), MSG_DONTWAIT) } > 0 {}
        bytes.fill(0);
        self.receive_notified = false;
        self.transmit_blocked = false;
    }

    fn close(&mut self) {
        self.fd.take();
    }

    #[cfg(any(test, feature = "conformance"))]
    fn take_event(&mut self) -> Option<EthernetDeviceEvent> {
        let mut descriptor = PollFd {
            fd: self.raw_fd(),
            events: POLLIN | if self.transmit_blocked { POLLOUT } else { 0 },
            revents: 0,
        };
        if unsafe { poll(&mut descriptor, 1, 0) } <= 0 {
            return None;
        }
        if descriptor.revents & (POLLERR | POLLHUP | POLLNVAL) != 0 {
            return Some(EthernetDeviceEvent::LinkStateChanged(false));
        }
        if descriptor.revents & POLLIN != 0 && !self.receive_notified {
            self.receive_notified = true;
            return Some(EthernetDeviceEvent::ReceiveReady);
        }
        if descriptor.revents & POLLOUT != 0 && self.transmit_blocked {
            self.transmit_blocked = false;
            return Some(EthernetDeviceEvent::TransmitReady);
        }
        None
    }

    fn raw_fd(&self) -> RawFd {
        self.fd.as_ref().map_or(-1, AsRawFd::as_raw_fd)
    }

    fn take_fd(&mut self) -> OwnedFd {
        self.fd.take().expect("frame endpoint is open")
    }
}

impl Drop for SeqpacketFrameEndpoint {
    fn drop(&mut self) {
        self.close();
    }
}

impl EthernetFrameSeam for SeqpacketFrameEndpoint {
    type Error = SeqpacketFrameError;

    /// Attempt to send exactly one whole frame without blocking.
    fn try_send_frame(&mut self, frame: EthernetFrame) -> Result<(), (Self::Error, EthernetFrame)> {
        let sent = unsafe {
            send(
                self.raw_fd(),
                frame.as_bytes().as_ptr(),
                frame.as_bytes().len(),
                MSG_DONTWAIT | MSG_NOSIGNAL,
            )
        };
        if sent == frame.as_bytes().len() as isize {
            return Ok(());
        }
        let error = std::io::Error::last_os_error();
        let kind = if error.kind() == ErrorKind::WouldBlock {
            self.transmit_blocked = true;
            SeqpacketFrameError::Backpressure
        } else {
            SeqpacketFrameError::Closed
        };
        Err((kind, frame))
    }

    fn try_receive_frame(&mut self) -> Result<Option<EthernetFrame>, Self::Error> {
        let mut bytes = [0u8; 1514];
        let received = unsafe {
            recv(
                self.raw_fd(),
                bytes.as_mut_ptr(),
                bytes.len(),
                MSG_DONTWAIT | MSG_TRUNC,
            )
        };
        if received == 0 {
            return Err(SeqpacketFrameError::Closed);
        }
        if received < 0 {
            return match std::io::Error::last_os_error().kind() {
                ErrorKind::WouldBlock => {
                    self.receive_notified = false;
                    Ok(None)
                }
                _ => Err(SeqpacketFrameError::Closed),
            };
        }
        let received = received as usize;
        if received > 1514 {
            return Err(SeqpacketFrameError::InvalidFrame(received));
        }
        EthernetFrame::copy_from_slice(&bytes[..received])
            .map(Some)
            .map_err(|_| SeqpacketFrameError::InvalidFrame(received))
    }
}

#[cfg(any(test, feature = "conformance"))]
#[repr(C)]
struct PollFd {
    fd: i32,
    events: i16,
    revents: i16,
}

const AF_UNIX: i32 = 1;
const SOCK_SEQPACKET: i32 = 5;
const SOCK_NONBLOCK: i32 = 0x800;
const SOCK_CLOEXEC: i32 = 0x80000;
const MSG_DONTWAIT: i32 = 0x40;
const MSG_TRUNC: i32 = 0x20;
const MSG_NOSIGNAL: i32 = 0x4000;
#[cfg(any(test, feature = "conformance"))]
const POLLIN: i16 = 0x001;
#[cfg(any(test, feature = "conformance"))]
const POLLOUT: i16 = 0x004;
#[cfg(any(test, feature = "conformance"))]
const POLLERR: i16 = 0x008;
#[cfg(any(test, feature = "conformance"))]
const POLLHUP: i16 = 0x010;
#[cfg(any(test, feature = "conformance"))]
const POLLNVAL: i16 = 0x020;

unsafe extern "C" {
    fn socketpair(domain: i32, socket_type: i32, protocol: i32, sockets: *mut i32) -> i32;
    fn send(fd: i32, bytes: *const u8, len: usize, flags: i32) -> isize;
    fn recv(fd: i32, bytes: *mut u8, len: usize, flags: i32) -> isize;
    #[cfg(any(test, feature = "conformance"))]
    fn poll(fds: *mut PollFd, count: usize, timeout_ms: i32) -> i32;
}

/// Netstack3-facing half of the port.
pub struct HostEthernetDevice {
    seam: SeqpacketFrameEndpoint,
    lifecycle: Arc<Mutex<PortLifecycleState>>,
}

/// Driver-side endpoint retained by the host device. The controlled-port
/// gate is deliberately outside [`EthernetFrameSeam`].
pub struct DriverEthernetPort {
    seam: SeqpacketFrameEndpoint,
    lifecycle: Arc<Mutex<PortLifecycleState>>,
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
    fn pump_transmit(&mut self) -> Result<bool, EthernetTxPumpError<Self::Error>>;
    fn pump_receive(&mut self, deadline: std::time::Instant) -> Result<bool, Self::Error>;
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum EthernetTxPumpError<E> {
    Closed,
    LinkDown,
    Target(E),
}

pub fn ethernet_port(
    mac_address: [u8; 6],
    queue_capacity: usize,
) -> Result<(HostEthernetDevice, DriverEthernetPort), EthernetPortConfigError> {
    if queue_capacity == 0 {
        return Err(EthernetPortConfigError::ZeroQueueCapacity);
    }
    if mac_address == [0; 6] || mac_address[0] & 1 != 0 {
        return Err(EthernetPortConfigError::InvalidMacAddress);
    }
    let mut sockets = [-1; 2];
    if unsafe {
        socketpair(
            AF_UNIX,
            SOCK_SEQPACKET | SOCK_NONBLOCK | SOCK_CLOEXEC,
            0,
            sockets.as_mut_ptr(),
        )
    } != 0
    {
        return Err(EthernetPortConfigError::SocketPair);
    }
    let driver_fd = unsafe { OwnedFd::from_raw_fd(sockets[0]) };
    let netstack_fd = unsafe { OwnedFd::from_raw_fd(sockets[1]) };
    let lifecycle = Arc::new(Mutex::new(PortLifecycleState {
        properties: Some(EthernetPortProperties {
            mac_address,
            mtu: SOFTMAC_ETHERNET_MTU,
        }),
        link_up: false,
        events: VecDeque::with_capacity(queue_capacity.saturating_mul(2).saturating_add(1)),
    }));
    Ok((
        HostEthernetDevice {
            seam: SeqpacketFrameEndpoint {
                fd: Some(netstack_fd),
                receive_notified: false,
                transmit_blocked: false,
            },
            lifecycle: lifecycle.clone(),
        },
        DriverEthernetPort {
            seam: SeqpacketFrameEndpoint {
                fd: Some(driver_fd),
                receive_notified: false,
                transmit_blocked: false,
            },
            lifecycle,
        },
    ))
}

impl HostEthernetDevice {
    pub fn properties(&self) -> Option<EthernetPortProperties> {
        self.lifecycle.lock().unwrap().properties
    }

    pub fn into_frame_fd(mut self) -> OwnedFd {
        self.seam.take_fd()
    }
}

#[cfg(any(test, feature = "conformance"))]
impl EthernetDevice for HostEthernetDevice {
    fn receive(&mut self) -> Option<EthernetFrame> {
        let state = self.lifecycle.lock().unwrap();
        if state.properties.is_none() || !state.link_up {
            return None;
        }
        self.seam.try_receive_frame().ok().flatten()
    }

    fn transmit(&mut self, frame: EthernetFrame) -> Result<(), EthernetFrame> {
        let state = self.lifecycle.lock().unwrap();
        if state.properties.is_none() || !state.link_up {
            return Err(frame);
        }
        self.seam.try_send_frame(frame).map_err(|(_, frame)| frame)
    }
}

#[cfg(any(test, feature = "conformance"))]
impl EthernetEventSource for HostEthernetDevice {
    fn take_event(&mut self) -> Option<EthernetDeviceEvent> {
        let mut lifecycle = self.lifecycle.lock().unwrap();
        let event = lifecycle.events.pop_front();
        if event.is_some() || !lifecycle.link_up || lifecycle.properties.is_none() {
            return event;
        }
        drop(lifecycle);
        self.seam.take_event()
    }
}

impl<D: wlan_mlme::device::DeviceOps> AssociatedSoftmacTx for wlan_mlme::client::ClientMlme<D> {
    type Error = anyhow::Error;

    fn transmit_ethernet(&mut self, frame: &[u8]) -> Result<(), Self::Error> {
        wlan_mlme::MlmeImpl::handle_eth_frame_tx(self, frame, fuchsia_trace::Id::new())
    }
}

impl DriverEthernetPort {
    pub(crate) fn is_closed(&self) -> bool {
        self.seam.fd.is_none()
    }

    pub fn deliver(&mut self, bytes: &[u8]) -> Result<(), EthernetIngressError> {
        let frame =
            EthernetFrame::copy_from_slice(bytes).map_err(EthernetIngressError::InvalidFrame)?;
        let state = self.lifecycle.lock().unwrap();
        if state.properties.is_none() {
            return Err(EthernetIngressError::Closed);
        }
        if !state.link_up {
            return Err(EthernetIngressError::LinkDown);
        }
        self.seam
            .try_send_frame(frame)
            .map_err(|(error, _)| match error {
                SeqpacketFrameError::Closed => EthernetIngressError::Closed,
                SeqpacketFrameError::Backpressure => EthernetIngressError::Backpressure,
                SeqpacketFrameError::InvalidFrame(len) => {
                    EthernetIngressError::InvalidFrame(if len < 14 {
                        FrameSizeError::TooShort { len }
                    } else {
                        FrameSizeError::TooLong { len }
                    })
                }
            })
    }

    pub fn take_transmit(&mut self) -> Result<Option<EthernetFrame>, EthernetIngressError> {
        let state = self.lifecycle.lock().unwrap();
        if state.properties.is_none() {
            return Err(EthernetIngressError::Closed);
        }
        if !state.link_up {
            return Err(EthernetIngressError::LinkDown);
        }
        self.seam.try_receive_frame().map_err(|error| match error {
            SeqpacketFrameError::Closed => EthernetIngressError::Closed,
            SeqpacketFrameError::Backpressure => unreachable!(),
            SeqpacketFrameError::InvalidFrame(len) => {
                EthernetIngressError::InvalidFrame(if len < 14 {
                    FrameSizeError::TooShort { len }
                } else {
                    FrameSizeError::TooLong { len }
                })
            }
        })
    }

    pub fn set_link(&mut self, up: bool) {
        let mut state = self.lifecycle.lock().unwrap();
        if state.properties.is_none() {
            return;
        }
        let changed = state.link_up != up;
        state.link_up = up;
        if !up {
            self.seam.discard_frames();
            self.seam.close();
            state.events.retain(|event| {
                !matches!(
                    event,
                    EthernetDeviceEvent::ReceiveReady | EthernetDeviceEvent::TransmitReady
                )
            });
        }
        if changed {
            push_event(&mut state.events, EthernetDeviceEvent::LinkStateChanged(up));
        }
    }

    pub fn teardown(&mut self) {
        let mut state = self.lifecycle.lock().unwrap();
        if state.properties.is_none() {
            return;
        }
        state.events.clear();
        if state.link_up {
            push_event(
                &mut state.events,
                EthernetDeviceEvent::LinkStateChanged(false),
            );
        }
        state.link_up = false;
        state.properties = None;
        self.seam.close();
    }
}

impl Drop for DriverEthernetPort {
    fn drop(&mut self) {
        self.teardown();
    }
}

fn push_event(events: &mut VecDeque<EthernetDeviceEvent>, event: EthernetDeviceEvent) {
    if let EthernetDeviceEvent::LinkStateChanged(_) = event {
        events.retain(|queued| !matches!(queued, EthernetDeviceEvent::LinkStateChanged(_)));
    } else if events.contains(&event) {
        return;
    }
    events.push_back(event);
}

#[cfg(test)]
mod tests {
    use super::*;

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

    #[test]
    fn arp_dhcp_and_data_leave_through_softmac_tx_facade() {
        let (mut device, mut sink) = ethernet_port([2, 0, 0, 0, 0, 1], 3).unwrap();
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
            let frame = sink.take_transmit().unwrap().unwrap();
            target.transmit_ethernet(frame.as_bytes()).unwrap();
        }
        assert_eq!(sink.take_transmit(), Ok(None));
        assert_eq!(target.frames, expected.map(EthernetFrame::into_vec));
    }

    #[test]
    fn backpressure_link_lifecycle_and_teardown_are_bounded() {
        let (mut device, mut sink) = ethernet_port([2, 0, 0, 0, 0, 1], 1).unwrap();
        assert_eq!(device.properties().unwrap().mtu, 1500);
        let arp = frame([0x08, 0x06], 1);
        assert_eq!(device.transmit(arp.clone()), Err(arp.clone()));
        sink.set_link(true);
        device.transmit(arp.clone()).unwrap();
        let data = frame([0x08, 0x00], 2);
        let mut queued = 1;
        loop {
            match device.transmit(data.clone()) {
                Ok(()) => queued += 1,
                Err(frame) => {
                    assert_eq!(frame, data);
                    break;
                }
            }
        }
        assert!(queued > 1);
        let mut target = TxTarget {
            blocked: true,
            ..Default::default()
        };
        let frame = sink.take_transmit().unwrap().unwrap();
        assert_eq!(target.transmit_ethernet(frame.as_bytes()), Err(()));
        sink.teardown();
        assert_eq!(device.properties(), None);
        assert_eq!(device.receive(), None);
        assert_eq!(device.transmit(arp.clone()), Err(arp));
        assert_eq!(sink.take_transmit(), Err(EthernetIngressError::Closed));
    }

    #[test]
    fn link_down_discards_frames_and_stale_readiness_from_old_association() {
        let (mut device, mut sink) = ethernet_port([2, 0, 0, 0, 0, 1], 2).unwrap();
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
        assert_eq!(sink.take_transmit(), Err(EthernetIngressError::LinkDown));
    }

    #[test]
    fn link_down_revokes_an_already_down_generation() {
        let (_, mut sink) = ethernet_port([2, 0, 0, 0, 0, 1], 1).unwrap();
        assert!(!sink.is_closed());
        sink.set_link(false);
        assert!(sink.is_closed());
    }

    #[test]
    fn invalid_frames_and_addresses_do_not_cross_the_boundary() {
        assert!(matches!(
            ethernet_port([0; 6], 1),
            Err(EthernetPortConfigError::InvalidMacAddress)
        ));
        let (_, mut sink) = ethernet_port([2, 0, 0, 0, 0, 1], 1).unwrap();
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

    #[test]
    fn dropping_netstack_endpoint_closes_driver_peer() {
        let (device, mut driver) = ethernet_port([2, 0, 0, 0, 0, 1], 1).unwrap();
        driver.set_link(true);
        drop(device);
        assert_eq!(
            driver.deliver(frame([0x08, 0x00], 1).as_bytes()),
            Err(EthernetIngressError::Closed)
        );
    }

    #[test]
    fn dropping_driver_endpoint_tears_down_netstack_facade() {
        let (mut device, mut driver) = ethernet_port([2, 0, 0, 0, 0, 1], 1).unwrap();
        driver.set_link(true);
        drop(driver);
        assert_eq!(device.properties(), None);
        assert_eq!(
            device.take_event(),
            Some(EthernetDeviceEvent::LinkStateChanged(false))
        );
        let frame = frame([0x08, 0x00], 1);
        assert_eq!(device.transmit(frame.clone()), Err(frame));
    }
}
