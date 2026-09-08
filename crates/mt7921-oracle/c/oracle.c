_Static_assert(sizeof(struct mt76_connac2_mcu_txd) == 64,
               "legacy MCU TXD layout changed");
_Static_assert(sizeof(struct mt76_desc) == 16, "DMA descriptor layout changed");
_Static_assert(sizeof(struct mt76_connac2_mcu_rxd) == 36,
               "MCU RXD layout changed");

struct oracle_mcu_response {
    int32_t result;
    uint32_t payload_offset;
    uint16_t length;
    uint16_t packet_type;
    uint8_t event_id;
    uint8_t sequence;
    uint8_t option;
    uint8_t extended_event_id;
};

int oracle_mcu_parse_response(const uint8_t *input, size_t input_len, int cmd,
                              int sequence, struct oracle_mcu_response *out)
{
    uint8_t storage[4096];
    struct sk_buff skb;
    struct mt76_dev dev = {0};
    struct mt76_connac2_mcu_rxd *rxd;
    if (!input || !out || input_len < sizeof(*rxd) || input_len > sizeof(storage))
        return -1;
    memcpy(storage, input, input_len);
    skb.data = storage;
    skb.len = input_len;
    rxd = (struct mt76_connac2_mcu_rxd *)skb.data;
    out->length = le16_to_cpu(rxd->len);
    out->packet_type = le16_to_cpu(rxd->pkt_type_id);
    out->event_id = rxd->eid;
    out->sequence = rxd->seq;
    out->option = rxd->option;
    out->extended_event_id = rxd->ext_eid;
    out->result = mt7921_mcu_parse_response(&dev, cmd, &skb, sequence);
    out->payload_offset = (uint32_t)(skb.data - storage);
    return 0;
}

int oracle_mcu_fill(const uint8_t *payload, size_t payload_len, int cmd,
                    uint8_t sequence, uint8_t *out, size_t out_len)
{
    uint8_t storage[4096];
    struct sk_buff skb;
    struct mt76_dev dev = {0};
    int wait_seq = 0;
    if (!sequence || sequence > 15 || payload_len + 64 > sizeof(storage) ||
        out_len < payload_len + 64)
        return -1;
    memcpy(storage + 64, payload, payload_len);
    skb.data = storage + 64;
    skb.len = payload_len;
    dev.mcu.msg_seq = sequence - 1;
    if (mt76_connac2_mcu_fill_message(&dev, &skb, cmd, &wait_seq))
        return -2;
    if (wait_seq != sequence)
        return -3;
    memcpy(out, skb.data, skb.len);
    return (int)skb.len;
}

int oracle_dma_descriptor(uint64_t addr0, uint16_t len0, bool has_second,
                          uint64_t addr1, uint16_t len1, uint32_t info,
                          struct mt76_desc *out)
{
    struct mt76_desc desc = {0};
    struct mt76_queue_entry entry = {0};
    struct mt76_queue q = { .head = 0, .ndesc = 1, .desc = &desc, .entry = &entry };
    struct mt76_queue_buf buf[2] = {
        { .addr = addr0, .len = len0 }, { .addr = addr1, .len = len1 }
    };
    int result = mt76_dma_add_buf(NULL, &q, buf, has_second ? 2 : 1, info, NULL, NULL);
    if (result < 0)
        return result;
    *out = desc;
    return 0;
}

int oracle_dma_rx_descriptor(uint64_t address, uint16_t length,
                             struct mt76_desc *out)
{
    struct mt76_desc desc = {0};
    struct mt76_queue_entry entry = {0};
    struct mt76_queue q = { .head = 0, .ndesc = 1, .desc = &desc, .entry = &entry };
    struct mt76_queue_buf buf = { .addr = address, .len = length };
    int result = mt76_dma_add_rx_buf(NULL, &q, &buf, NULL);
    if (result < 0)
        return result;
    *out = desc;
    return 0;
}

/* Source-exact assignments from mt76_connac2_mac_write_txwi and its 802.3 /
 * 802.11 helpers.  The kernel functions themselves require mac80211 skb,
 * station, vif, key and PHY objects, so the wrapper fixes those coupled inputs
 * to the MT7921 client seam and exposes only values accepted by its Rust API. */
