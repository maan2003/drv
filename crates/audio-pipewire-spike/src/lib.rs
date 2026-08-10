//! First PipeWire compatibility slice: a virtual PCM endpoint and its SPA format POD.
//!
//! The endpoint contract deliberately contains no PipeWire or host-audio types. The
//! compatibility frontend can therefore be replaced without changing the eventual
//! Fuchsia-derived device/graph implementation.

#![forbid(unsafe_code)]

use drv_fuchsia_audio_processing::apply_gain_s16;
use drv_fuchsia_audio_timeline::TimelineFunction;
use pipewire_native_spa::{
    param::{
        ParamType,
        format::{Format, MediaSubtype, MediaType},
    },
    pod::{
        Error,
        builder::Builder,
        types::{Id, ObjectType, PropertyFlags},
    },
};

mod device_registry;
pub mod protocol;

/// The project-owned PCM contract at the compatibility/backend boundary.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PcmFormat {
    pub sample_format: SampleFormat,
    pub rate: u32,
    pub channels: u32,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SampleFormat {
    Signed16Le,
}

pub const VIRTUAL_SINK_FORMAT: PcmFormat = device_registry::PLAYBACK_FORMAT;

/// Fixed non-unity gain used to prove pinned Fuchsia processing is in-path.
pub const VIRTUAL_SINK_GAIN_DB: f32 = -6.020_600_3;

/// Minimal backend surface needed after PipeWire has negotiated a playback format.
pub trait PlaybackEndpoint {
    fn format(&self) -> PcmFormat;
    fn write(&mut self, pcm: &[u8]) -> Result<(), EndpointError>;
    fn frame_position(&self) -> u64;
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum EndpointError {
    PartialFrame,
    PositionOverflow,
}

/// A hardware-free endpoint that deterministically consumes complete PCM frames.
#[derive(Debug)]
pub struct VirtualPcmEndpoint {
    bytes_consumed: i64,
    frames_from_bytes: TimelineFunction,
    processed_sample_checksum: i64,
}

impl Default for VirtualPcmEndpoint {
    fn default() -> Self {
        Self {
            bytes_consumed: 0,
            frames_from_bytes: TimelineFunction::new(0, 0, 1, 4).unwrap(),
            processed_sample_checksum: 0,
        }
    }
}

impl PlaybackEndpoint for VirtualPcmEndpoint {
    fn format(&self) -> PcmFormat {
        VIRTUAL_SINK_FORMAT
    }

    fn write(&mut self, pcm: &[u8]) -> Result<(), EndpointError> {
        const FRAME_BYTES: usize = 2 * 2;
        if !pcm.len().is_multiple_of(FRAME_BYTES) {
            return Err(EndpointError::PartialFrame);
        }
        let mut samples = pcm
            .chunks_exact(2)
            .map(|sample| i16::from_le_bytes(sample.try_into().unwrap()))
            .collect::<Vec<_>>();
        apply_gain_s16(&mut samples, VIRTUAL_SINK_GAIN_DB);
        self.processed_sample_checksum = samples
            .iter()
            .fold(self.processed_sample_checksum, |sum, sample| {
                sum.wrapping_add(i64::from(*sample))
            });
        self.bytes_consumed = self
            .bytes_consumed
            .checked_add(i64::try_from(pcm.len()).map_err(|_| EndpointError::PositionOverflow)?)
            .ok_or(EndpointError::PositionOverflow)?;
        Ok(())
    }

    fn frame_position(&self) -> u64 {
        self.frames_from_bytes.apply(self.bytes_consumed) as u64
    }
}

impl VirtualPcmEndpoint {
    /// Deterministic evidence of samples after pinned Fuchsia gain processing.
    pub fn processed_sample_checksum(&self) -> i64 {
        self.processed_sample_checksum
    }
}

// Values from PipeWire's stable spa/param/audio/raw-types.h ABI. The native SPA
// crate models generic IDs but intentionally does not duplicate these audio IDs.
#[repr(u32)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum SpaAudioFormat {
    S16Le = 0x103,
}

impl From<SpaAudioFormat> for u32 {
    fn from(value: SpaAudioFormat) -> Self {
        value as Self
    }
}

impl TryFrom<u32> for SpaAudioFormat {
    type Error = ();

    fn try_from(value: u32) -> Result<Self, Self::Error> {
        match value {
            0x103 => Ok(Self::S16Le),
            _ => Err(()),
        }
    }
}

#[repr(u32)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum SpaAudioChannel {
    FrontLeft = 3,
    FrontRight = 4,
}

impl From<SpaAudioChannel> for u32 {
    fn from(value: SpaAudioChannel) -> Self {
        value as Self
    }
}

impl TryFrom<u32> for SpaAudioChannel {
    type Error = ();

    fn try_from(value: u32) -> Result<Self, Self::Error> {
        match value {
            3 => Ok(Self::FrontLeft),
            4 => Ok(Self::FrontRight),
            _ => Err(()),
        }
    }
}

