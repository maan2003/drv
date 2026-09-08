use ath11k_hal::descriptors::*;

#[test]
fn tcl_data_command_is_byte_exact_and_checked() {
    let mut addr = RxdmaBufferRing::from_bytes(&[0x67, 0x45, 0x23, 0x01, 0xab, 0, 0, 0]).unwrap();
    addr.set_return_buffer_manager(5).unwrap();
    addr.set_software_cookie(0x1a_bcde).unwrap();

    let mut cmd = TclDataCommand::new();
    cmd.set_buffer_address(&addr);
    cmd.set_encapsulation_type(2).unwrap();
    cmd.set_search_type(1).unwrap();
    cmd.set_address_search_enable(3).unwrap();
    cmd.set_command_number(0x1234).unwrap();
    cmd.set_data_length(0x5678).unwrap();
    cmd.set_ipv4_checksum(true);
    cmd.set_to_firmware(true);
    cmd.set_packet_offset(0x101).unwrap();
    cmd.set_buffer_timestamp(0x45678).unwrap();
    cmd.set_buffer_timestamp_valid(true);
    cmd.set_tid_overwrite(true);
    cmd.set_tid(9).unwrap();
    cmd.set_lmac_id(2).unwrap();
    cmd.set_dscp_tid_table(0x2a).unwrap();
    cmd.set_search_index(0x8_7654).unwrap();
    cmd.set_cache_set(0xd).unwrap();
    cmd.set_ring_id(0x5a).unwrap();
    cmd.set_looping_count(0xc).unwrap();

    assert_eq!(
        cmd.as_bytes(),
        &[
            0x67, 0x45, 0x23, 0x01, 0xab, 0xf5, 0xe6, 0xd5, 0x08, 0xd0, 0x34, 0x12, 0x78, 0x56,
            0xa1, 0x80, 0x78, 0x56, 0x6c, 0x0a, 0x2a, 0x95, 0x1d, 0x36, 0x00, 0x00, 0xa0, 0xc5,
        ]
    );
    assert_eq!(
        cmd.set_encapsulation_type(4),
        Err(LayoutError::FieldValueOutOfRange)
    );
    assert_eq!(
        TclDataCommand::from_bytes(&[0; 27]),
        Err(LayoutError::WrongLength {
            expected: 28,
            actual: 27
        })
    );
}

#[test]
fn reo_and_rxdma_masks_land_in_oracle_words() {
    let mut mpdu = RxMpduDescriptor::new();
    mpdu.set_msdu_count(0x56).unwrap();
    mpdu.set_sequence_number(0xabc).unwrap();
    mpdu.set_fragment(true);
    mpdu.set_raw_mpdu(true);
    mpdu.set_peer_id(0x1234).unwrap();
    assert_eq!(mpdu.as_bytes(), &[0x56, 0xbc, 0x1a, 0x40, 0x34, 0x12, 0, 0]);

    let mut msdu = RxMsduDescriptor::new();
    msdu.set_first_in_mpdu(true);
    msdu.set_continuation(true);
    msdu.set_length(0x2345).unwrap();
    msdu.set_reo_destination(0x12).unwrap();
    msdu.set_drop(true);
    assert_eq!(msdu.as_bytes(), &[0x2d, 0x1a, 0x65, 0, 0, 0, 0, 0]);

    let mut bytes = [0_u8; ReoEntranceRing::LEN];
    bytes[16..20].copy_from_slice(&0x89ab_cdef_u32.to_le_bytes());
    bytes[20] = 0x7e;
    let mut entrance = ReoEntranceRing::from_bytes(&bytes).unwrap();
    entrance.set_mpdu_byte_count(0x2345).unwrap();
    entrance.set_reo_destination(0x12).unwrap();
    entrance.set_frameless_bar(true);
    entrance.set_rxdma_push_reason(2).unwrap();
    entrance.set_rxdma_error_code(0x13).unwrap();
    entrance.set_ring_id(0xa5).unwrap();
    entrance.set_looping_count(0xb).unwrap();
    assert_eq!(
        &entrance.as_bytes()[16..],
        &[
            0xef, 0xcd, 0xab, 0x89, 0x7e, 0x45, 0xa3, 0x0c, 0x4e, 0, 0, 0, 0, 0, 0x50, 0xba
        ]
    );

    let mut bytes = [0_u8; ReoDestinationRing::LEN];
    bytes[24..28].copy_from_slice(&0x1234_5678_u32.to_le_bytes());
    bytes[28] = 0x55;
    let mut dest = ReoDestinationRing::from_bytes(&bytes).unwrap();
    dest.set_buffer_type(1).unwrap();
    dest.set_push_reason(2).unwrap();
    dest.set_error_code(0x1d).unwrap();
    dest.set_rx_queue_number(0xbeef).unwrap();
    dest.set_reorder_info_valid(true);
    dest.set_reorder_opcode(0xa).unwrap();
    dest.set_reorder_slot(0x7f).unwrap();
    dest.set_ring_id(0x33).unwrap();
    dest.set_looping_count(0xe).unwrap();
    assert_eq!(
        &dest.as_bytes()[24..36],
        &[
            0x78, 0x56, 0x34, 0x12, 0x55, 0xed, 0xef, 0xbe, 0xf5, 0x0f, 0, 0
        ]
    );
    assert_eq!(&dest.as_bytes()[60..], &[0, 0, 0x30, 0xe3]);
}

