#![deny(unsafe_op_in_unsafe_fn)]

//! Differential test boundary for pinned Linux mt76/MT7921 format code.

#[cfg(test)]
mod tests {
    use mt76_core::DmaDescriptor as Mt76Descriptor;
    use mt7921_core::{DmaDescriptor as Mt7921Descriptor, DmaSegment};
    use mt7921_core::{DownloadCommand, encode_download_command};
    use proptest::prelude::*;

    #[repr(C)]
    #[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
    struct CDescriptor {
        buf0: u32,
        ctrl: u32,
        buf1: u32,
        info: u32,
    }

    #[repr(C)]
    #[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
    struct CMcuResponse {
        result: i32,
        payload_offset: u32,
        length: u16,
        packet_type: u16,
        event_id: u8,
        sequence: u8,
        option: u8,
        extended_event_id: u8,
    }

    unsafe extern "C" {
        fn oracle_mcu_fill(
            payload: *const u8,
            payload_len: usize,
            command: i32,
            sequence: u8,
            output: *mut u8,
            output_len: usize,
        ) -> i32;
        fn oracle_dma_descriptor(
            address0: u64,
            length0: u16,
            has_second: bool,
            address1: u64,
            length1: u16,
            info: u32,
            output: *mut CDescriptor,
        ) -> i32;
        fn oracle_dma_rx_descriptor(address: u64, length: u16, output: *mut CDescriptor) -> i32;
        fn oracle_mcu_parse_response(
            input: *const u8,
            input_len: usize,
            command: i32,
            sequence: i32,
            output: *mut CMcuResponse,
        ) -> i32;
    }

    fn command_id(command: DownloadCommand) -> i32 {
        const QUERY: i32 = 1 << 16;
        const CE: i32 = 1 << 18;
        match command {
            DownloadCommand::NicPowerControl => 0x04,
            DownloadCommand::GetNicCapability => CE | 0x8a,
            DownloadCommand::ReadEepromBlock { .. } => QUERY | (0x01 << 8) | 0xed,
            DownloadCommand::FirmwareLogToHost => CE | 0xc5,
            DownloadCommand::EepromBufferMode => (0x21 << 8) | 0xed,
            DownloadCommand::ProtectControl => (0x3e << 8) | 0xed,
            DownloadCommand::PatchSemaphoreGet | DownloadCommand::PatchSemaphoreRelease => 0x10,
            DownloadCommand::PatchFinish => 0x07,
            DownloadCommand::FirmwareStart { .. } => 0x02,
            DownloadCommand::PatchStart { .. } => 0x05,
            DownloadCommand::TargetAddressLength { .. } => 0x01,
        }
    }

    fn c_mcu(command: DownloadCommand, sequence: u8) -> Vec<u8> {
        let rust = encode_download_command(command, sequence).expect("valid typed command");
        let payload = &rust[mt7921_core::CONNAC2_MCU_TXD_BYTES..];
        let mut output = vec![0; rust.len()];
        // SAFETY: both slices remain live for the call and advertise their exact
        // lengths; `output` has the 64 bytes of headroom required by the C path.
        let length = unsafe {
            oracle_mcu_fill(
                payload.as_ptr(),
                payload.len(),
                command_id(command),
                sequence,
                output.as_mut_ptr(),
                output.len(),
            )
        };
        assert_eq!(length as usize, output.len());
        output
    }

    fn c_dma(first: (u64, u16), second: Option<(u64, u16)>, info: u32) -> CDescriptor {
        let mut output = CDescriptor::default();
        let (address1, length1) = second.unwrap_or_default();
        // SAFETY: `output` is a live, correctly aligned CDescriptor and all other
        // arguments are values. The extracted C function only writes that object.
        let result = unsafe {
            oracle_dma_descriptor(
                first.0,
                first.1,
                second.is_some(),
                address1,
                length1,
                info,
                &mut output,
            )
        };
        assert_eq!(result, 0);
        output
    }

