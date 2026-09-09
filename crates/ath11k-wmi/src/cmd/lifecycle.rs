//! Source-shaped WMI lifecycle seam used by ath11k-core.

use alloc::vec::Vec;

use super::{EncodeCommand, Init};
use crate::event::{
    EventStream, InstallKeyCompletion, PeerAssocConfirmation, PeerCreateConfirmation,
    PeerDeleteResponse, Ready, ServiceReadyState, VdevStartResponse,
};
use crate::{Transport, WmiError};

/// Host-side state created by `ath11k_wmi_attach` and destroyed by detach.
/// HTC service establishment stays behind the caller-supplied `Transport`.
pub struct Wmi<T> {
    events: EventStream<T>,
    pdev_ids: Vec<u32>,
    connected: bool,
}

impl<T: Transport> Wmi<T> {
    pub fn attach(transport: T) -> Self {
        Self {
            events: EventStream::new(transport),
            pdev_ids: Vec::new(),
            connected: false,
        }
    }

    pub fn pdev_attach(&mut self, pdev_id: u32) {
        self.pdev_ids.push(pdev_id);
    }

    /// Records the point at which Linux connects the WMI control service.
    /// Concrete HTC connection work is performed by the Transport adapter.
    pub fn connect(&mut self) {
        self.connected = true;
    }

    pub fn is_connected(&self) -> bool {
        self.connected
    }

    pub fn pdev_ids(&self) -> &[u32] {
        &self.pdev_ids
    }

    pub fn wait_for_service_ready(
        &mut self,
        deadline_ns: u64,
    ) -> Result<ServiceReadyState, WmiError> {
        self.events.wait_for_service_ready(deadline_ns)
    }

    pub fn cmd_init(&mut self, request: &Init) -> Result<(), WmiError> {
        self.send(request)
    }

    pub fn wait_for_unified_ready(&mut self, deadline_ns: u64) -> Result<Ready, WmiError> {
        self.events.wait_for_unified_ready(deadline_ns)
    }

    pub fn discard_regulatory_update(&mut self) {
        self.events.discard_regulatory_update();
    }

    pub fn wait_for_regulatory_update(
        &mut self,
        deadline_ns: u64,
    ) -> Result<crate::event::RegulatoryChannelList, WmiError> {
        self.events.wait_for_regulatory_update(deadline_ns)
    }

    pub fn wait_for_vdev_start(
        &mut self,
        deadline_ns: u64,
        vdev_id: u32,
    ) -> Result<VdevStartResponse, WmiError> {
        self.events.wait_for_vdev_start(deadline_ns, vdev_id)
    }

    pub fn discard_vdev_start(&mut self, vdev_id: u32) {
        self.events.discard_vdev_start(vdev_id);
    }

    pub fn wait_for_peer_created(
        &mut self,
        deadline_ns: u64,
        vdev_id: u32,
        peer: [u8; 6],
    ) -> Result<PeerCreateConfirmation, WmiError> {
        self.events
            .wait_for_peer_created(deadline_ns, vdev_id, peer)
    }

    pub fn discard_peer_created(&mut self, vdev_id: u32, peer: [u8; 6]) {
        self.events.discard_peer_created(vdev_id, peer);
    }

    pub fn discard_peer_deleted(&mut self, vdev_id: u32, peer: [u8; 6]) {
        self.events.discard_peer_deleted(vdev_id, peer);
    }

    pub fn discard_peer_associated(&mut self, vdev_id: u32, peer: [u8; 6]) {
        self.events.discard_peer_associated(vdev_id, peer);
    }

    pub fn discard_key_installed(&mut self, vdev_id: u32, key_index: u32) {
        self.events.discard_key_installed(vdev_id, key_index);
    }

    pub fn wait_for_peer_deleted(
        &mut self,
        deadline_ns: u64,
        vdev_id: u32,
        peer: [u8; 6],
    ) -> Result<PeerDeleteResponse, WmiError> {
        self.events
            .wait_for_peer_deleted(deadline_ns, vdev_id, peer)
    }

