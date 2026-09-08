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

fn command() -> impl Strategy<Value = Command> {
    prop_oneof![
        5 => (0_u16..=700, any::<bool>()).prop_map(|(length, fail)| Command::Send { length, fail }),
        4 => (0_u8..=12, any::<u8>(), any::<bool>()).prop_map(
            |(endpoint, amount, short)| Command::ReceiveCredit { endpoint, amount, short }
        ),
        4 => (0_u8..=12, 0_u16..=512, any::<bool>()).prop_map(
            |(endpoint, length, short)| Command::ReceivePayload { endpoint, length, short }
        ),
        2 => any::<u8>().prop_map(|length| Command::TxBuffer { length }),
        2 => any::<u8>().prop_map(|length| Command::RxBuffer { length }),
    ]
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
    #![proptest_config(ProptestConfig::with_cases(128))]

    #[test]
    fn htc_and_dma_command_sequences_keep_accounting_consistent(
        commands in proptest::collection::vec(command(), 1..=64)
    ) {
        run_commands(&commands);
    }
}