    fn c_dma_rx(buffer: (u64, u16)) -> CDescriptor {
        let mut output = CDescriptor::default();
        // SAFETY: `output` is a live, correctly aligned CDescriptor. The
        // extracted RX function is configured for its ordinary non-WED path.
        let result = unsafe { oracle_dma_rx_descriptor(buffer.0, buffer.1, &mut output) };
        assert_eq!(result, 0);
        output
    }

    fn c_response(bytes: &[u8], command: DownloadCommand, sequence: u8) -> CMcuResponse {
        let mut output = CMcuResponse::default();
        // SAFETY: `bytes` and `output` remain live for the call and the C
        // wrapper checks the input length before executing the extracted path.
        let result = unsafe {
            oracle_mcu_parse_response(
                bytes.as_ptr(),
                bytes.len(),
                command_id(command),
                i32::from(sequence),
                &mut output,
            )
        };
        assert_eq!(result, 0);
        output
    }

    fn response_bytes(
        sequence: u8,
        event_id: u8,
        option: u8,
        extended_event_id: u8,
        body: &[u8],
    ) -> Vec<u8> {
        let mut bytes = vec![0; 36 + body.len()];
        bytes[24..26].copy_from_slice(&u16::try_from(12 + body.len()).unwrap().to_le_bytes());
        bytes[26..28].copy_from_slice(&0xa0u16.to_le_bytes());
        bytes[28] = event_id;
        bytes[29] = sequence;
        bytes[30] = option;
        bytes[32] = extended_event_id;
        bytes[36..].copy_from_slice(body);
        bytes
    }

    fn words(bytes: [u8; 16]) -> CDescriptor {
        CDescriptor {
            buf0: u32::from_le_bytes(bytes[0..4].try_into().unwrap()),
            ctrl: u32::from_le_bytes(bytes[4..8].try_into().unwrap()),
            buf1: u32::from_le_bytes(bytes[8..12].try_into().unwrap()),
            info: u32::from_le_bytes(bytes[12..16].try_into().unwrap()),
        }
    }