    pub fn wait_for_peer_associated(
        &mut self,
        deadline_ns: u64,
        vdev_id: u32,
        peer: [u8; 6],
    ) -> Result<PeerAssocConfirmation, WmiError> {
        self.events
            .wait_for_peer_associated(deadline_ns, vdev_id, peer)
    }

    pub fn wait_for_key_installed(
        &mut self,
        deadline_ns: u64,
        vdev_id: u32,
        key_index: u32,
    ) -> Result<InstallKeyCompletion, WmiError> {
        self.events
            .wait_for_key_installed(deadline_ns, vdev_id, key_index)
    }

    pub fn send<R: EncodeCommand>(&mut self, request: &R) -> Result<(), WmiError> {
        self.events.transport_mut().send(request.encode_command()?)
    }

    pub fn pop_pending_event(&mut self) -> Option<crate::Event> {
        self.events.pop_pending()
    }

    pub fn next_event(&mut self, deadline_ns: u64) -> Result<Option<crate::Event>, WmiError> {
        self.events.next_event(deadline_ns)
    }

    pub fn detach(self) -> T {
        self.events.into_inner()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Command, Event};
    use alloc::collections::VecDeque;

    #[derive(Default)]
    struct MockTransport {
        commands: Vec<Command>,
        incoming: VecDeque<Event>,
    }
    impl Transport for MockTransport {
        fn send(&mut self, command: Command) -> Result<(), WmiError> {
            self.commands.push(command);
            Ok(())
        }
        fn receive(&mut self, _deadline_ns: u64) -> Result<Option<Event>, WmiError> {
            Ok(self.incoming.pop_front())
        }
    }

    #[test]
    fn attach_connect_init_detach_preserves_transport() {
        let mut wmi = Wmi::attach(MockTransport::default());
        wmi.pdev_attach(0);
        wmi.connect();
        wmi.cmd_init(&Init {
            resource_config: Default::default(),
            memory_chunks: Vec::new(),
            hardware_mode: None,
            bands: Vec::new(),
        })
        .unwrap();
        assert!(wmi.is_connected());
        assert_eq!(wmi.pdev_ids(), &[0]);
        let transport = wmi.detach();
        assert_eq!(transport.commands.len(), 1);
        assert_eq!(transport.commands[0].id, crate::tags::WMI_INIT_CMDID);
    }

    fn vdev_start_response(vdev_id: u32, status: u32) -> Event {
        let mut tlvs = Vec::new();
        tlvs.extend_from_slice(
            &((u32::from(crate::tags::WMI_TAG_VDEV_START_RESPONSE_EVENT.0) << 16) | 40)
                .to_le_bytes(),
        );
        for word in [vdev_id, 0, 0, status, 0, 0, 0, 0, 0, 0] {
            tlvs.extend_from_slice(&word.to_le_bytes());
        }
        Event::from_tlvs(crate::tags::WMI_VDEV_START_RESP_EVENTID, tlvs).unwrap()
    }

    fn install_key_completion(vdev_id: u32, key_index: u32, status: u32) -> Event {
        let mut value = Vec::new();
        value.extend_from_slice(&vdev_id.to_le_bytes());
        value.extend_from_slice(&[2, 0, 0, 0, 0, 2, 0, 0]);
        value.extend_from_slice(&key_index.to_le_bytes());
        value.extend_from_slice(&0u32.to_le_bytes());
        value.extend_from_slice(&status.to_le_bytes());
        let mut tlvs = Vec::new();
        tlvs.extend_from_slice(
            &((u32::from(crate::tags::WMI_TAG_VDEV_INSTALL_KEY_COMPLETE_EVENT.0) << 16)
                | value.len() as u32)
                .to_le_bytes(),
        );
        tlvs.extend_from_slice(&value);
        Event::from_tlvs(crate::tags::WMI_VDEV_INSTALL_KEY_COMPLETE_EVENTID, tlvs).unwrap()
    }

