use ath11k_dp::tx::{ClientTxConfig, EncapType, client_tx_command_info};
use ath11k_hal::descriptors::*;
use ath11k_hal::{
    Descriptor, HalError, PacketNumberType, ReoCommand, ReoCommandKind, ReoCommandParams,
    ReoQueueDescriptor, ReoResources, ReoStatus, ReoStatusKind, RingMemory,
    initialize_command_ring, setup_wcn6750_io,
};
use ath11k_oracle as _;
use ath11k_platform_backend::{Bidirectional, FromDevice, ToDevice};
use drv_hardware_backends::DeterministicBackend;
use proptest::prelude::*;

unsafe extern "C" {
    fn oracle_hal_tx_setup(
        out: *mut u8,
        address: u64,
        metadata: u16,
        id: u32,
        kind: u8,
        encap: u8,
        encrypt: u8,
        len: u32,
        offset: u32,
        flags0: u32,
        flags1: u32,
        addr_flags: u16,
        ast_index: u32,
        ast_hash: u16,
        tid: u8,
        search: u8,
        lmac: u8,
        dscp: u8,
        mesh: u8,
        manager: u8,
    );
    fn oracle_dp_client_tx(
        out: *mut u8,
        address: u64,
        msdu_id: u32,
        length: u32,
        manager: u8,
        pool_id: u8,
        mac_id: u8,
        lmac_id: u8,
        metadata: u16,
        encap: u8,
        address_search: u8,
        search_type: u8,
        ast_index: u16,
        ast_hash: u8,
        tid: u8,
        checksum_offload: u8,
    );
    fn oracle_hal_dscp_tid_map(table: *const u8, out: *mut u8);
    fn oracle_hal_rx_buffer(out: *mut u8, address: u64, cookie: u32, manager: u8);
    fn oracle_hal_rx_buffer_get(
        input: *const u8,
        address: *mut u64,
        cookie: *mut u32,
        manager: *mut u8,
    );
    fn oracle_hal_reo_entrance(
        out: *mut u8,
        address: u64,
        cookie: u32,
        manager: u8,
        msdus: u8,
        queue: u64,
        bytes: u16,
        destination: u8,
        frameless: u8,
        reason: u8,
        error: u8,
        ring: u8,
        looping: u8,
    );
    fn oracle_hal_reo_destination(
        out: *mut u8,
        address: u64,
        cookie: u32,
        manager: u8,
        queue: u64,
        kind: u8,
        reason: u8,
        error: u8,
        rxq: u16,
        valid: u8,
        opcode: u8,
        slot: u8,
        ring: u8,
        looping: u8,
    );
    fn oracle_hal_wbm_release(
        out: *mut u8,
        address: u64,
        cookie: u32,
        manager: u8,
        source: u8,
        action: u8,
        kind: u8,
        index: u8,
        tqm_reason: u8,
        rx_reason: u8,
        rx_error: u8,
        reo_reason: u8,
        reo_error: u8,
        internal: u8,
        status: u32,
        count: u8,
        rssi: u8,
        valid: u8,
        first: u8,
        last: u8,
        amsdu: u8,
        notification: u8,
        timestamp: u32,
        peer: u16,
        tid: u8,
        ring: u8,
        looping: u8,
    );
    fn oracle_hal_wbm_msdu_link(out: *mut u8, source: *const u8, action: u8);
    fn oracle_hal_rx_msdu_link_info(
        input: *const u8,
        count: *mut u8,
        cookies: *mut u32,
        manager: *mut u8,
    );
    fn oracle_hal_link_descriptor(out: *mut u8, address: u64, cookie: u32);
    fn oracle_hal_ce_source(out: *mut u8, address: u64, len: u32, id: u32, swap: u8);
    fn oracle_hal_ce_destination(out: *mut u8, address: u64);
    fn oracle_hal_ce_status_take_length(inout: *mut u8) -> u32;
    fn oracle_hal_rx_wcn6750_fields(
        mpdu: *mut u8,
        peer: u16,
        length: u16,
        duration: *mut u8,
        usecs: u32,
    );
    fn oracle_hal_reo_queue_stats(out: *mut u8, number: u16, address: u64, flags: u32);
    fn oracle_hal_reo_flush_cache(
        out: *mut u8,
        number: u16,
        address: u64,
        flags: u32,
        available: u8,
        current: *mut u8,
    ) -> i32;
    fn oracle_hal_reo_update_rx_queue(
        out: *mut u8,
        number: u16,
        address: u64,
        flags: u32,
        update0: u32,
        update1: u32,
        update2: u32,
        pn: *const u32,
        rxq: u16,
        ba_window: u16,
        pn_size: u8,
    );
    fn oracle_hal_reo_status(
        input: *const u8,
        number: *mut u16,
        execution_time: *mut u16,
        status: *mut u8,
        timestamp: *mut u32,
    ) -> i32;
    fn oracle_hal_reo_queue_descriptor(
        out: *mut u8,
        tid: u8,
        ba_window: u32,
        start_sequence: u32,
        pn_type: u8,
    ) -> u32;
    fn oracle_hal_reo_init_cmd_ring(out: *mut u8, entries: u16, entry_bytes: u16);
    fn oracle_hal_wcn6750_reo_setup(
        kind: *mut u8,
        offset: *mut u32,
        value: *mut u32,
        general: u32,
        misc: u32,
    );
}

