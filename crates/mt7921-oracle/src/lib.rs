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
    }
}