#[test]
fn msdu_link_info_stops_at_first_zero_low_address() {
    let mut bytes = [0_u8; 128];
    // msdu_link starts at byte 32; each hal_rx_msdu_details is 16 bytes.
    bytes[32..36].copy_from_slice(&0x1234_u32.to_le_bytes());
    bytes[36..40].copy_from_slice(&(3_u32 << 8 | 0x4567_u32 << 11).to_le_bytes());
    bytes[48..52].copy_from_slice(&0x5678_u32.to_le_bytes());
    bytes[52..56].copy_from_slice(&(3_u32 << 8 | 0x89ab_u32 << 11).to_le_bytes());
    let link = RxMsduLink::from_bytes(&bytes).unwrap();
    let info = link.info();
    assert_eq!(info.count, 2);
    assert_eq!(info.return_buffer_manager, 3);
    assert_eq!(&info.cookies[..2], &[0x4567, 0x89ab]);
    assert_eq!(link.msdu(0).unwrap().buffer_address().address(), 0x1234);
    assert!(link.msdu(6).is_none());
}

#[test]
fn ce_and_wbm_layouts_are_little_endian() {
    let mut source = CeSourceDescriptor::from_bytes(&[
        0xef, 0xbe, 0xad, 0xde, 0x12, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
    ])
    .unwrap();
    source.set_hash_enable(true);
    source.set_gather(true);
    source.set_length(0x3456).unwrap();
    source.set_metadata(0x789a).unwrap();
    source.set_ring_id(0xbc).unwrap();
    source.set_looping_count(0xd).unwrap();
    assert_eq!(
        source.as_bytes(),
        &[
            0xef, 0xbe, 0xad, 0xde, 0x12, 0x09, 0x56, 0x34, 0x9a, 0x78, 0, 0, 0, 0, 0xc0, 0xdb
        ]
    );

    let mut destination =
        CeDestinationDescriptor::from_bytes(&[0x78, 0x56, 0x34, 0x12, 0xab, 0, 0, 0]).unwrap();
    destination.set_ring_id(0x45).unwrap();
    destination.set_looping_count(0xe).unwrap();
    assert_eq!(destination.address(), 0xab_1234_5678);
    assert_eq!(
        destination.as_bytes(),
        &[0x78, 0x56, 0x34, 0x12, 0xab, 0, 0x50, 0xe4]
    );

    let mut status = CeDestinationStatusDescriptor::new();
    status.set_hash_enable(true);
    status.set_destination_swap(true);
    status.set_length(0x1234).unwrap();
    status.set_toeplitz_hash(0x1122_3344_5566_7788);
    status.set_metadata(0x9abc).unwrap();
    status.set_ring_id(0xde).unwrap();
    status.set_looping_count(0xf).unwrap();
    assert_eq!(
        status.as_bytes(),
        &[
            0, 0x05, 0x34, 0x12, 0x88, 0x77, 0x66, 0x55, 0x44, 0x33, 0x22, 0x11, 0xbc, 0x9a, 0xe0,
            0xfd,
        ]
    );
    assert_eq!(status.toeplitz_hash(), 0x1122_3344_5566_7788);

    let mut release = WbmReleaseRing::new();
    release.set_release_source(4).unwrap();
    release.set_descriptor_type(5).unwrap();
    release.set_first_msdu_index(0xa).unwrap();
    release.set_internal_error(true);
    release.set_tqm_status_number(0xabcdef).unwrap();
    release.set_transmit_count(0x55).unwrap();
    release.set_peer_id(0x1234).unwrap();
    release.set_tid(9).unwrap();
    release.set_ring_id(0x67).unwrap();
    release.set_looping_count(0xe).unwrap();
    assert_eq!(
        &release.as_bytes()[8..16],
        &[0x44, 0x15, 0, 0x80, 0xef, 0xcd, 0xab, 0x55]
    );
    assert_eq!(&release.as_bytes()[28..], &[0x34, 0x12, 0x79, 0xe6]);
}