/// Encode the `SPA_PARAM_EnumFormat` advertised by the virtual playback port.
///
/// The returned bytes are directly suitable for a PipeWire native-protocol
/// node/port parameter event; no project-specific envelope is introduced.
pub fn enum_format_pod(storage: &mut [u8]) -> Result<&[u8], Error> {
    enum_format_pod_for(VIRTUAL_SINK_FORMAT, storage)
}

fn enum_format_pod_for(format: PcmFormat, storage: &mut [u8]) -> Result<&[u8], Error> {
    debug_assert_eq!(format.sample_format, SampleFormat::Signed16Le);
    debug_assert_eq!(format.channels, 2);
    Builder::new(storage)
        .push_object(ObjectType::Format, ParamType::EnumFormat, |object| {
            object
                .push_property(
                    Format::MediaType,
                    PropertyFlags::empty(),
                    Id(MediaType::Audio),
                )
                .push_property(
                    Format::MediaSubtype,
                    PropertyFlags::empty(),
                    Id(MediaSubtype::Raw),
                )
                .push_property(
                    Format::AudioFormat,
                    PropertyFlags::empty(),
                    Id(SpaAudioFormat::S16Le),
                )
                .push_property(
                    Format::AudioRate,
                    PropertyFlags::empty(),
                    format.rate as i32,
                )
                .push_property(
                    Format::AudioChannels,
                    PropertyFlags::empty(),
                    format.channels as i32,
                )
                .push_property(
                    Format::AudioPosition,
                    PropertyFlags::empty(),
                    &[
                        Id(SpaAudioChannel::FrontLeft),
                        Id(SpaAudioChannel::FrontRight),
                    ][..],
                )
        })
        .build()
}

#[cfg(test)]
mod tests {
    use super::*;
    use pipewire_native_spa::pod::{parser::Parser, types::Type};

    #[test]
    fn virtual_endpoint_advances_only_by_complete_stereo_frames() {
        let mut endpoint = VirtualPcmEndpoint::default();
        endpoint.write(&[0; 16]).unwrap();
        assert_eq!(endpoint.frame_position(), 4);
        assert_eq!(endpoint.write(&[0; 3]), Err(EndpointError::PartialFrame));
        assert_eq!(endpoint.frame_position(), 4);
    }

    #[test]
    fn pipewire_virtual_sink_position_runs_through_fuchsia_timeline() {
        let mut storage = [0; 256];
        assert!(enum_format_pod(&mut storage).is_ok());

        let mut endpoint = VirtualPcmEndpoint::default();
        endpoint.write(&vec![0; 48_000 * 4]).unwrap();
        assert_eq!(endpoint.frame_position(), 48_000);
    }

    #[test]
    fn virtual_endpoint_runs_pcm_through_pinned_fuchsia_gain() {
        let mut endpoint = VirtualPcmEndpoint::default();
        let mut pcm = Vec::new();
        pcm.extend(20_000_i16.to_le_bytes());
        pcm.extend(10_000_i16.to_le_bytes());
        endpoint.write(&pcm).unwrap();

        assert_eq!(endpoint.frame_position(), 1);
        assert_eq!(endpoint.processed_sample_checksum(), 15_000);
    }

    #[test]
    fn enum_format_is_a_native_spa_object_with_expected_fields() {
        let mut storage = [0; 256];
        let pod = enum_format_pod(&mut storage).unwrap();
        let mut parser = Parser::new(pod);

        let (properties, consumed) = parser
            .pop_object::<Format, ParamType, _>(|object, id| {
                assert_eq!(id, ParamType::EnumFormat);
                Ok(object
                    .map(|(key, _, value)| {
                        if key == Format::AudioFormat {
                            assert_eq!(
                                value.decode::<Id<SpaAudioFormat>>().unwrap(),
                                Id(SpaAudioFormat::S16Le)
                            );
                        }
                        (key, value.type_())
                    })
                    .collect::<Vec<_>>())
            })
            .unwrap();

        assert_eq!(consumed, pod.len());
        assert_eq!(parser.available(), 0);
        assert_eq!(
            properties,
            vec![
                (Format::MediaType, Type::Id),
                (Format::MediaSubtype, Type::Id),
                (Format::AudioFormat, Type::Id),
                (Format::AudioRate, Type::Int),
                (Format::AudioChannels, Type::Int),
                (Format::AudioPosition, Type::Array),
            ]
        );

        // Header is also checked independently of the decoder: 0x40003 is
        // SPA_TYPE_OBJECT_Format and 3 is SPA_PARAM_EnumFormat.
        assert_eq!(u32::from_ne_bytes(pod[4..8].try_into().unwrap()), 15);
        assert_eq!(u32::from_ne_bytes(pod[8..12].try_into().unwrap()), 0x40003);
        assert_eq!(u32::from_ne_bytes(pod[12..16].try_into().unwrap()), 3);
    }
}