fn c_buffer(address: u64, cookie: u32, manager: u8) -> [u8; 8] {
    let mut out = [0; 8];
    // SAFETY: all arguments are typed values and `out` has the exact C layout size.
    unsafe { oracle_hal_rx_buffer(out.as_mut_ptr(), address, cookie, manager) };
    out
}

proptest! {
    #[test]
    fn tx_command_matches_pinned_c(
        metadata in any::<u16>(), id in 0u32..=0x1f_ffff, kind in 0u8..=1,
        encap in 0u8..=3, encrypt in 0u8..=15, len in any::<u16>(),
        offset in 0u32..=0x1ff, checksum_flags in 0u32..=0x3f,
        timestamp in 0u32..=0x7ffff, timestamp_valid in any::<bool>(),
        tid_overwrite in any::<bool>(), addr_flags in 0u16..=3,
        ast_index in 0u16..=u16::MAX, ast_hash in 0u16..=15,
        tid in 0u8..=15, search in 0u8..=3, lmac in 0u8..=3,
        dscp in 0u8..=63, mesh in any::<bool>(), manager in 0u8..=7,
    ) {
        let device = DeterministicBackend::device();
        let dma = device.alloc_coherent::<ToDevice>(1, 1).unwrap();
        let address = dma.device_address(0).unwrap();
        let flags0 = checksum_flags << 16;
        let flags1 = timestamp | (u32::from(timestamp_valid) << 19) |
            (u32::from(tid_overwrite) << 21);
        let info = TxCommandInfo { metadata_flags: metadata, descriptor_id: id,
            descriptor_type: kind, encapsulation_type: encap, data_length: u32::from(len),
            packet_offset: offset, encryption_type: encrypt, flags0, flags1,
            address_search_flags: addr_flags, bss_ast_hash: ast_hash,
            bss_ast_index: ast_index, tid, search_type: search, lmac_id: lmac,
            dscp_tid_table: dscp, mesh_enable: mesh, return_buffer_manager: manager };
        let rust = TclDataCommand::for_transmit(&address, info);
        let mut c = [0; 28];
        // SAFETY: valid field values and exact output size.
        unsafe { oracle_hal_tx_setup(c.as_mut_ptr(), address.bits(), metadata, id, kind,
            encap, encrypt, u32::from(len), offset, flags0, flags1, addr_flags,
            u32::from(ast_index), ast_hash, tid, search, lmac, dscp, mesh.into(), manager) };
        prop_assert_eq!(rust.as_bytes(), &c);
    }

    #[test]
    fn client_tx_field_selection_matches_pinned_c(
        msdu_id in 0u32..=32_766, data_length in any::<u32>(),
        manager in 0u8..=7, pool_id in 0u8..=3, mac_id in 0u8..=3,
        lmac_id in 0u8..=3, metadata in any::<u16>(), encap in 0u8..=2,
        address_search in 0u8..=3, search_type in 0u8..=3,
        ast_index in any::<u16>(), ast_hash in 0u8..=15, tid in 0u8..=15,
        checksum_offload in any::<bool>(),
    ) {
        let encapsulation = match encap {
            0 => EncapType::Raw,
            1 => EncapType::NativeWifi,
            _ => EncapType::Ethernet,
        };
        let config = ClientTxConfig {
            return_buffer_manager: manager, pool_id, mac_id, lmac_id, metadata,
            encapsulation, address_search_enable: address_search, search_type,
            ast_index, ast_hash, tid, checksum_offload,
        };
        let device = DeterministicBackend::device();
        let dma = device.alloc_coherent::<ToDevice>(1, 1).unwrap();
        let address = dma.device_address(0).unwrap();
        let rust = TclDataCommand::for_transmit(&address,
            client_tx_command_info(msdu_id, data_length, config));
        let mut c = [0; 28];
        // SAFETY: valid high-level DP fields and exact output size.
        unsafe { oracle_dp_client_tx(c.as_mut_ptr(), address.bits(), msdu_id, data_length,
            manager, pool_id, mac_id, lmac_id, metadata, encap, address_search,
            search_type, ast_index, ast_hash, tid, checksum_offload.into()) };
        prop_assert_eq!(rust.as_bytes(), &c);
    }

    #[test]
    fn dscp_tid_map_matches_pinned_c(table in any::<[u8; 64]>()) {
        let rust = dscp_tid_map_words(&table);
        let rust = rust.map(u32::to_le_bytes).concat();
        let mut c = [0; 24];
        // SAFETY: exact 64-byte input and 24-byte output.
        unsafe { oracle_hal_dscp_tid_map(table.as_ptr(), c.as_mut_ptr()) };
        prop_assert_eq!(rust, c);
    }

    #[test]
    fn reo_queue_stats_command_matches_pinned_c(
        number in any::<u16>(), flags in any::<u32>(),
    ) {
        let device = DeterministicBackend::device();
        let dma = device.alloc_coherent::<ToDevice>(1, 1).unwrap();
        let address = dma.device_address(0).unwrap();
        let mut resources = ReoResources::default();
        let rust = ReoCommand::encode(number, ReoCommandKind::QueueStats, &address,
            ReoCommandParams { flags, ..Default::default() }, &mut resources).unwrap();
        let mut c = [0; 40];
        // SAFETY: exact output size and typed scalar arguments.
        unsafe { oracle_hal_reo_queue_stats(c.as_mut_ptr(), number, address.bits(), flags) };
        let rust = rust.into_descriptor();
        prop_assert_eq!(rust.bytes(), &c);
    }

    #[test]
    fn reo_flush_cache_command_matches_pinned_c(
        number in any::<u16>(), flags in any::<u32>(), available in 0u8..=7,
        current in 0u8..=2,
    ) {
        let device = DeterministicBackend::device();
        let dma = device.alloc_coherent::<ToDevice>(1, 1).unwrap();
        let address = dma.device_address(0).unwrap();
        let mut resources = ReoResources {
            available_block_resources: available,
            current_block_index: current,
        };
        let rust = ReoCommand::encode(number, ReoCommandKind::FlushCache, &address,
            ReoCommandParams { flags, ..Default::default() }, &mut resources);
        let mut c = [0; 40];
        let mut c_current = current;
        // SAFETY: exact output size, valid current pointer, and typed scalar arguments.
        let c_result = unsafe { oracle_hal_reo_flush_cache(c.as_mut_ptr(), number,
            address.bits(), flags, available, &mut c_current) };
        prop_assert_eq!(rust.is_ok(), c_result == 0);
        if let Ok(rust) = rust {
            let rust = rust.into_descriptor();
            prop_assert_eq!(rust.bytes(), &c);
            prop_assert_eq!(resources.current_block_index, c_current);
        }
    }

    #[test]
    fn reo_update_rx_queue_command_matches_pinned_c(
        number in any::<u16>(), flags in any::<u32>(), update0 in any::<u32>(),
        update1 in any::<u32>(), update2 in any::<u32>(), pn in any::<[u32; 4]>(),
        rxq in any::<u16>(), ba_window in any::<u16>(), pn_size in any::<u8>(),
    ) {
        let device = DeterministicBackend::device();
        let dma = device.alloc_coherent::<ToDevice>(1, 1).unwrap();
        let address = dma.device_address(0).unwrap();
        let params = ReoCommandParams { flags, update0, update1, update2, pn,
            rx_queue_number: rxq, ba_window_size: ba_window, pn_size };
        let mut resources = ReoResources::default();
        let rust = ReoCommand::encode(number, ReoCommandKind::UpdateRxQueue, &address,
            params, &mut resources).unwrap();
        let mut c = [0; 40];
        // SAFETY: exact output size, four-element PN input, and typed scalar arguments.
        unsafe { oracle_hal_reo_update_rx_queue(c.as_mut_ptr(), number, address.bits(),
            flags, update0, update1, update2, pn.as_ptr(), rxq, ba_window, pn_size) };
        let rust = rust.into_descriptor();
        prop_assert_eq!(rust.bytes(), &c);
    }

    #[test]
    fn reo_status_matches_pinned_c(
        mut bytes in any::<[u8; 104]>(), tag in 0u16..=511, number in any::<u16>(),
        execution_time in 0u16..=0x3ff, status in 0u8..=3, timestamp in any::<u32>(),
    ) {
        let tlv = u32::from(tag) << 1 | 100 << 10;
        let header = u32::from(number) | u32::from(execution_time) << 16 |
            u32::from(status) << 26;
        bytes[0..4].copy_from_slice(&tlv.to_le_bytes());
        bytes[4..8].copy_from_slice(&header.to_le_bytes());
        bytes[8..12].copy_from_slice(&timestamp.to_le_bytes());
        let descriptor = Descriptor::new(bytes.to_vec(), bytes.len()).unwrap();
        let rust = ReoStatus::decode(&descriptor);
        let (mut c_number, mut c_execution_time, mut c_status, mut c_timestamp) =
            (0, 0, 0, 0);
        // SAFETY: exact input size and valid writable scalar outputs.
        let c_tag = unsafe { oracle_hal_reo_status(bytes.as_ptr(), &mut c_number,
            &mut c_execution_time, &mut c_status, &mut c_timestamp) };
        match rust {
            Ok(rust) => {
                let rust_tag = match rust.kind {
                    ReoStatusKind::UpdateRxQueue => 153,
                    ReoStatusKind::QueueStats => 311,
                    ReoStatusKind::FlushQueue => 312,
                    ReoStatusKind::FlushCache => 313,
                    ReoStatusKind::UnblockCache => 314,
                    ReoStatusKind::FlushTimeoutList => 342,
                    ReoStatusKind::DescriptorThreshold => 345,
                };
                prop_assert_eq!(rust_tag, c_tag);
                prop_assert_eq!(rust.header.command_number, c_number);
                prop_assert_eq!(rust.header.execution_time_us, c_execution_time);
                prop_assert_eq!(rust.header.execution_status, c_status);
                prop_assert_eq!(rust.header.timestamp, c_timestamp);
            }
            Err(error) => {
                prop_assert_eq!(error, HalError::Unsupported);
                prop_assert_eq!(c_tag, -1);
            }
        }
    }

    #[test]
    fn reo_queue_descriptor_matches_pinned_c(
        tid in 0u8..=16, ba_window in any::<u32>(),
        start_sequence in any::<u32>(), pn_type in 0u8..=3,
    ) {
        let pn = match pn_type {
            0 => PacketNumberType::None,
            1 => PacketNumberType::Wpa,
            2 => PacketNumberType::WapiEven,
            _ => PacketNumberType::WapiUneven,
        };
        let rust = ReoQueueDescriptor::new(tid, ba_window, start_sequence, pn);
        let mut c = [0; 512];
        // SAFETY: exact output size and typed scalar arguments.
        let c_length = unsafe { oracle_hal_reo_queue_descriptor(c.as_mut_ptr(), tid,
            ba_window, start_sequence, pn_type) } as usize;
        prop_assert_eq!(rust.bytes().len(), c_length);
        prop_assert_eq!(rust.bytes(), &c[..c_length]);
    }

    #[test]
    fn reo_command_ring_initialization_matches_pinned_c(
        entries in 1u16..=102, initial in any::<u8>(),
    ) {
        const ENTRY_BYTES: u16 = 40;
        let device = DeterministicBackend::device();
        let byte_count = usize::from(entries) * usize::from(ENTRY_BYTES);
        let mut memory = RingMemory {
            dma: device.alloc_coherent::<Bidirectional>(byte_count, 8).unwrap(),
            entries,
            entry_bytes: ENTRY_BYTES,
        };
        let initial = vec![initial; byte_count];
        memory.dma.write(0, &initial).unwrap();
        initialize_command_ring(&mut memory).unwrap();
        let mut rust = vec![0; byte_count];
        memory.dma.read(0, &mut rust).unwrap();
        let mut c = initial;
        // SAFETY: output holds exactly entries * entry_bytes bytes.
        unsafe { oracle_hal_reo_init_cmd_ring(c.as_mut_ptr(), entries, ENTRY_BYTES) };
        prop_assert_eq!(rust, c);
    }

    #[test]
    fn wcn6750_reo_setup_matches_pinned_c(general in any::<u32>(), misc in any::<u32>()) {
        use std::cell::RefCell;
        const GENERAL: usize = 0x00a3_8000;
        let rust = RefCell::new(Vec::new());
        setup_wcn6750_io(
            |offset| {
                let value = if offset == GENERAL { general } else { misc };
                rust.borrow_mut().push((0_u8, offset as u32, value));
                Ok(value)
            },
            |offset, value| {
                rust.borrow_mut().push((1_u8, offset as u32, value));
                Ok(())
            },
        ).unwrap();
        let mut kind = [0; 10];
        let mut offset = [0; 10];
        let mut value = [0; 10];
        // SAFETY: each output has the exact ten-element C layout.
        unsafe { oracle_hal_wcn6750_reo_setup(kind.as_mut_ptr(), offset.as_mut_ptr(),
            value.as_mut_ptr(), general, misc) };
        let c: Vec<_> = (0..10).map(|index| (kind[index], offset[index], value[index])).collect();
        prop_assert_eq!(rust.into_inner(), c);
    }

    #[test]
    fn rx_buffer_setup_and_parse_match_pinned_c(cookie in 0u32..=0x1f_ffff, manager in 0u8..=7) {
        let device = DeterministicBackend::device();
        let dma = device.alloc_coherent::<FromDevice>(1, 1).unwrap();
        let address = dma.device_address(0).unwrap();
        let rust = RxdmaBufferRing::for_buffer(&address, cookie, manager);
        let c = c_buffer(address.bits(), cookie, manager);
        prop_assert_eq!(rust.as_bytes(), &c);
        let (mut c_address, mut c_cookie, mut c_manager) = (0, 0, 0);
        // SAFETY: exact input and valid writable scalar outputs.
        unsafe { oracle_hal_rx_buffer_get(c.as_ptr(), &mut c_address, &mut c_cookie, &mut c_manager) };
        let info = rust.info();
        prop_assert_eq!((info.address, info.cookie, info.return_buffer_manager),
            (c_address, c_cookie, c_manager));
    }

    #[test]
    fn reo_entrance_and_destination_parse_pinned_c(
        address in 0u64..(1u64 << 40), cookie in 0u32..=0x1f_ffff, manager in 0u8..=7,
        msdus in any::<u8>(), queue in 0u64..(1u64 << 40), bytes in 0u16..=0x3fff,
        destination in 0u8..=31, frameless in any::<bool>(), reason in 0u8..=3,
        error in 0u8..=31, kind in 0u8..=1, rxq in any::<u16>(), valid in any::<bool>(),
        opcode in 0u8..=15, slot in any::<u8>(), ring in any::<u8>(), looping in 0u8..=15,
    ) {
        let mut entrance = [0; 32];
        unsafe { oracle_hal_reo_entrance(entrance.as_mut_ptr(), address, cookie, manager,
            msdus, queue, bytes, destination, frameless.into(), reason, error, ring, looping) };
        let rust = ReoEntranceRing::from_bytes(&entrance).unwrap();
        let parsed = rust.received_buffer();
        prop_assert_eq!((parsed.info.address, parsed.info.cookie, parsed.info.return_buffer_manager,
            parsed.msdu_count, rust.queue_address(), rust.mpdu_byte_count(), rust.reo_destination(),
            rust.frameless_bar(), rust.rxdma_push_reason(), rust.rxdma_error_code(),
            rust.ring_id(), rust.looping_count()),
            (address, cookie, manager, msdus, queue, bytes, destination, frameless, reason,
             error, ring, looping));

        let mut destination_bytes = [0; 64];
        unsafe { oracle_hal_reo_destination(destination_bytes.as_mut_ptr(), address, cookie,
            manager, queue, kind, reason, error, rxq, valid.into(), opcode, slot, ring, looping) };
        let rust = ReoDestinationRing::from_bytes(&destination_bytes).unwrap();
        let buffer = rust.buffer_address().info();
        let trace = [buffer.address, u64::from(buffer.cookie), u64::from(buffer.return_buffer_manager),
            rust.queue_address(), u64::from(rust.buffer_type()), u64::from(rust.push_reason()),
            u64::from(rust.error_code()), u64::from(rust.rx_queue_number()),
            u64::from(rust.reorder_info_valid()), u64::from(rust.reorder_opcode()),
            u64::from(rust.reorder_slot()), u64::from(rust.ring_id()), u64::from(rust.looping_count())];
        prop_assert_eq!(trace, [address, u64::from(cookie), u64::from(manager), queue,
            u64::from(kind), u64::from(reason), u64::from(error), u64::from(rxq),
            u64::from(valid), u64::from(opcode), u64::from(slot), u64::from(ring),
            u64::from(looping)]);
    }

    #[test]
    fn wbm_release_fields_and_link_setup_match_pinned_c(
        address in 0u64..(1u64 << 40), cookie in 0u32..=0x1f_ffff, manager in 0u8..=7,
        source in 0u8..=7, action in 0u8..=7, kind in 0u8..=7, index in 0u8..=15,
        tqm_reason in 0u8..=15, rx_reason in 0u8..=3, rx_error in 0u8..=31,
        reo_reason in 0u8..=3, reo_error in 0u8..=31, internal in any::<bool>(),
        status in 0u32..=0xff_ffff, count in 0u8..=0x7f, rssi in any::<u8>(),
        valid in any::<bool>(), first in any::<bool>(), last in any::<bool>(),
        amsdu in any::<bool>(), notification in any::<bool>(), timestamp in 0u32..=0x7ffff,
        peer in any::<u16>(), tid in 0u8..=15, ring in any::<u8>(), looping in 0u8..=15,
    ) {
        let mut c = [0; 32];
        unsafe { oracle_hal_wbm_release(c.as_mut_ptr(), address, cookie, manager, source,
            action, kind, index, tqm_reason, rx_reason, rx_error, reo_reason, reo_error,
            internal.into(), status, count, rssi, valid.into(), first.into(), last.into(),
            amsdu.into(), notification.into(), timestamp, peer, tid, ring, looping) };
        let rust = WbmReleaseRing::from_bytes(&c).unwrap();
        let buffer = rust.buffer_address().info();
        prop_assert_eq!((buffer.address, buffer.cookie, buffer.return_buffer_manager),
            (address, cookie, manager));
        let trace = [u64::from(rust.release_source()), u64::from(rust.buffer_manager_action()),
            u64::from(rust.descriptor_type()), u64::from(rust.first_msdu_index()),
            u64::from(rust.tqm_release_reason()), u64::from(rust.rxdma_push_reason()),
            u64::from(rust.rxdma_error_code()), u64::from(rust.reo_push_reason()),
            u64::from(rust.reo_error_code()), u64::from(rust.internal_error()),
            u64::from(rust.tqm_status_number()), u64::from(rust.transmit_count()),
            u64::from(rust.ack_rssi()), u64::from(rust.software_release_details_valid()),
            u64::from(rust.first_msdu()), u64::from(rust.last_msdu()), u64::from(rust.msdu_in_amsdu()),
            u64::from(rust.firmware_tx_notification()), u64::from(rust.buffer_timestamp()),
            u64::from(rust.peer_id()), u64::from(rust.tid()), u64::from(rust.ring_id()),
            u64::from(rust.looping_count())];
        prop_assert_eq!(trace, [u64::from(source), u64::from(action), u64::from(kind),
            u64::from(index), u64::from(tqm_reason), u64::from(rx_reason), u64::from(rx_error),
            u64::from(reo_reason), u64::from(reo_error), u64::from(internal), u64::from(status),
            u64::from(count), u64::from(rssi), u64::from(valid), u64::from(first), u64::from(last),
            u64::from(amsdu), u64::from(notification), u64::from(timestamp), u64::from(peer),
            u64::from(tid), u64::from(ring), u64::from(looping)]);
        let mut linked = [0; 32];
        unsafe { oracle_hal_wbm_msdu_link(linked.as_mut_ptr(), c.as_ptr(), action) };
        let rust_linked = WbmReleaseRing::for_msdu_link(&rust, action);
        prop_assert_eq!(rust_linked.as_bytes(), &linked);
    }

    #[test]
    fn rx_msdu_link_info_matches_pinned_c(
        mut bytes in any::<[u8; 128]>(), stop in 0usize..=6,
    ) {
        for index in 0..stop {
            bytes[32 + index * 16] |= 1;
        }
        if stop < 6 {
            bytes[32 + stop * 16..36 + stop * 16].fill(0);
        }
        let rust = RxMsduLink::from_bytes(&bytes).unwrap().info();
        let mut count = 0;
        let mut cookies = [0; 6];
        let mut manager = 0;
        // SAFETY: exact input size and valid writable scalar/array outputs.
        unsafe { oracle_hal_rx_msdu_link_info(bytes.as_ptr(), &mut count,
            cookies.as_mut_ptr(), &mut manager) };
        prop_assert_eq!(rust.count, count);
        prop_assert_eq!(rust.cookies, cookies);
        prop_assert_eq!(rust.return_buffer_manager, manager);
    }

    #[test]
    fn link_descriptor_address_matches_pinned_c(
        offset in 0usize..4096, cookie in any::<u32>(),
    ) {
        let device = DeterministicBackend::device();
        let dma = device.alloc_coherent::<ToDevice>(4096, 8).unwrap();
        let address = dma.device_address(offset).unwrap();
        let rust = WbmLinkDescriptor::new_at(&address, cookie);
        let mut c = [0; 8];
        // SAFETY: exact output size and typed scalar arguments.
        unsafe { oracle_hal_link_descriptor(c.as_mut_ptr(), address.bits(), cookie) };
        prop_assert_eq!(rust.as_bytes(), &c);
    }

    #[test]
    fn ce_descriptor_setup_and_status_parse_match_pinned_c(
        len in any::<u32>(), id in any::<u32>(), swap in any::<bool>(), status_len in any::<u16>(),
        status_low in 0u32..=0xffff,
    ) {
        let device = DeterministicBackend::device();
        let tx = device.alloc_coherent::<ToDevice>(1, 1).unwrap();
        let tx_address = tx.device_address(0).unwrap();
        let mut c_source = [0; 16];
        unsafe { oracle_hal_ce_source(c_source.as_mut_ptr(), tx_address.bits(), len, id, swap.into()) };
        let rust_source = CeSourceDescriptor::for_transfer(&tx_address, len, id, swap);
        prop_assert_eq!(rust_source.as_bytes(), &c_source);

        let rx = device.alloc_coherent::<FromDevice>(1, 1).unwrap();
        let rx_address = rx.device_address(0).unwrap();
        let mut c_destination = [0; 8];
        unsafe { oracle_hal_ce_destination(c_destination.as_mut_ptr(), rx_address.bits()) };
        let rust_destination = CeDestinationDescriptor::from_address(&rx_address);
        prop_assert_eq!(rust_destination.as_bytes(), &c_destination);

        let mut bytes = [0; 16];
        bytes[..4].copy_from_slice(&(status_low | (u32::from(status_len) << 16)).to_le_bytes());
        let mut rust = CeDestinationStatusDescriptor::from_bytes(&bytes).unwrap();
        let c_len = unsafe { oracle_hal_ce_status_take_length(bytes.as_mut_ptr()) };
        prop_assert_eq!(u32::from(rust.take_length()), c_len);
        prop_assert_eq!(rust.as_bytes(), &bytes);
    }

    #[test]
    fn wcn6750_rx_field_offsets_match_pinned_c(peer in any::<u16>(), length in 0u16..=0x3fff,
        duration in 0u32..=0xff_ffff) {
        let (mut mpdu, mut time) = ([0; 92], [0; 56]);
        unsafe { oracle_hal_rx_wcn6750_fields(mpdu.as_mut_ptr(), peer, length,
            time.as_mut_ptr(), duration) };
        let rust_mpdu = RxMpduInfoWcn6750::from_bytes(&mpdu).unwrap();
        let rust_duration = RxPpduEndDuration::from_bytes(&time).unwrap();
        prop_assert_eq!((rust_mpdu.peer_id(), rust_mpdu.mpdu_length()), (peer, length));
        prop_assert_eq!(rust_duration.duration(), duration);
    }
}
