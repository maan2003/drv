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

    #[repr(C, align(4))]
    #[derive(Clone, Copy)]
    struct CTxwi([u8; 64]);

    impl Default for CTxwi {
        fn default() -> Self {
            Self([0; 64])
        }
    }

    #[repr(C)]
    #[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
    struct CConnac2Rx {
        payload_offset: u32,
        channel: u8,
        signal: i8,
        has_pn: bool,
        pn: [u8; 6],
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
        fn oracle_client_data_txwi(
            payload_len: u16,
            payload_iova: u32,
            token: u16,
            pid: u8,
            eapol: bool,
            protected_frame: bool,
            qos: bool,
            tid: u8,
            output: *mut u8,
        ) -> i32;
        fn oracle_client_management_txwi(
            frame_len: u16,
            frame_iova: u32,
            token: u16,
            pid: u8,
            subtype: u8,
            output: *mut u8,
        ) -> i32;
        fn oracle_connac2_rx_frame(
            input: *const u8,
            input_len: usize,
            output: *mut CConnac2Rx,
        ) -> i32;
        fn oracle_passive_hw_scan(scan_sequence: u8, band: u8, channel: u8, output: *mut u8)
        -> i32;
        fn oracle_cancel_hw_scan(scan_sequence: u8, output: *mut u8) -> i32;
        fn oracle_client_bss(
            bss_index: u8,
            bssid: *const u8,
            channel: u16,
            beacon_interval: u16,
            dtim_period: u8,
            qos: bool,
            enable: bool,
            output: *mut u8,
        ) -> i32;
        fn oracle_initial_peer_sta(
            bss_index: u8,
            wcid: u8,
            peer: *const u8,
            output: *mut u8,
        ) -> i32;
        fn oracle_key_v2(
            bss_index: u8,
            wcid: u8,
            muar_index: u8,
            key_id: u8,
            key: *const u8,
            retained: bool,
            retained_id: u8,
            retained_key: *const u8,
            remove: bool,
            output: *mut u8,
        ) -> i32;
    }

    fn c_fill(payload: &[u8], command: i32, sequence: u8) -> Vec<u8> {
        // The C wrapper reserves the kernel path's maximum 64-byte headroom;
        // a UNI command actually consumes 48 bytes and reports that length.
        let mut output = vec![0; payload.len() + 64];
        // SAFETY: both slices remain live and expose their exact lengths.
        let length = unsafe {
            oracle_mcu_fill(
                payload.as_ptr(),
                payload.len(),
                command,
                sequence,
                output.as_mut_ptr(),
                output.len(),
            )
        };
        assert!(length >= 0);
        output.truncate(length as usize);
        output
    }

    fn c_passive_scan(scan_sequence: u8, band: u8, channel: u8, sequence: u8) -> Vec<u8> {
        let mut payload = [0; 1186];
        // SAFETY: `payload` is live and has the exact size required by C.
        assert_eq!(
            unsafe { oracle_passive_hw_scan(scan_sequence, band, channel, payload.as_mut_ptr()) },
            0
        );
        c_fill(&payload, (1 << 18) | 0x03, sequence)
    }

    fn c_cancel_scan(scan_sequence: u8, sequence: u8) -> Vec<u8> {
        let mut payload = [0; 4];
        // SAFETY: `payload` is live and has the exact size required by C.
        assert_eq!(
            unsafe { oracle_cancel_hw_scan(scan_sequence, payload.as_mut_ptr()) },
            0
        );
        c_fill(&payload, (1 << 18) | 0x1b, sequence)
    }

    #[allow(clippy::too_many_arguments)]
    fn c_bss(
        bss_index: u8,
        bssid: [u8; 6],
        channel: u16,
        beacon_interval: u16,
        dtim_period: u8,
        qos: bool,
        enable: bool,
        sequence: u8,
    ) -> Vec<u8> {
        let mut payload = [0; 44];
        // SAFETY: both arrays remain live and have the exact sizes required by C.
        assert_eq!(
            unsafe {
                oracle_client_bss(
                    bss_index,
                    bssid.as_ptr(),
                    channel,
                    beacon_interval,
                    dtim_period,
                    qos,
                    enable,
                    payload.as_mut_ptr(),
                )
            },
            0
        );
        c_fill(&payload, (1 << 17) | 2, sequence)
    }

    fn c_initial_sta(bss_index: u8, wcid: u8, peer: [u8; 6], sequence: u8) -> Vec<u8> {
        let mut payload = [0; 40];
        // SAFETY: both arrays remain live and have the exact sizes required by C.
        assert_eq!(
            unsafe {
                oracle_initial_peer_sta(bss_index, wcid, peer.as_ptr(), payload.as_mut_ptr())
            },
            0
        );
        c_fill(&payload, (1 << 17) | 3, sequence)
    }

    #[allow(clippy::too_many_arguments)]
    fn c_key(
        bss_index: u8,
        wcid: u8,
        muar_index: u8,
        key_id: u8,
        key: [u8; 16],
        retained: Option<(u8, [u8; 16])>,
        remove: bool,
        sequence: u8,
    ) -> Vec<u8> {
        let mut payload = [0; 88];
        let (retained_id, retained_key) = retained.unwrap_or_default();
        // SAFETY: all arrays remain live and have the exact sizes required by C.
        assert_eq!(
            unsafe {
                oracle_key_v2(
                    bss_index,
                    wcid,
                    muar_index,
                    key_id,
                    key.as_ptr(),
                    retained.is_some(),
                    retained_id,
                    retained_key.as_ptr(),
                    remove,
                    payload.as_mut_ptr(),
                )
            },
            0
        );
        c_fill(&payload, (1 << 17) | 3, sequence)
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

    struct DataTxwiInput {
        payload_len: u16,
        payload_iova: u32,
        token: u16,
        pid: u8,
        eapol: bool,
        protected: bool,
        qos: bool,
        tid: u8,
    }

    fn c_data_txwi(input: DataTxwiInput) -> [u8; 64] {
        let mut output = CTxwi::default();
        // SAFETY: `output` is live, 4-byte aligned, and exactly 64 bytes. The
        // source-exact C wrapper writes only those bytes from scalar inputs.
        let result = unsafe {
            oracle_client_data_txwi(
                input.payload_len,
                input.payload_iova,
                input.token,
                input.pid,
                input.eapol,
                input.protected,
                input.qos,
                input.tid,
                output.0.as_mut_ptr(),
            )
        };
        assert_eq!(result, 0);
        output.0
    }

    fn c_management_txwi(
        frame_len: u16,
        frame_iova: u32,
        token: u16,
        pid: u8,
        subtype: u8,
    ) -> [u8; 64] {
        let mut output = CTxwi::default();
        // SAFETY: `output` is live, 4-byte aligned, and exactly 64 bytes. The
        // source-exact C wrapper writes only those bytes from scalar inputs.
        let result = unsafe {
            oracle_client_management_txwi(
                frame_len,
                frame_iova,
                token,
                pid,
                subtype,
                output.0.as_mut_ptr(),
            )
        };
        assert_eq!(result, 0);
        output.0
    }

    fn c_connac2_rx(bytes: &[u8]) -> CConnac2Rx {
        let mut output = CConnac2Rx::default();
        // SAFETY: both objects remain live for the call, and the wrapper
        // bounds-checks every variable group before reading it.
        let result = unsafe { oracle_connac2_rx_frame(bytes.as_ptr(), bytes.len(), &mut output) };
        assert_eq!(result, 0);
        output
    }

    struct RxEnvelope<'a> {
        packet_type: u32,
        channel: u8,
        group4: bool,
        group1: Option<[u8; 16]>,
        group2: bool,
        group5: bool,
        remove_pad: u8,
        rcpi: [u8; 2],
        frame: &'a [u8],
    }

    fn connac2_rx_envelope(input: RxEnvelope<'_>) -> Vec<u8> {
        let metadata_len = 24
            + usize::from(input.group4) * 16
            + usize::from(input.group1.is_some()) * 16
            + usize::from(input.group2) * 8
            + 8
            + usize::from(input.group5) * 72
            + usize::from(input.remove_pad) * 2;
        let mut bytes = vec![0; metadata_len + input.frame.len()];
        let packet_flag = u32::from(input.packet_type == 7);
        let rxd0 = (bytes.len() as u32) | (packet_flag << 16) | (input.packet_type << 27);
        bytes[0..4].copy_from_slice(&rxd0.to_le_bytes());
        let groups = (u32::from(input.group1.is_some()) << 11)
            | (u32::from(input.group2) << 12)
            | (1 << 13)
            | (u32::from(input.group4) << 14)
            | (u32::from(input.group5) << 15);
        bytes[4..8].copy_from_slice(&groups.to_le_bytes());
        bytes[8..12].copy_from_slice(&(u32::from(input.remove_pad) << 14).to_le_bytes());
        bytes[12..16].copy_from_slice(&(u32::from(input.channel) << 8).to_le_bytes());
        let mut offset = 24;
        if input.group4 {
            offset += 16;
        }
        if let Some(group1) = input.group1 {
            bytes[offset..offset + 16].copy_from_slice(&group1);
            offset += 16;
        }
        if input.group2 {
            offset += 8;
        }
        bytes[offset + 4..offset + 6].copy_from_slice(&input.rcpi);
        offset += 8;
        if input.group5 {
            bytes[offset + 24..offset + 26].copy_from_slice(&input.rcpi);
            offset += 72;
        }
        offset += usize::from(input.remove_pad) * 2;
        bytes[offset..].copy_from_slice(input.frame);
        bytes
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
        fn client_data_txwi_and_txp_match_source_assignments(
            payload_len in 1u16..=0x0fff,
            payload_iova in 0u32..=u32::MAX - 0x0fff,
            token in 0u16..8192,
            pid in 3u8..127,
            eapol: bool,
            protected_input: bool,
            qos: bool,
            tid in 0u8..=7,
        ) {
            let protected = protected_input || !eapol;
            let rust = mt7921_core::encode_client_data_txwi(
                usize::from(payload_len),
                u64::from(payload_iova),
                token,
                pid,
                eapol,
                protected,
                qos,
                tid,
            ).unwrap();
            prop_assert_eq!(rust, c_data_txwi(DataTxwiInput {
                payload_len, payload_iova, token, pid, eapol, protected, qos, tid
            }));
        }

        #[test]
        fn client_management_txwi_txp_and_dma_match_source_assignments(
            frame_len in 24u16..=0x0fff,
            txwi_iova in 0u32..=u32::MAX - 64,
            frame_iova in 0u32..=u32::MAX - 0x0fff,
            token in 0u16..8192,
            pid in 3u8..127,
            subtype in 0u8..=15,
        ) {
            let mut frame = vec![0; usize::from(frame_len)];
            frame[0..2].copy_from_slice(&(u16::from(subtype) << 4).to_le_bytes());
            let rust = mt7921_core::encode_client_management_tx(
                &frame, u64::from(txwi_iova), u64::from(frame_iova), token, pid).unwrap();
            let c = c_management_txwi(
                frame_len, frame_iova, token, pid, subtype);
            prop_assert_eq!(rust.txwi, c);
            prop_assert_eq!(words(rust.descriptor.to_le_bytes()),
                c_dma((u64::from(txwi_iova), 64), None, 0));
        }

        #[test]
        fn connac2_normal_rx_frames_match_source_group_walk(
            packet_type in prop_oneof![Just(2u32), Just(7u32)],
            channel in prop::sample::select(vec![1u8, 6, 11, 14, 36, 52, 100, 144, 165]),
            group4: bool,
            has_group1: bool,
            group1: [u8; 16],
            group2: bool,
            group5: bool,
            remove_pad in 0u8..=3,
            rcpi0 in 0u8..=219,
            rcpi1 in 0u8..=219,
            frame in proptest::collection::vec(any::<u8>(), 2..=256),
        ) {
            let group1 = has_group1.then_some(group1);
            let bytes = connac2_rx_envelope(RxEnvelope {
                packet_type, channel, group4, group1, group2, group5,
                remove_pad, rcpi: [rcpi0, rcpi1], frame: &frame,
            });
            let c = c_connac2_rx(&bytes);
            let rust = mt7921_core::parse_connac2_rx_frame(&bytes).unwrap();
            prop_assert_eq!(c.payload_offset as usize + frame.len(), bytes.len());
            prop_assert_eq!(rust.bytes, frame);
            prop_assert_eq!(rust.channel, c.channel);
            prop_assert_eq!(rust.rssi_dbm, c.signal);
            prop_assert_eq!(rust.pn, group1.map(|pn| [pn[5], pn[4], pn[3], pn[2], pn[1], pn[0]]));
        }

        #[test]
        fn connac2_auth_rx_matches_source_group_walk_and_frame_fields(
            packet_type in prop_oneof![Just(2u32), Just(7u32)],
            channel in prop::sample::select(vec![1u8, 11, 36, 100, 165]),
            group4: bool,
            has_group1: bool,
            group1: [u8; 16],
            group2: bool,
            group5: bool,
            remove_pad in 0u8..=3,
            rcpi0 in 0u8..=219,
            rcpi1 in 0u8..=219,
            receiver: [u8; 6],
            transmitter: [u8; 6],
            bssid: [u8; 6],
            algorithm: u16,
            sequence: u16,
            status: u16,
            fields in proptest::collection::vec(any::<u8>(), 0..=128),
        ) {
            let mut frame = vec![0; 30 + fields.len()];
            frame[0..2].copy_from_slice(&0x00b0u16.to_le_bytes());
            frame[4..10].copy_from_slice(&receiver);
            frame[10..16].copy_from_slice(&transmitter);
            frame[16..22].copy_from_slice(&bssid);
            frame[24..26].copy_from_slice(&algorithm.to_le_bytes());
            frame[26..28].copy_from_slice(&sequence.to_le_bytes());
            frame[28..30].copy_from_slice(&status.to_le_bytes());
            frame[30..].copy_from_slice(&fields);
            let bytes = connac2_rx_envelope(RxEnvelope {
                packet_type,
                channel,
                group4,
                group1: has_group1.then_some(group1),
                group2,
                group5,
                remove_pad,
                rcpi: [rcpi0, rcpi1],
                frame: &frame,
            });
            let c = c_connac2_rx(&bytes);
            let stripped = mt7921_core::parse_connac2_rx_frame(&bytes).unwrap();
            let auth = mt7921_core::parse_mt7921_auth_rx(&bytes).unwrap();
            prop_assert_eq!(c.payload_offset as usize + frame.len(), bytes.len());
            prop_assert_eq!(stripped.bytes, frame);
            prop_assert_eq!(auth.receiver, receiver);
            prop_assert_eq!(auth.transmitter, transmitter);
            prop_assert_eq!(auth.bssid, bssid);
            prop_assert_eq!(auth.algorithm, algorithm);
            prop_assert_eq!(auth.sequence, sequence);
            prop_assert_eq!(auth.status, status);
            prop_assert_eq!(auth.fields, fields);
        }

        #[test]
        fn passive_hw_scan_commands_match_c_assignments(
            sequence in 1u8..=15,
            scan_sequence in 0u8..=0x7f,
            channel in prop::sample::select(vec![
                (mt7921_core::PhysicalBand::Ghz2, 1u16, 2412u16, 1u8),
                (mt7921_core::PhysicalBand::Ghz2, 14, 2484, 1),
                (mt7921_core::PhysicalBand::Ghz5, 36, 5180, 2),
                (mt7921_core::PhysicalBand::Ghz5, 100, 5500, 2),
                (mt7921_core::PhysicalBand::Ghz5, 165, 5825, 2),
            ]),
        ) {
            let candidate = mt7921_core::CandidateChannel {
                band: channel.0,
                number: channel.1,
                frequency_mhz: channel.2,
            };
            let start = mt7921_core::encode_passive_mcu_command(
                &mt7921_core::PassiveMcuCommand::StartScan {
                    scan_sequence,
                    channel: candidate,
                },
                sequence,
            ).unwrap();
            prop_assert_eq!(start, c_passive_scan(scan_sequence, channel.3, channel.1 as u8, sequence));
            let cancel = mt7921_core::encode_passive_mcu_command(
                &mt7921_core::PassiveMcuCommand::CancelScan { scan_sequence },
                sequence,
            ).unwrap();
            prop_assert_eq!(cancel, c_cancel_scan(scan_sequence, sequence));
        }

        #[test]
        fn client_bss_info_matches_c_assignments(
            sequence in 1u8..=15,
            bss_index: u8,
            bssid: [u8; 6],
            channel in 1u16..=177,
            beacon_interval in 1u16..=u16::MAX,
            dtim_period in 1u8..=u8::MAX,
            qos: bool,
            enable: bool,
        ) {
            prop_assume!(bssid != [0; 6]);
            let rust = mt7921_core::encode_client_bss_command(
                sequence, bss_index, bssid, channel, beacon_interval,
                dtim_period, qos, enable,
            ).unwrap();
            let mut c = c_bss(
                bss_index, bssid, channel, beacon_interval, dtim_period,
                qos, enable, sequence,
            );
            if !enable {
                prop_assert_eq!(rust[56], 0);
                prop_assert_eq!(c[56], 1);
                c[56] = rust[56];
            }
            prop_assert_eq!(rust, c);
        }

        #[test]
        fn initial_peer_sta_rec_matches_c_assignments(
            sequence in 1u8..=15,
            bss_index: u8,
            wcid in 1u8..=u8::MAX,
            peer: [u8; 6],
        ) {
            prop_assume!(peer != [0; 6]);
            let rust = mt7921_core::encode_initial_peer_wcid_command(
                sequence, bss_index, wcid, peer,
            ).unwrap();
            prop_assert_eq!(rust, c_initial_sta(bss_index, wcid, peer, sequence));
        }

        #[test]
        fn key_v2_install_and_disable_match_c_assignments(
            sequence in 1u8..=15,
            bss_index: u8,
            wcid: u8,
            muar_index: u8,
            key_id: u8,
            key: [u8; 16],
            retained_id: u8,
            retained_key: [u8; 16],
        ) {
            let rust = mt7921_core::encode_key_v2_command(
                sequence, bss_index, wcid, muar_index, key_id, &key, None,
            ).unwrap();
            prop_assert_eq!(rust.as_bytes(), c_key(
                bss_index, wcid, muar_index, key_id, key, None, false, sequence));

            let rust = mt7921_core::encode_key_v2_command(
                sequence, bss_index, wcid, muar_index, key_id, &key,
                Some((retained_id, &retained_key)),
            ).unwrap();
            let mut c = c_key(
                bss_index, wcid, muar_index, key_id, key,
                Some((retained_id, retained_key)), false, sequence,
            );
            if key_id != 0 {
                prop_assert_eq!(rust.as_bytes()[102], 0);
                prop_assert_eq!(c[102], key_id);
                c[102] = 0;
            }
            prop_assert_eq!(rust.as_bytes(), c);

            let rust = mt7921_core::encode_disable_keys_command(
                sequence, bss_index, wcid, muar_index,
            ).unwrap();
            prop_assert_eq!(rust.as_bytes(), c_key(
                bss_index, wcid, muar_index, 0, [0; 16], None, true, sequence));
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
