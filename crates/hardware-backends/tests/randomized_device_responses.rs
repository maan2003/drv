use drv_hardware::{Bidirectional, Error};
use drv_hardware_backends::{DeterministicBackend, DeviceResponseInput, run_edu_sequence};
use proptest::prelude::*;

// Prefix from artifacts/redwood-native-ath11k/deterministic-dry-run.jsonl.
// Keeping a known response shape in every input lets handshakes progress while
// proptest varies the bytes and device scheduling decisions around it.
const RECORDED_RESPONSE_PREFIX: [u8; 8] = [0x01, 0x50, 0, 0, 0x24, 0, 0x56, 0];

fn response_input() -> impl Strategy<Value = DeviceResponseInput> {
    (
        prop::collection::vec(any::<u32>(), 0..=16),
        prop::collection::vec(any::<u8>(), 0..=32),
        prop::collection::vec(0_usize..=8, 0..=16),
        prop::collection::vec(0_usize..=31, 0..=32),
        any::<bool>(),
    )
        .prop_map(
            |(
                register_reads,
                varied_bytes,
                completions_per_poll,
                completion_order,
                stays_silent,
            )| {
                let mut response_bytes = RECORDED_RESPONSE_PREFIX.to_vec();
                response_bytes.extend(varied_bytes);
                DeviceResponseInput {
                    register_reads,
                    response_bytes,
                    completions_per_poll,
                    completion_order,
                    stays_silent,
                }
            },
        )
}

fn exercise(input: DeviceResponseInput) {
    const POLL_BUDGET: usize = 16;
    let (device, operations, model) =
        DeterministicBackend::recording_noncoherent_device_with_model(input.clone());

    // The real safe DMA sequence is used rather than direct backend access.
    // Any ownership discrepancy therefore returns an error at the same seam
    // used by drivers.
    let result = run_edu_sequence(&device);
    if input.stays_silent || input.completions_per_poll.first().copied().unwrap_or(0) == 0 {
        assert_eq!(result, Err(Error::Timeout));
    }

    assert!(
        model.remaining_decisions()
            <= input.register_reads.len() + input.completions_per_poll.len()
    );
    assert!(
        operations.borrow().len() <= 32,
        "bounded sequence exceeded its operation budget"
    );

    // A second bounded polling loop covers silence and completion batches
    // independently of the fixed sequence above.
    let interrupt = device.open_interrupt(0).unwrap();
    for deadline in 0..POLL_BUDGET as u64 {
        let _ = interrupt.wait_until(deadline).unwrap();
    }

    // Exercise device handoff and acquire accounting once more. A repeated
    // handoff without acquire must fail closed on the noncoherent backend.
    let mut dma = device.alloc_streaming::<Bidirectional>(16, 8).unwrap();
    dma.write(0, &[1, 2, 3, 4]).unwrap();
    dma.sync_for_device(0, 4).unwrap();
    assert_eq!(dma.sync_for_device(0, 4), Err(Error::DeviceFault));
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(128))]

    #[test]
    fn randomized_device_response_testing_is_bounded(input in response_input()) {
        exercise(input);
    }
}

#[test]
fn recorded_input_completes_the_safe_dma_sequence() {
    let input = DeviceResponseInput {
        response_bytes: RECORDED_RESPONSE_PREFIX.to_vec(),
        completions_per_poll: vec![1, 1],
        ..DeviceResponseInput::default()
    };
    exercise(input);
}
