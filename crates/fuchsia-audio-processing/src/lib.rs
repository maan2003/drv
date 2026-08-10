//! Safe host bridge to Fuchsia's pinned audio gain processing.

#![deny(unsafe_op_in_unsafe_fn)]

/// Applies Fuchsia's decibel conversion and non-unity gain operation in place.
pub fn apply_gain_s16(samples: &mut [i16], gain_db: f32) {
    // SAFETY: the mutable slice provides a valid, exclusively borrowed region
    // for exactly `len` i16 samples; the bridge does not retain the pointer.
    unsafe { drv_fuchsia_apply_gain_s16(samples.as_mut_ptr(), samples.len(), gain_db) }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MixError {
    LengthMismatch,
    PartialStereoFrame,
}

/// Mixes two equal-length stereo streams through Fuchsia's planar channel strip.
pub fn mix_stereo_s16(first: &[i16], second: &[i16]) -> Result<Vec<i16>, MixError> {
    if first.len() != second.len() {
        return Err(MixError::LengthMismatch);
    }
    if !first.len().is_multiple_of(2) {
        return Err(MixError::PartialStereoFrame);
    }
    let mut dest = vec![0; first.len()];
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
    Ok(dest)
}

unsafe extern "C" {
    fn drv_fuchsia_apply_gain_s16(samples: *mut i16, sample_count: usize, gain_db: f32);
    fn drv_fuchsia_mix_stereo_s16(
        first: *const i16,
        second: *const i16,
        dest: *mut i16,
        frame_count: usize,
    );
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
    fn pinned_fuchsia_channel_strip_mixes_two_stereo_streams() {
        let first = [10_000, -10_000, 20_000, -20_000];
        let second = [5_000, 5_000, 20_000, -20_000];
        assert_eq!(
            mix_stereo_s16(&first, &second).unwrap(),
            [15_000, -5_000, i16::MAX, i16::MIN]
        );
    }
}
