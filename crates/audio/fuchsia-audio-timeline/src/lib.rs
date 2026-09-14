//! Safe scalar bridge to Fuchsia's pinned audio timeline implementation.

#![deny(unsafe_op_in_unsafe_fn)]

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct TimelineFunction {
    subject_time: i64,
    reference_time: i64,
    subject_delta: u64,
    reference_delta: u64,
}

impl TimelineFunction {
    pub fn new(
        subject_time: i64,
        reference_time: i64,
        subject_delta: u64,
        reference_delta: u64,
    ) -> Option<Self> {
        (reference_delta != 0).then_some(Self {
            subject_time,
            reference_time,
            subject_delta,
            reference_delta,
        })
    }

    pub fn apply(self, reference_input: i64) -> i64 {
        // SAFETY: the bridge accepts scalars only. `new` maintains Fuchsia's
        // nonzero reference-delta precondition, and no pointers cross the ABI.
        unsafe {
            drv_fuchsia_timeline_apply(
                self.subject_time,
                self.reference_time,
                self.subject_delta,
                self.reference_delta,
                reference_input,
            )
        }
    }
}

unsafe extern "C" {
    fn drv_fuchsia_timeline_apply(
        subject_time: i64,
        reference_time: i64,
        subject_delta: u64,
        reference_delta: u64,
        reference_input: i64,
    ) -> i64;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn calls_fuchsia_flooring_timeline_function() {
        let timeline = TimelineFunction::new(10, 100, 1, 4).unwrap();
        assert_eq!(timeline.apply(111), 12);
        assert_eq!(timeline.apply(89), 7);
    }

    #[test]
    fn rejects_zero_reference_delta_before_ffi() {
        assert_eq!(TimelineFunction::new(0, 0, 1, 0), None);
    }
}