    fn regulatory_update() -> Event {
        let mut fixed = alloc::vec![0; 56];
        fixed[48..52].copy_from_slice(&1u32.to_le_bytes());
        let mut tlvs = Vec::new();
        tlvs.extend_from_slice(
            &((u32::from(crate::tags::WMI_TAG_REG_CHAN_LIST_CC_EVENT.0) << 16)
                | fixed.len() as u32)
                .to_le_bytes(),
        );
        tlvs.extend_from_slice(&fixed);
        tlvs.extend_from_slice(
            &((u32::from(crate::tags::WMI_TAG_ARRAY_STRUCT.0) << 16) | 16).to_le_bytes(),
        );
        tlvs.extend_from_slice(&[0; 16]);
        Event::from_tlvs(crate::tags::WMI_REG_CHAN_LIST_CC_EVENTID, tlvs).unwrap()
    }

    #[test]
    fn vdev_start_wait_correlates_id_and_retains_earlier_events() {
        let unrelated = Event::from_tlvs(crate::EventId(0x123), Vec::new()).unwrap();
        let mismatched = vdev_start_response(2, 0);
        let matched = vdev_start_response(1, 7);
        let mut wmi = Wmi::attach(MockTransport {
            commands: Vec::new(),
            incoming: VecDeque::from([unrelated.clone(), mismatched.clone(), matched]),
        });
        let response = wmi.wait_for_vdev_start(10, 1).unwrap();
        assert_eq!((response.vdev_id, response.status), (1, 7));
        assert_eq!(wmi.next_event(10), Ok(Some(unrelated)));
        assert_eq!(wmi.next_event(10), Ok(Some(mismatched)));
    }

    #[test]
    fn new_vdev_start_discards_a_pending_same_vdev_completion() {
        let stale = vdev_start_response(1, 0);
        let matched = vdev_start_response(1, 7);
        let mut wmi = Wmi::attach(MockTransport {
            commands: Vec::new(),
            incoming: VecDeque::from([stale]),
        });
        assert_eq!(wmi.wait_for_vdev_start(10, 2), Err(WmiError::Timeout));
        wmi.events.transport_mut().incoming.push_back(matched);
        wmi.discard_vdev_start(1);
        let response = wmi.wait_for_vdev_start(10, 1).unwrap();
        assert_eq!(response.status, 7);
    }

    #[test]
    fn key_wait_correlates_index_and_retains_other_completion() {
        let other = install_key_completion(1, 2, 0);
        let matched = install_key_completion(1, 1, 7);
        let mut wmi = Wmi::attach(MockTransport {
            commands: Vec::new(),
            incoming: VecDeque::from([other.clone(), matched]),
        });
        let response = wmi.wait_for_key_installed(10, 1, 1).unwrap();
        assert_eq!((response.key_index, response.status), (1, 7));
        assert_eq!(wmi.next_event(10), Ok(Some(other)));
    }

    #[test]
    fn new_country_discards_the_pre_ready_regulatory_event() {
        let stale = regulatory_update();
        let fresh = regulatory_update();
        let unrelated = Event::from_tlvs(crate::EventId(0x123), Vec::new()).unwrap();
        let mut wmi = Wmi::attach(MockTransport {
            commands: Vec::new(),
            incoming: VecDeque::from([stale, unrelated.clone()]),
        });
        assert_eq!(wmi.wait_for_vdev_start(10, 1), Err(WmiError::Timeout));
        wmi.discard_regulatory_update();
        wmi.events.transport_mut().incoming.push_back(fresh);
        let update = wmi.wait_for_regulatory_update(10).unwrap();
        assert!(!update.extended);
        assert_eq!(update.rules.len(), 1);
        assert_eq!(wmi.next_event(10), Ok(Some(unrelated)));
    }
}
