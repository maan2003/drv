//! Safe host bridge to Fuchsia's pinned audio processing primitives.

#![deny(unsafe_op_in_unsafe_fn)]

/// Applies Fuchsia's decibel conversion and non-unity gain operation in place.
pub fn apply_gain_s16(samples: &mut [i16], gain_db: f32) {
    // SAFETY: the mutable slice provides a valid, exclusively borrowed region
    // for exactly `len` i16 samples; the bridge does not retain the pointer.
    unsafe { drv_fuchsia_apply_gain_s16(samples.as_mut_ptr(), samples.len(), gain_db) }
}

/// Computes Fuchsia's gain scale for later use by the real-time entry point.
///
/// Call this while constructing the graph, not on the real-time thread.
pub fn gain_db_to_scale(gain_db: f32) -> f32 {
    // SAFETY: this scalar bridge retains no state or pointers.
    unsafe { drv_fuchsia_db_to_scale(gain_db) }
}

/// Applies a caller-precomputed linear gain without allocation or libm calls.
///
/// Compute or select `scale` off the real-time thread. For example, -6.0206 dB
/// is a scale of 0.5. Non-finite scales should also be rejected off-thread.
pub fn apply_gain_scale_s16(samples: &mut [i16], scale: f32) {
    // SAFETY: the mutable slice provides a valid, exclusively borrowed region
    // for exactly `len` i16 samples; the bridge does not retain the pointer.
    unsafe { drv_fuchsia_apply_gain_scale_s16(samples.as_mut_ptr(), samples.len(), scale) }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MixError {
    LengthMismatch,
    PartialStereoFrame,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ResampleError {
    PartialStereoFrame,
    FrameCountOverflow,
}

/// Mixes two equal-length stereo streams into newly allocated output.
pub fn mix_stereo_s16(first: &[i16], second: &[i16]) -> Result<Vec<i16>, MixError> {
    if first.len() != second.len() {
        return Err(MixError::LengthMismatch);
    }
    if !first.len().is_multiple_of(2) {
        return Err(MixError::PartialStereoFrame);
    }
    let mut dest = vec![0; first.len()];
    mix_stereo_s16_into(first, second, &mut dest)?;
    Ok(dest)
}

/// Mixes into caller-provided storage without allocation.
pub fn mix_stereo_s16_into(
    first: &[i16],
    second: &[i16],
    dest: &mut [i16],
) -> Result<(), MixError> {
    if first.len() != second.len() || first.len() != dest.len() {
        return Err(MixError::LengthMismatch);
    }
    if !first.len().is_multiple_of(2) {
        return Err(MixError::PartialStereoFrame);
    }
    // SAFETY: all three slices cover exactly `first.len()` samples and do not
    // overlap mutably; the bridge retains no pointers.
    unsafe {
        drv_fuchsia_mix_stereo_s16(
            first.as_ptr(),
            second.as_ptr(),
            dest.as_mut_ptr(),
            first.len() / 2,
        )
    }
    Ok(())
}

/// Point-resamples interleaved stereo S16 from 44.1 kHz onto a 48 kHz timeline.
///
/// The call treats `source` as one contiguous stream beginning at frame zero.
/// Fuchsia's pinned `PositionManager` owns the exact fractional source clock
/// progression, while the host bridge owns only PCM conversion and allocation.
pub fn resample_stereo_s16_44100_to_48000(source: &[i16]) -> Result<Vec<i16>, ResampleError> {
    if !source.len().is_multiple_of(2) {
        return Err(ResampleError::PartialStereoFrame);
    }
    let source_frames = source.len() / 2;
    let dest_frames = source_frames
        .checked_mul(48_000)
        .and_then(|frames| frames.checked_add(44_100 - 1))
        .map(|frames| frames / 44_100)
        .ok_or(ResampleError::FrameCountOverflow)?;
    let dest_samples = dest_frames
        .checked_mul(2)
        .ok_or(ResampleError::FrameCountOverflow)?;
    let mut dest = vec![0; dest_samples];
    // SAFETY: source and destination cover the declared stereo frame counts,
    // are separately borrowed, and the bridge retains no pointers.
    let produced = unsafe {
        drv_fuchsia_resample_stereo_s16_44100_to_48000(
            source.as_ptr(),
            source_frames,
            dest.as_mut_ptr(),
            dest_frames,
        )
    };
    debug_assert_eq!(produced, dest_frames);
    dest.truncate(produced * 2);
    Ok(dest)
}

unsafe extern "C" {
    fn drv_fuchsia_apply_gain_s16(samples: *mut i16, sample_count: usize, gain_db: f32);
    fn drv_fuchsia_db_to_scale(gain_db: f32) -> f32;
    fn drv_fuchsia_apply_gain_scale_s16(samples: *mut i16, sample_count: usize, scale: f32);
    fn drv_fuchsia_mix_stereo_s16(
        first: *const i16,
        second: *const i16,
        dest: *mut i16,
        frame_count: usize,
    );
    fn drv_fuchsia_resample_stereo_s16_44100_to_48000(
        source: *const i16,
        source_frame_count: usize,
        dest: *mut i16,
        dest_frame_count: usize,
    ) -> usize;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pinned_fuchsia_gain_attenuates_signed_pcm() {
        let mut samples = [20_000, -20_000, 1_000, -1_000];
        apply_gain_s16(&mut samples, -6.020_600_3);
        assert_eq!(samples, [10_000, -10_000, 500, -500]);
    }

    #[test]
    fn pinned_fuchsia_minimum_gain_is_silent() {
        let mut samples = [i16::MIN, i16::MAX];
        apply_gain_s16(&mut samples, -160.0);
        assert_eq!(samples, [0, 0]);
    }

    #[test]
    fn precomputed_gain_scale_is_deterministic() {
        let mut samples = [i16::MIN, -3, -1, 1, 3, i16::MAX];
        apply_gain_scale_s16(&mut samples, 0.5);
        assert_eq!(samples, [-16_384, -2, 0, 0, 2, 16_384]);
    }

    #[test]
    fn realtime_gain_matches_db_gain_for_every_s16_value() {
        let mut expected = (i16::MIN..=i16::MAX).collect::<Vec<_>>();
        let mut actual = expected.clone();
        apply_gain_s16(&mut expected, -6.020_600_3);
        let scale = gain_db_to_scale(-6.020_600_3);
        assert_eq!(scale.to_bits(), 0x3eff_ffff);
        apply_gain_scale_s16(&mut actual, scale);
        assert_eq!(actual, expected);
    }

    #[test]
    fn pinned_fuchsia_mix_sample_mixes_two_stereo_streams() {
        let first = [10_000, -10_000, 20_000, -20_000];
        let second = [5_000, 5_000, 20_000, -20_000];
        assert_eq!(
            mix_stereo_s16(&first, &second).unwrap(),
            [15_000, -5_000, i16::MAX, i16::MIN]
        );
    }

    #[test]
    fn caller_provided_mix_output_is_deterministic() {
        let first = [10_000, -10_000, 20_000, -20_000];
        let second = [5_000, 5_000, 20_000, -20_000];
        let mut dest = [0; 4];
        mix_stereo_s16_into(&first, &second, &mut dest).unwrap();
        assert_eq!(dest, [15_000, -5_000, i16::MAX, i16::MIN]);
    }

    #[test]
    fn pinned_fuchsia_positions_produce_exact_one_second_frame_count() {
        let source = vec![0; 44_100 * 2];
        assert_eq!(
            resample_stereo_s16_44100_to_48000(&source).unwrap().len(),
            48_000 * 2
        );
    }

    #[test]
    fn pinned_fuchsia_positions_select_deterministic_point_samples() {
        let source: Vec<_> = (0_i16..441).flat_map(|frame| [frame, -frame]).collect();
        let dest = resample_stereo_s16_44100_to_48000(&source).unwrap();
        assert_eq!(dest.len(), 480 * 2);
        for (dest_frame, samples) in dest.chunks_exact(2).enumerate() {
            let source_frame = (dest_frame * 44_100 / 48_000) as i16;
            assert_eq!(samples, [source_frame, -source_frame]);
        }
    }

    #[test]
    fn resampler_rejects_partial_stereo_frames() {
        assert_eq!(
            resample_stereo_s16_44100_to_48000(&[1]),
            Err(ResampleError::PartialStereoFrame)
        );
    }
}
