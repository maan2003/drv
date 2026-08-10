//! Safe host bridge to Fuchsia's pinned audio gain processing.

#![deny(unsafe_op_in_unsafe_fn)]

/// Applies Fuchsia's decibel conversion and non-unity gain operation in place.
pub fn apply_gain_s16(samples: &mut [i16], gain_db: f32) {
    // SAFETY: the mutable slice provides a valid, exclusively borrowed region
    // for exactly `len` i16 samples; the bridge does not retain the pointer.
    unsafe { drv_fuchsia_apply_gain_s16(samples.as_mut_ptr(), samples.len(), gain_db) }
}

unsafe extern "C" {
    fn drv_fuchsia_apply_gain_s16(samples: *mut i16, sample_count: usize, gain_db: f32);
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
}