    proptest! {
        #[test]
        fn legacy_mcu_download_envelopes_match_c(
            sequence in 1u8..=15,
            address in 0u32..=u32::MAX,
            length in 1u32..=u32::MAX,
            mode: u32,
            eeprom_block in 0u32..=0x9f,
            release: bool,
        ) {
            let commands = [
                DownloadCommand::NicPowerControl,
                DownloadCommand::GetNicCapability,
                DownloadCommand::ReadEepromBlock { address: eeprom_block * 16 },
                DownloadCommand::FirmwareLogToHost,
                DownloadCommand::EepromBufferMode,
                DownloadCommand::ProtectControl,
                if release { DownloadCommand::PatchSemaphoreRelease } else { DownloadCommand::PatchSemaphoreGet },
                DownloadCommand::PatchFinish,
                DownloadCommand::FirmwareStart { address: 0x0091_5000, option: 1 },
                DownloadCommand::PatchStart { address: 0x0090_0000, length, mode },
                DownloadCommand::TargetAddressLength { address, length, mode },
            ];
            for command in commands {
                prop_assert_eq!(encode_download_command(command, sequence).unwrap(), c_mcu(command, sequence));
            }
        }

        #[test]
        fn mt76_dma_descriptors_match_c(
            address0 in 0u64..(1u64 << 36),
            length0 in 0u16..=0x3fff,
            address1 in 0u64..(1u64 << 36),
            length1 in 0u16..=0x3fff,
            info: u32,
            two_segments: bool,
        ) {
            let second = two_segments.then_some((address1, length1));
            let rust = Mt76Descriptor::tx((address0, length0), second, info).unwrap();
            prop_assert_eq!(words(rust.to_le_bytes()), c_dma((address0, length0), second, info));
            let rust = Mt76Descriptor::rx((address0, length0)).unwrap();
            prop_assert_eq!(words(rust.to_le_bytes()), c_dma_rx((address0, length0)));
        }

        #[test]
        fn mt7921_low32_dma_descriptors_match_c(
            address0: u32,
            length0 in 0u16..=0x3fff,
            address1: u32,
            length1 in 0u16..=0x3fff,
            info: u32,
            two_segments: bool,
        ) {
            let first = DmaSegment { iova: u64::from(address0), len: length0 };
            let second = two_segments.then_some(DmaSegment { iova: u64::from(address1), len: length1 });
            let rust = Mt7921Descriptor::tx(first, second, info).unwrap();
            prop_assert_eq!(words(rust.to_le_bytes()), c_dma(
                (first.iova, first.len), second.map(|s| (s.iova, s.len)), info));
            let rust = Mt7921Descriptor::rx(first).unwrap();
            prop_assert_eq!(words(rust.to_le_bytes()), c_dma_rx((u64::from(address0), length0)));
        }

        #[test]
        fn connac2_mcu_reply_envelopes_match_c(
            sequence in 1u8..=15,
            event_id: u8,
            option: u8,
            extended_event_id: u8,
            body in proptest::collection::vec(any::<u8>(), 0..=256),
        ) {
            let bytes = response_bytes(sequence, event_id, option, extended_event_id, &body);
            let rust = mt76_core::parse_download_response(&bytes, sequence).unwrap();
            for command in [
                DownloadCommand::GetNicCapability,
                DownloadCommand::ReadEepromBlock { address: 0 },
                DownloadCommand::EepromBufferMode,
                DownloadCommand::ProtectControl,
                DownloadCommand::FirmwareStart { address: 0x0091_5000, option: 1 },
                DownloadCommand::PatchStart { address: 0x0090_0000, length: 1, mode: 0 },
                DownloadCommand::TargetAddressLength { address: 0, length: 1, mode: 0 },
            ] {
                let c = c_response(&bytes, command, sequence);
                prop_assert_eq!(c.result, 0);
                prop_assert_eq!(c.payload_offset, 36);
                prop_assert_eq!(c.length, rust.length);
                prop_assert_eq!(c.packet_type, rust.packet_type);
                prop_assert_eq!(c.event_id, rust.event_id);
                prop_assert_eq!(c.sequence, rust.sequence);
                prop_assert_eq!(c.option, rust.option);
                prop_assert_eq!(c.extended_event_id, rust.extended_event_id);
            }
        }

        #[test]
        fn mt7921_patch_reply_scalars_match_c(
            sequence in 1u8..=15,
            result: u8,
            release: bool,
        ) {
            // Linux returns the first byte of the RXD's final word for these
            // commands rather than pulling the complete 36-byte header.
            let mut bytes = response_bytes(sequence, 4, 0, result, &[]);
            bytes[32] = result;
            let command = if release {
                DownloadCommand::PatchSemaphoreRelease
            } else {
                DownloadCommand::PatchSemaphoreGet
            };
            let c = c_response(&bytes, command, sequence);
            prop_assert_eq!(c.result, i32::from(result));
            prop_assert_eq!(c.payload_offset, 32);

            let c = c_response(&bytes, DownloadCommand::PatchFinish, sequence);
            prop_assert_eq!(c.result, i32::from(result));
            prop_assert_eq!(c.payload_offset, 32);
        }

        #[test]
        fn mt7921_eeprom_reply_payload_matches_c_boundary(
            sequence in 1u8..=15,
            block in 0u32..=0x9f,
            valid: u32,
            data: [u8; 16],
        ) {
            let address = block * 16;
            let mut body = Vec::with_capacity(24);
            body.extend_from_slice(&address.to_le_bytes());
            body.extend_from_slice(&valid.to_le_bytes());
            body.extend_from_slice(&data);
            let bytes = response_bytes(sequence, 1, 0, 0, &body);
            let c = c_response(&bytes, DownloadCommand::ReadEepromBlock { address }, sequence);
            prop_assert_eq!(c.payload_offset, 36);
            let rust = mt7921_core::parse_eeprom_block(&bytes[c.payload_offset as usize..], address).unwrap();
            prop_assert_eq!(rust.address, address);
            prop_assert_eq!(rust.valid, valid);
            prop_assert_eq!(rust.data, data);
        }
    }
}
