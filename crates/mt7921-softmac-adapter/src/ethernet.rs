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
use std::sync::{Arc, Mutex};

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
