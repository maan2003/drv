//! Host adaptation of the pinned Fuchsia Audio Device Registry contracts.
//!
//! The unchanged upstream sources that define the device registration and
//! ring-buffer lifecycle are packaged under `upstream-fuchsia/`. This module
//! supplies only the host-owned storage and PipeWire-facing projection needed
//! by the bounded virtual-device slice.

use crate::{EndpointError, PcmFormat, PlaybackEndpoint, SampleFormat, VirtualPcmEndpoint};

const PLAYBACK_TOKEN_ID: u64 = 2;
const PLAYBACK_ELEMENT_ID: u64 = 1;
pub(crate) const PLAYBACK_FORMAT: PcmFormat = PcmFormat {
    sample_format: SampleFormat::Signed16Le,
    rate: 48_000,
    channels: 2,
};

#[derive(Debug)]
pub(crate) struct DeviceRegistry {
    playback: RegisteredDevice,
}

#[derive(Debug)]
pub(crate) struct RegisteredDevice {
    info: RegisteredDeviceInfo,
    ring_buffer: RingBufferEndpoint,
}

#[derive(Clone, Debug)]
pub(crate) struct RegisteredDeviceInfo {
    token_id: u64,
    element_id: u64,
    name: String,
    description: String,
    format: PcmFormat,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum RingBufferState {
    // Names and transitions follow media_audio::Device::RingBufferState.
    Stopped,
    Started,
}

#[derive(Debug)]
struct RingBufferEndpoint {
    state: RingBufferState,
    endpoint: VirtualPcmEndpoint,
}

impl DeviceRegistry {
    pub(crate) fn register_virtual_playback() -> Self {
        Self::register_playback_with_format(PLAYBACK_FORMAT)
    }

    pub(crate) fn register_playback_with_format(format: PcmFormat) -> Self {
        Self {
            playback: RegisteredDevice {
                info: RegisteredDeviceInfo {
                    token_id: PLAYBACK_TOKEN_ID,
                    element_id: PLAYBACK_ELEMENT_ID,
                    name: "drv.adr-virtual-sink".into(),
                    description: "drv Fuchsia ADR Virtual Sink".into(),
                    format,
                },
                // The virtual device is initialized and its one ring buffer is
                // created before it becomes visible, matching ADR readiness.
                ring_buffer: RingBufferEndpoint {
                    state: RingBufferState::Stopped,
                    endpoint: VirtualPcmEndpoint::default(),
                },
            },
        }
    }

    pub(crate) fn playback(&self) -> &RegisteredDevice {
        &self.playback
    }

    pub(crate) fn playback_mut(&mut self) -> &mut RegisteredDevice {
        &mut self.playback
    }
}

impl RegisteredDeviceInfo {
    pub(crate) fn token_id(&self) -> u64 {
        self.token_id
    }

    pub(crate) fn node_id(&self) -> i32 {
        self.token_id as i32
    }

    pub(crate) fn port_id(&self) -> i32 {
        self.node_id() + self.element_id as i32
    }

    pub(crate) fn format(&self) -> PcmFormat {
        self.format
    }

    pub(crate) fn name(&self) -> &str {
        &self.name
    }

    pub(crate) fn description(&self) -> &str {
        &self.description
    }

    pub(crate) fn sample_format_name(&self) -> &'static str {
        match self.format.sample_format {
            SampleFormat::Signed16Le => "S16LE",
        }
    }
}

impl RegisteredDevice {
    pub(crate) fn info(&self) -> &RegisteredDeviceInfo {
        &self.info
    }

    pub(crate) fn write_ring_buffer(&mut self, pcm: &[u8]) -> Result<(), EndpointError> {
        self.ring_buffer.start();
        self.ring_buffer.endpoint.write(pcm)
    }

    pub(crate) fn frame_position(&self) -> u64 {
        self.ring_buffer.endpoint.frame_position()
    }

    pub(crate) fn processed_sample_checksum(&self) -> i64 {
        self.ring_buffer.endpoint.processed_sample_checksum()
    }
}

impl RingBufferEndpoint {
    fn start(&mut self) {
        // ADR rejects a second Start; this bounded endpoint starts once and
        // remains started for the lifetime of its registered device.
        if self.state == RingBufferState::Stopped {
            self.state = RingBufferState::Started;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn registered_device_owns_format_identity_and_ring_position() {
        let mut registry = DeviceRegistry::register_virtual_playback();
        let device = registry.playback_mut();
        assert_eq!(device.info().token_id(), 2);
        assert_eq!(device.info().node_id(), 2);
        assert_eq!(device.info().port_id(), 3);
        assert_eq!(device.info().format().rate, 48_000);

        device.write_ring_buffer(&[0; 480 * 4]).unwrap();
        assert_eq!(device.frame_position(), 480);
        assert_eq!(device.ring_buffer.state, RingBufferState::Started);
    }
}