#[test]
fn rx_end_family_offsets_match_oracle() {
    let mut start = RxPpduStart::new();
    start.set_ppdu_id(0x1234).unwrap();
    start.set_channel_number_word(0x5566_7788);
    start.set_timestamp(0x99aa_bbcc);
    assert_eq!(
        start.as_bytes(),
        &[
            0x34, 0x12, 0, 0, 0x88, 0x77, 0x66, 0x55, 0xcc, 0xbb, 0xaa, 0x99
        ]
    );

    let mut mpdu = RxMpduInfoWcn6750::new();
    mpdu.set_peer_id(0x1234).unwrap();
    mpdu.set_mpdu_length(0x2345).unwrap();
    assert_eq!(&mpdu.as_bytes()[4..8], &[0, 0, 0x34, 0x12]);
    assert_eq!(&mpdu.as_bytes()[52..56], &[0x45, 0x23, 0, 0]);

    let mut stats = RxPpduEndUserStats::new();
    stats.set_mpdu_fcs_error_count(0x155).unwrap();
    stats.set_mpdu_fcs_ok_count(0x101).unwrap();
    stats.set_frame_control_valid(true);
    stats.set_packet_type(0xa).unwrap();
    stats.set_frame_control(0x8899).unwrap();
    stats.set_ast_index(0x6677).unwrap();
    stats.set_udp_msdu_count(0x1122).unwrap();
    stats.set_tcp_msdu_count(0x3344).unwrap();
    stats.set_mpdu_ok_byte_count(0x123456).unwrap();
    assert_eq!(
        &stats.as_bytes()[8..20],
        &[
            0, 0, 0x55, 0x01, 0x01, 0x03, 0xa0, 0, 0x77, 0x66, 0x99, 0x88
        ]
    );
    assert_eq!(&stats.as_bytes()[36..40], &[0x22, 0x11, 0x44, 0x33]);
    assert_eq!(&stats.as_bytes()[68..72], &[0x56, 0x34, 0x12, 0]);

    let mut duration = RxPpduEndDuration::new();
    duration.set_duration(0xabcdef).unwrap();
    assert_eq!(&duration.as_bytes()[36..40], &[0xef, 0xcd, 0xab, 0]);
    assert_eq!(
        duration.set_duration(0x0100_0000),
        Err(LayoutError::FieldValueOutOfRange)
    );
}

#[test]
fn monitor_tlv_header_uses_linux_bit_positions() {
    let mut header = RxMonitorTlvHeader::new();
    header.set_tag(0x155).unwrap();
    header.set_length(0x4567).unwrap();
    header.set_user_id(0x2a).unwrap();
    assert_eq!(header.as_bytes(), &[0xaa, 0x9e, 0x15, 0xa9]);
    assert_eq!(header.tag(), 0x155);
    assert_eq!(header.length(), 0x4567);
    assert_eq!(header.user_id(), 0x2a);
}
