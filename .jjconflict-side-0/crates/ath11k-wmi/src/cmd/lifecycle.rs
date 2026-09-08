//! Source-shaped WMI lifecycle seam used by ath11k-core.

use alloc::vec::Vec;

use super::{EncodeCommand, Init};
use crate::event::{EventStream, Ready, ServiceReadyState};
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

    pub fn send<R: EncodeCommand>(&mut self, request: &R) -> Result<(), WmiError> {
        self.events.transport_mut().send(request.encode_command()?)
    }

    pub fn pop_pending_event(&mut self) -> Option<crate::Event> {
        self.events.pop_pending()
    }

    pub fn detach(self) -> T {
        self.events.into_inner()
    }
}