int oracle_client_data_txwi(uint16_t payload_len, uint32_t payload_iova,
                            uint16_t token, uint8_t pid, bool eapol,
                            bool protected_frame, bool qos, uint8_t tid,
                            uint8_t out[64])
{
    uint32_t *txwi = (uint32_t *)out;
    uint8_t subtype = qos ? 8 : 0;
    memset(out, 0, 64);
    txwi[0] = cpu_to_le32(FIELD_PREP(MT_TXD0_TX_BYTES, payload_len + 32) |
                            FIELD_PREP(MT_TXD0_PKT_FMT, MT_TX_TYPE_CT) |
                            FIELD_PREP(MT_TXD0_Q_IDX, eapol ? 3 : 1));
    txwi[1] = cpu_to_le32(MT_TXD1_LONG_FORMAT |
                            FIELD_PREP(MT_TXD1_WLAN_IDX, 7));
    txwi[3] = cpu_to_le32(FIELD_PREP(MT_TXD3_REM_TX_COUNT, 15) |
                            (protected_frame ? MT_TXD3_PROTECT_FRAME : 0));
    txwi[5] = cpu_to_le32(FIELD_PREP(MT_TXD5_PID, pid) |
                            MT_TXD5_TX_STATUS_HOST);
    if (eapol) {
        txwi[1] |= cpu_to_le32(FIELD_PREP(MT_TXD1_HDR_FORMAT, MT_HDR_FORMAT_802_11) |
                                 FIELD_PREP(MT_TXD1_HDR_INFO, qos ? 13 : 12) |
                                 FIELD_PREP(MT_TXD1_TID, qos ? tid : 0));
        txwi[2] |= cpu_to_le32(FIELD_PREP(MT_TXD2_FRAME_TYPE, 2) |
                                 FIELD_PREP(MT_TXD2_SUB_TYPE, subtype) |
                                 MT_TXD2_FIX_RATE);
        txwi[7] |= cpu_to_le32(FIELD_PREP(MT_TXD7_TYPE, 2) |
                                 FIELD_PREP(MT_TXD7_SUB_TYPE, subtype));
        txwi[2] |= cpu_to_le32(MT_TXD2_HTC_VLD);
        txwi[6] |= cpu_to_le32(FIELD_PREP(MT_TXD6_TX_RATE, 0x4b) |
                                 MT_TXD6_FIXED_BW);
        txwi[3] |= cpu_to_le32(MT_TXD3_BA_DISABLE);
    } else {
        txwi[1] |= cpu_to_le32(FIELD_PREP(MT_TXD1_HDR_FORMAT, MT_HDR_FORMAT_802_3) |
                                 FIELD_PREP(MT_TXD1_TID, tid) | MT_TXD1_ETH_802_3);
        txwi[2] |= cpu_to_le32(FIELD_PREP(MT_TXD2_FRAME_TYPE, 2) |
                                 FIELD_PREP(MT_TXD2_SUB_TYPE, 8));
        txwi[7] |= cpu_to_le32(FIELD_PREP(MT_TXD7_TYPE, 2) |
                                 FIELD_PREP(MT_TXD7_SUB_TYPE, 8));
    }
    ((uint16_t *)(out + 32))[0] = cpu_to_le16(token | MT_MSDU_ID_VALID);
    ((uint32_t *)(out + 40))[0] = cpu_to_le32(payload_iova);
    ((uint16_t *)(out + 44))[0] = cpu_to_le16(payload_len | MT_TXD_LEN_LAST);
    return 0;
}

