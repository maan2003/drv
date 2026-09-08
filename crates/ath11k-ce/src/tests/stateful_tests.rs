use super::*;
use proptest::prelude::*;

#[derive(Clone, Debug)]
enum Command {
    Send {
        length: u16,
        fail: bool,
    },
    ReceiveCredit {
        endpoint: u8,
        amount: u8,
        short: bool,
    },
    ReceivePayload {
        endpoint: u8,
        length: u16,
        short: bool,
    },
    TxBuffer {
        length: u8,
    },
    RxBuffer {
        length: u8,
    },
}

const DEFAULT_STATEFUL_CASES: u32 = 128;
const DEFAULT_STATEFUL_STEPS: usize = 64;
const STATEFUL_COMMAND_LIMIT: usize = 4096;

fn stateful_cases() -> u32 {
    std::env::var("ATH11K_STATEFUL_CASES")
        .or_else(|_| std::env::var("PROPTEST_CASES"))
        .ok()
        .map(|value| {
            value.parse().unwrap_or_else(|error| {
                panic!("ATH11K_STATEFUL_CASES must be a positive integer: {error}")
            })
        })
        .unwrap_or(DEFAULT_STATEFUL_CASES)
        .max(1)
}

fn stateful_max_steps() -> usize {
    std::env::var("ATH11K_STATEFUL_STEPS")
        .ok()
        .map(|value| {
            value.parse().unwrap_or_else(|error| {
                panic!("ATH11K_STATEFUL_STEPS must be a positive integer: {error}")
            })
        })
        .unwrap_or(DEFAULT_STATEFUL_STEPS)
        .clamp(1, STATEFUL_COMMAND_LIMIT)
}

fn command(endpoint: u8) -> impl Strategy<Value = Command> {
    prop_oneof![
        5 => (0_u16..=700, any::<bool>()).prop_map(|(length, fail)| Command::Send { length, fail }),
        4 => (prop_oneof![4 => Just(endpoint), 1 => 0_u8..=12], any::<u8>(), any::<bool>()).prop_map(
            |(endpoint, amount, short)| Command::ReceiveCredit { endpoint, amount, short }
        ),
        4 => (prop_oneof![4 => Just(endpoint), 1 => 0_u8..=12], 0_u16..=512, any::<bool>()).prop_map(
            |(endpoint, length, short)| Command::ReceivePayload { endpoint, length, short }
        ),
        2 => any::<u8>().prop_map(|length| Command::TxBuffer { length }),
        2 => any::<u8>().prop_map(|length| Command::RxBuffer { length }),
    ]
}

fn command_sequence(max_commands: usize) -> impl Strategy<Value = Vec<Command>> {
    let long_sequence_start = (max_commands / 2).max(1);
    (
        0_u8..=12,
        prop_oneof![
            1 => 1_usize..=max_commands,
            4 => long_sequence_start..=max_commands,
        ],
    )
        .prop_flat_map(|(endpoint, length)| {
            let max_runs = length.div_ceil(8).max(1);
            proptest::collection::vec((command(endpoint), 1_usize..=32), 1..=max_runs).prop_map(
                move |runs| {
                    let mut commands = Vec::with_capacity(length);
                    for (command, run_length) in runs {
                        commands.extend(core::iter::repeat_n(command, run_length));
                        if commands.len() >= length {
                            commands.truncate(length);
                            return commands;
                        }
                    }

                    let last = commands.last().cloned().expect("at least one command run");
                    commands.resize(length, last);
                    commands
                },
            )
        })
}

fn credit_frame(endpoint: u8, amount: u8, short: bool) -> Vec<u8> {
    let mut frame = Vec::from(
        HtcHeader {
            endpoint: 1,
            flags: 2,
            payload_len: if short { 7 } else { 8 },
            control_byte_0: if short { 7 } else { 8 },
            control_byte_1: 0,
        }
        .encode(),
    );
    frame.extend_from_slice(&[1, 4, 0, 0, endpoint, amount, 0, 0]);
    if short {
        frame.pop();
    }
    frame
}

fn payload_frame(endpoint: u8, length: u16, short: bool) -> Vec<u8> {
    let payload_len = usize::from(length).min(HTC_MAX_LEN);
    let mut frame = Vec::from(
        HtcHeader {
            endpoint,
            flags: 0,
            payload_len: length,
            control_byte_0: 0,
            control_byte_1: 0,
        }
        .encode(),
    );
    frame.resize(HTC_HEADER_LEN + payload_len, 0x5a);
    if short && frame.len() > HTC_HEADER_LEN {
        frame.pop();
    }
    frame
}

