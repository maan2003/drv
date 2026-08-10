//! First PipeWire compatibility slice: a virtual PCM endpoint and its SPA format POD.
//!
//! The endpoint contract deliberately contains no PipeWire or host-audio types. The
//! compatibility frontend can therefore be replaced without changing the eventual
//! Fuchsia-derived device/graph implementation.

#![forbid(unsafe_code)]

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

pub const VIRTUAL_SINK_FORMAT: PcmFormat = PcmFormat {
    sample_format: SampleFormat::Signed16Le,
    rate: 48_000,
    channels: 2,
};

/// Minimal backend surface needed after PipeWire has negotiated a playback format.
pub trait PlaybackEndpoint {
    fn format(&self) -> PcmFormat;
    fn write(&mut self, pcm: &[u8]) -> Result<(), EndpointError>;
    fn frame_position(&self) -> u64;
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum EndpointError {
    PartialFrame,
}

/// A hardware-free endpoint that deterministically consumes complete PCM frames.
#[derive(Debug, Default)]
pub struct VirtualPcmEndpoint {
    frames: u64,
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
        self.frames += (pcm.len() / FRAME_BYTES) as u64;
        Ok(())
    }

    fn frame_position(&self) -> u64 {
        self.frames
    }
}

// Values from PipeWire's stable spa/param/audio/raw-types.h ABI. The native SPA
// crate models generic IDs but intentionally does not duplicate these audio IDs.
#[repr(u32)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum SpaAudioFormat {
    S16Le = 4,
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
            4 => Ok(Self::S16Le),
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
                .push_property(Format::AudioRate, PropertyFlags::empty(), 48_000_i32)
                .push_property(Format::AudioChannels, PropertyFlags::empty(), 2_i32)
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
    fn enum_format_is_a_native_spa_object_with_expected_fields() {
        let mut storage = [0; 256];
        let pod = enum_format_pod(&mut storage).unwrap();
        let mut parser = Parser::new(pod);

        let (properties, consumed) = parser
            .pop_object::<Format, ParamType, _>(|object, id| {
                assert_eq!(id, ParamType::EnumFormat);
                Ok(object
                    .map(|(key, _, value)| (key, value.type_()))
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