int oracle_client_management_txwi(uint16_t frame_len, uint32_t frame_iova,
                                  uint16_t token, uint8_t pid,
                                  uint8_t subtype, uint8_t out[64])
{
    uint32_t *txwi = (uint32_t *)out;
    memset(out, 0, 64);
    txwi[0] = cpu_to_le32(FIELD_PREP(MT_TXD0_TX_BYTES, frame_len + 32) |
                            FIELD_PREP(MT_TXD0_PKT_FMT, MT_TX_TYPE_CT) |
                            FIELD_PREP(MT_TXD0_Q_IDX, MT_LMAC_ALTX0));
    txwi[1] = cpu_to_le32(MT_TXD1_LONG_FORMAT |
                            FIELD_PREP(MT_TXD1_WLAN_IDX, 19) |
                            FIELD_PREP(MT_TXD1_HDR_FORMAT, MT_HDR_FORMAT_802_11) |
                            FIELD_PREP(MT_TXD1_HDR_INFO, 12));
    txwi[2] = cpu_to_le32(FIELD_PREP(MT_TXD2_SUB_TYPE, subtype) |
                            MT_TXD2_FIX_RATE | MT_TXD2_HTC_VLD);
    txwi[3] = cpu_to_le32(FIELD_PREP(MT_TXD3_REM_TX_COUNT, 15) |
                            MT_TXD3_BA_DISABLE);
    txwi[5] = cpu_to_le32(FIELD_PREP(MT_TXD5_PID, pid) |
                            MT_TXD5_TX_STATUS_HOST);
    txwi[6] = cpu_to_le32(FIELD_PREP(MT_TXD6_TX_RATE, 0x4b) |
                            MT_TXD6_FIXED_BW);
    txwi[7] = cpu_to_le32(FIELD_PREP(MT_TXD7_SUB_TYPE, subtype));
    ((uint16_t *)(out + 32))[0] = cpu_to_le16(token | MT_MSDU_ID_VALID);
    ((uint32_t *)(out + 40))[0] = cpu_to_le32(frame_iova);
    ((uint16_t *)(out + 44))[0] = cpu_to_le16(frame_len | MT_TXD_LEN_LAST);
    return 0;
}

struct oracle_connac2_rx {
    uint32_t payload_offset;
    uint8_t channel;
    int8_t signal;
    bool has_pn;
    uint8_t pn[6];
};

/* Normalized subset of mt7921_mac_fill_rx: the exact RXD group walk, channel,
 * two-chain signal selection, GROUP1 IV assignment, and final skb_pull offset.
 * Device/station bookkeeping and radiotap state are intentionally omitted. */
int oracle_connac2_rx_frame(const uint8_t *input, size_t input_len,
                            struct oracle_connac2_rx *out)
{
    uint32_t rxd[6], rxd1, rxd2, rxd3, rxv[2], rcpi;
    size_t offset = 24;
    int8_t signal0, signal1;
    if (!input || !out || input_len < 24)
        return -1;
    memcpy(rxd, input, sizeof(rxd));
    rxd1 = le32_to_cpu(rxd[1]);
    rxd2 = le32_to_cpu(rxd[2]);
    rxd3 = le32_to_cpu(rxd[3]);
    memset(out, 0, sizeof(*out));
    out->channel = FIELD_GET(MT_RXD3_NORMAL_CH_FREQ, rxd3);
    if (rxd1 & MT_RXD1_NORMAL_GROUP_4)
        offset += 16;
    if (rxd1 & MT_RXD1_NORMAL_GROUP_1) {
        if (offset + 16 > input_len)
            return -1;
        out->has_pn = true;
        out->pn[0] = input[offset + 5]; out->pn[1] = input[offset + 4];
        out->pn[2] = input[offset + 3]; out->pn[3] = input[offset + 2];
        out->pn[4] = input[offset + 1]; out->pn[5] = input[offset + 0];
        offset += 16;
    }
    if (rxd1 & MT_RXD1_NORMAL_GROUP_2)
        offset += 8;
    if (!(rxd1 & MT_RXD1_NORMAL_GROUP_3) || offset + 8 > input_len)
        return -1;
    memcpy(rxv, input + offset, sizeof(rxv));
    rcpi = le32_to_cpu(rxv[1]);
    offset += 8;
    if (rxd1 & MT_RXD1_NORMAL_GROUP_5) {
        if (offset + 72 > input_len)
            return -1;
        memcpy(&rcpi, input + offset + 24, sizeof(rcpi));
        rcpi = le32_to_cpu(rcpi);
        offset += 72;
    }
    signal0 = (FIELD_GET(MT_PRXV_RCPI0, rcpi) - 220) / 2;
    signal1 = (FIELD_GET(MT_PRXV_RCPI1, rcpi) - 220) / 2;
    out->signal = -128;
    if (signal0 < 0 && signal0 > out->signal)
        out->signal = signal0;
    if (signal1 < 0 && signal1 > out->signal)
        out->signal = signal1;
    offset += 2 * FIELD_GET(MT_RXD2_NORMAL_HDR_OFFSET, rxd2);
    if (offset > input_len)
        return -1;
    out->payload_offset = offset;
    return 0;
}