fn run_commands(commands: &[Command]) {
    let (device, operations) = DeterministicBackend::recording_noncoherent_device();
    let mut transport = HtcTransport::new(connected_wmi_htc(), PacketIo::default());
    let mut credits = 4_i32;
    let mut sequence = 0_u8;
    let mut emitted = 0_usize;

    for command in commands {
        match *command {
            Command::Send { length, fail } => {
                transport.io.fail_send = fail;
                let bytes = vec![0x5a; usize::from(length)];
                let needed = (bytes.len() + HTC_HEADER_LEN).div_ceil(256) as i32;
                let before = transport.io.sent.len();
                let result = transport.send(TxFrame {
                    service: ServiceId::WMI_CONTROL,
                    bytes,
                });
                if credits < needed {
                    assert_eq!(result, Err(CeError::NoCredits));
                } else {
                    sequence = sequence.wrapping_add(1);
                    if fail {
                        assert_eq!(result, Err(CeError::DeviceFault));
                    } else {
                        result.unwrap();
                        credits -= needed;
                        emitted += 1;
                    }
                }
                assert_eq!(
                    transport.io.sent.len() - before,
                    usize::from(!fail && credits >= 0 && result.is_ok())
                );
            }
            Command::ReceiveCredit {
                endpoint,
                amount,
                short,
            } => {
                let before = credits;
                transport
                    .io
                    .receive
                    .push_back(credit_frame(endpoint, amount, short));
                let result = transport.receive(0);
                if short {
                    assert_eq!(result, Err(CeError::InvalidFrame));
                } else {
                    assert_eq!(result, Ok(None));
                    if endpoint == 1 {
                        credits += i32::from(amount);
                    }
                }
                if short {
                    assert_eq!(credits, before);
                }
            }
            Command::ReceivePayload {
                endpoint,
                length,
                short,
            } => {
                transport
                    .io
                    .receive
                    .push_back(payload_frame(endpoint, length, short));
                let result = transport.receive(0);
                let valid = usize::from(endpoint) < HTC_ENDPOINT_COUNT
                    && usize::from(length) + HTC_HEADER_LEN <= HTC_MAX_LEN
                    && (!short || length == 0);
                if valid {
                    if length == 0 {
                        assert_eq!(result, Ok(None));
                    } else {
                        let frame = result.unwrap().unwrap();
                        assert_eq!(frame.bytes.len(), usize::from(length));
                    }
                } else {
                    assert_eq!(result, Err(CeError::InvalidFrame));
                }
            }
            Command::TxBuffer { length } => {
                let length = usize::from(length).max(1);
                let mut buffer = CeTxBuffer::allocate(&device, length).unwrap();
                operations.borrow_mut().clear();
                buffer.write(&vec![0x33; length]).unwrap();
                buffer.descriptor(1, false).unwrap();
                assert!(
                    matches!(operations.borrow().as_slice(), [Operation::SyncForDevice { range, .. }] if range == &(0..length))
                );
            }
            Command::RxBuffer { length } => {
                let length = usize::from(length).max(1);
                let mut buffer = CeRxBuffer::allocate(&device, 256).unwrap();
                buffer.descriptor().unwrap();
                operations.borrow_mut().clear();
                assert_eq!(buffer.complete(length).unwrap().len(), length);
                assert!(
                    matches!(operations.borrow().as_slice(), [Operation::SyncForCpu { range, .. }] if range == &(0..256))
                );
            }
        }

        let endpoint = transport.htc().endpoint(1).unwrap();
        assert_eq!(endpoint.tx_credits, credits);
        assert_eq!(endpoint.sequence, sequence);
        assert_eq!(transport.io.sent.len(), emitted);
        for (_, transfer_id, frame) in &transport.io.sent {
            assert_eq!(*transfer_id, 1);
            assert_eq!(HtcHeader::decode(frame).unwrap().endpoint, 1);
        }
    }
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(stateful_cases()))]

    #[test]
    fn htc_and_dma_command_sequences_keep_accounting_consistent(
        commands in command_sequence(stateful_max_steps())
    ) {
        run_commands(&commands);
    }
}
