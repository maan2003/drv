_Static_assert(sizeof(struct mt76_connac2_mcu_txd) == 64,
               "legacy MCU TXD layout changed");
_Static_assert(sizeof(struct mt76_desc) == 16, "DMA descriptor layout changed");
_Static_assert(sizeof(struct mt76_connac2_mcu_rxd) == 36,
               "MCU RXD layout changed");

/* Normalized observations from the pinned mt7921_mcu_rx_event ->
 * mt7921_mcu_{,uni_}rx_unsolicited_event path and the four children exercised
 * by the Rust client seam.  These are source-exact field assignments: scan
 * and coredump retain the complete skb in Linux, while beacon loss and ROC
 * consume the fields below before freeing it. */
struct oracle_unsolicited_result {
    uint8_t kind;
    uint8_t queued;
    uint8_t freed;
    uint8_t connection_loss;
    uint8_t fw_assert;
    uint8_t reset_requested;
    uint8_t bss_index;
    uint8_t reason;
    uint8_t token;
    uint8_t status;
    uint8_t primary_channel;
    uint8_t band;
    uint8_t bandwidth;
    uint8_t center_channel;
    uint8_t request_type;
    uint32_t max_interval_ms;
};

int oracle_mt7921_unsolicited(const uint8_t *input, size_t input_len,
                              uint8_t active_bss, bool beacon_filter,
                              bool station,
                              struct oracle_unsolicited_result *out)
{
    const struct mt76_connac2_mcu_rxd *rxd;
    const uint8_t *body;

    if (!input || !out || input_len < sizeof(*rxd))
        return -1;
    memset(out, 0, sizeof(*out));
    rxd = (const struct mt76_connac2_mcu_rxd *)input;
    body = input + sizeof(*rxd);

    /* mt7921_mcu_rx_event gives the UNI option precedence over legacy EIDs. */
    if (rxd->option & BIT(2)) {
        uint32_t interval;
        const uint8_t *grant;
        if (rxd->eid != 0x27) {
            out->freed = 1;
            return 0;
        }
        if (input_len < sizeof(*rxd) + 4 + 20)
            return -2;
        grant = body + 4; /* rxd->tlv + UNI event header */
        out->kind = 3;
        out->bss_index = grant[4];
        out->token = grant[5];
        out->status = grant[6];
        out->primary_channel = grant[7];
        out->band = grant[9];
        out->bandwidth = grant[10];
        out->center_channel = grant[11];
        out->request_type = grant[13];
        memcpy(&interval, grant + 16, sizeof(interval));
        out->max_interval_ms = le32_to_cpu(interval);
        out->freed = 1;
        return 0;
    }

    switch (rxd->eid) {
    case 0x13: /* MCU_EVENT_BSS_BEACON_LOSS */
        if (input_len < sizeof(*rxd) + 4)
            return -2;
        out->kind = 1;
        out->bss_index = body[0];
        out->reason = body[1];
        out->connection_loss = body[0] == active_bss && beacon_filter && station;
        out->freed = 1;
        break;
    case 0x23: /* MCU_EVENT_SCHED_SCAN_DONE */
    case 0x0d: /* MCU_EVENT_SCAN_DONE */
        out->kind = 2;
        out->queued = 1;
        break;
    case 0xf0: /* MCU_EVENT_COREDUMP */
        out->kind = 4;
        out->queued = 1;
        out->fw_assert = 1;
        /* Reset occurs later in mt7921_coredump_work, never in RX context. */
        out->reset_requested = 0;
        break;
    default:
        out->freed = 1;
        break;
    }
    return 0;
}

struct oracle_txs_result {
    uint8_t skb_completed;
    uint8_t acked;
    uint8_t ampdu_len;
    uint8_t ampdu_ack_len;
    int32_t skb_rate_index;
    uint8_t polled;
    uint8_t rate_mcs;
    uint8_t rate_nss;
    uint8_t rate_flags;
    uint8_t rate_bw;
    uint8_t rate_he_gi;
    uint8_t rate_he_dcm;
    uint16_t rate_legacy;
};

/* Executes the pinned mt7921_mac_add_txs ->
 * mt76_connac2_mac_add_txs_skb -> mt76_connac2_mac_fill_txs path.  The
 * surrounding station, skb-status queue, and band tables are the minimum
 * state those source bodies read. */
int oracle_mt7921_add_txs(const uint32_t txs[8], bool pending_skb,
                          uint8_t band, uint8_t prior_rate_flags,
                          uint8_t prior_he_gi,
                          struct oracle_txs_result *out)
{
    struct mt792x_dev dev = {0};
    struct mt792x_link_sta link = {0};
    struct ieee80211_channel channel = { .band = band };
    struct sk_buff skb = {0};
    struct ieee80211_tx_info *info = IEEE80211_SKB_CB(&skb);
    unsigned int i;

    if (!txs || !out || band > NL80211_BAND_6GHZ)
        return -1;
    memset(out, 0, sizeof(*out));
    info->status.rates[0].idx = 0;
    link.wcid.sta = (void *)1;
    link.wcid.rate.flags = prior_rate_flags;
    link.wcid.rate.he_gi = prior_he_gi;
    dev.mt76.phy.dev = &dev.mt76;
    dev.mt76.phy.chandef.chan = &channel;
    for (i = 0; i < ARRAY_SIZE(dev.mt76.phy.sband_2g.sband.bitrates); i++) {
        dev.mt76.phy.sband_2g.sband.bitrates[i].bitrate = (i + 1) * 10;
        dev.mt76.phy.sband_5g.sband.bitrates[i].bitrate = (i + 1) * 10;
        dev.mt76.phy.sband_6g.sband.bitrates[i].bitrate = (i + 1) * 10;
    }
    dev.wcids[FIELD_GET(MT_TXS2_WCID, txs[2]) % MT792x_WTBL_SIZE] = &link;
    oracle_status_skb = pending_skb ? &skb : NULL;
    oracle_status_done = false;
    oracle_polled = false;

    mt7921_mac_add_txs(&dev, (__le32 *)txs);

    out->skb_completed = oracle_status_done;
    out->acked = !!(info->flags & IEEE80211_TX_STAT_ACK);
    out->ampdu_len = info->status.ampdu_len;
    out->ampdu_ack_len = info->status.ampdu_ack_len;
    out->skb_rate_index = info->status.rates[0].idx;
    out->polled = oracle_polled;
    out->rate_mcs = link.wcid.rate.mcs;
    out->rate_nss = link.wcid.rate.nss;
    out->rate_flags = link.wcid.rate.flags;
    out->rate_bw = link.wcid.rate.bw;
    out->rate_he_gi = link.wcid.rate.he_gi;
    out->rate_he_dcm = link.wcid.rate.he_dcm;
    out->rate_legacy = link.wcid.rate.legacy;
    oracle_status_skb = NULL;
    return 0;
}

/* Source-exact request assignments from mt76_connac_mcu_hw_scan and
 * mt76_connac_mcu_cancel_hw_scan.  The omitted mac80211 inputs are fixed to
 * the passive, single-channel shape exposed by PassiveMcuCommand. */
int oracle_passive_hw_scan(uint8_t scan_sequence, uint8_t band,
                           uint8_t channel, uint8_t out[1186])
{
    memset(out, 0, 1186);
    out[0] = scan_sequence;
    out[3] = BIT(0);             /* wildcard SSID */
    out[6] = BIT(5);             /* SCAN_FUNC_SPLIT_SCAN */
    out[7] = 1;                  /* version */
    out[158] = 4;                /* specified channels */
    out[159] = 1;
    out[160] = band;
    out[161] = channel;
    return 0;
}

int oracle_cancel_hw_scan(uint8_t scan_sequence, uint8_t out[4])
{
    memset(out, 0, 4);
    out[0] = scan_sequence;
    return 0;
}

/* mt76_connac_mcu_uni_add_bss station-mode BASIC+QBSS assignments. */
int oracle_client_bss(uint8_t bss_index, const uint8_t bssid[6],
                      uint16_t channel, uint16_t beacon_interval,
                      uint8_t dtim_period, bool qos, bool enable,
                      uint8_t out[44])
{
    uint32_t conn_type = cpu_to_le32(0x00010001);
    uint16_t value;
    memset(out, 0, 44);
    out[0] = bss_index;
    out[4] = 0; out[5] = 0; out[6] = 32; out[7] = 0;
    out[8] = 1;                  /* source keeps station BSS deactivated */
    memcpy(out + 12, &conn_type, sizeof(conn_type));
    out[16] = !enable;
    memcpy(out + 18, bssid, 6);
    value = cpu_to_le16(19);
    memcpy(out + 24, &value, sizeof(value));
    value = cpu_to_le16(beacon_interval);
    memcpy(out + 26, &value, sizeof(value));
    out[28] = dtim_period;
    out[29] = channel <= 14 ? 0x4e : 0xb1;
    value = cpu_to_le16(19);
    memcpy(out + 30, &value, sizeof(value));
    value = cpu_to_le16(channel <= 14 ? 0x53 : 0x78);
    memcpy(out + 32, &value, sizeof(value));
    out[36] = 15; out[38] = 8;
    out[40] = qos;
    return 0;
}

/* mt76_connac_mcu_sta_basic_tlv followed by the empty reset-and-set WTBL
 * emitted by the first mt7921_mac_sta_add transition. */
int oracle_initial_peer_sta(uint8_t bss_index, uint8_t wcid,
                            const uint8_t peer[6], uint8_t out[40])
{
    uint32_t conn_type = cpu_to_le32(0x00010002);
    uint16_t value;
    memset(out, 0, 40);
    out[0] = bss_index; out[1] = wcid; out[2] = 2; out[4] = 1;
    out[8] = 0; out[10] = 20;
    memcpy(out + 12, &conn_type, sizeof(conn_type));
    memcpy(out + 20, peer, 6);
    value = cpu_to_le16(1);      /* EXTRA_INFO_VER */
    memcpy(out + 26, &value, sizeof(value));
    out[28] = 13; out[30] = 12;
    out[32] = wcid; out[33] = 1;
    return 0;
}

/* mt76_connac_mcu_sta_key_tlv, including Linux's fixed two-key allocation
 * and shortened TLV length for a one-key CCMP install. */
int oracle_key_v2(uint8_t bss_index, uint8_t wcid, uint8_t muar_index,
                  uint8_t key_id, const uint8_t key[16], bool retained,
                  uint8_t retained_id, const uint8_t retained_key[16],
                  bool remove, uint8_t out[88])
{
    memset(out, 0, 88);
    out[0] = bss_index; out[1] = wcid; out[2] = 1;
    out[4] = 1; out[5] = muar_index;
    out[8] = 17;                /* STA_REC_KEY_V2 */
    if (remove) {
        out[10] = 8;
        out[12] = 1;            /* DISABLE_KEY */
        return 0;
    }
    out[10] = retained ? 80 : 44;
    out[13] = retained ? 2 : 1;
    out[16] = 5; out[17] = 36;
    out[18] = retained ? retained_id : key_id;
    out[19] = 16;
    memcpy(out + 20, retained ? retained_key : key, 16);
    if (retained) {
        out[52] = 10; out[53] = 36; out[54] = key_id; out[55] = 16;
        memcpy(out + 56, key, 16);
    }
    return 0;
}

/* BSS_CHANGED_ASSOC's second mt7921_mcu_sta_update(dev, NULL, ...): the
 * reserved interface WCID receives only the firmware-offload WTBL TLV. */
int oracle_post_assoc_interface_sta(uint8_t bss_index,
                                    const uint8_t bssid[6], uint8_t out[60])
{
    memset(out, 0, 60);
    out[0] = bss_index; out[1] = 19; out[2] = 1;
    out[8] = 13; out[10] = 52;
    out[12] = 19; out[13] = 1; out[14] = 3;
    out[20] = 0; out[22] = 20;
    memcpy(out + 24, bssid, 6);
    out[30] = 0x0e;
    out[40] = 1; out[42] = 12;
    out[45] = 1; out[46] = 1; out[47] = 1;
    out[52] = 6; out[54] = 8;
    out[56] = 1; out[58] = 1;
    return 0;
}

/* Exact mt76_connac_mcu_build_sku assignments for the valid-input uniform
 * power-limit subset, wrapped in its SET_RATE_TX_POWER request header. */
int oracle_rate_tx_power(uint8_t band, int8_t target,
                         const uint8_t *channels, uint8_t n_chan,
                         const uint8_t alpha2[2], bool last_msg,
                         uint8_t *out)
{
    uint8_t i, nss;
    memset(out, 0, 44 + (size_t)n_chan * 162);
    out[4] = n_chan;
    out[5] = band;
    out[6] = last_msg;
    memcpy(out + 8, alpha2, 2);
    for (i = 0; i < n_chan; i++) {
        uint8_t *entry = out + 44 + (size_t)i * 162;
        uint8_t *sku = entry + 1;
        entry[0] = channels[i];
        memset(sku, 127, 161);
        if (band == 1)
            memset(sku, target, 4);
        memset(sku + 4, target, 8);
        memset(sku + 12, target, 8);
        memset(sku + 20, target, 8);
        sku[28] = target;
        for (nss = 0; nss < 4; nss++)
            memset(sku + 29 + (size_t)nss * 12, target, 10);
        memset(sku + 77, target, 84);
    }
    return 44 + n_chan * 162;
}

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

struct oracle_dma_queue_state {
    uint32_t tail;
    uint32_t queued;
    uint32_t returned_index;
    uint32_t entry_cleared;
    uint32_t released_buffers;
    uint32_t rx_head_cleared;
};

int oracle_dma_dequeue_bookkeeping(uint32_t ndesc, uint32_t tail,
                                   uint32_t queued, bool dma_done,
                                   struct oracle_dma_queue_state *out)
{
    struct mt76_desc desc[64] = {0};
    struct mt76_queue_entry entry[64] = {0};
    struct mt76_queue q = {0};
    struct mt76_dev dev = {0};
    bool more;
    void *buf;
    uint32_t i;
    if (!out || ndesc < 2 || ndesc > 64 || tail >= ndesc || queued > ndesc)
        return -1;
    for (i = 0; i < queued; i++) {
        uint32_t idx = (tail + i) % ndesc;
        entry[idx].buf = (void *)(uintptr_t)(idx + 1);
    }
    if (dma_done)
        desc[tail].ctrl = cpu_to_le32(MT_DMA_CTL_DMA_DONE);
    q.tail = tail;
    q.ndesc = ndesc;
    q.queued = queued;
    q.desc = desc;
    q.entry = entry;
    buf = mt76_dma_dequeue(&dev, &q, false, NULL, NULL, &more, NULL);
    out->tail = q.tail;
    out->queued = q.queued;
    out->returned_index = buf ? (uint32_t)(uintptr_t)buf - 1 : UINT32_MAX;
    out->entry_cleared = buf && entry[tail].buf == NULL;
    out->released_buffers = 0;
    out->rx_head_cleared = 1;
    return 0;
}

int oracle_dma_rx_cleanup_bookkeeping(uint32_t ndesc, uint32_t tail,
                                      uint32_t queued,
                                      struct oracle_dma_queue_state *out)
{
    struct mt76_desc desc[64] = {0};
    struct mt76_queue_entry entry[64] = {0};
    struct mt76_queue q = {0};
    struct mt76_dev dev = {0};
    uint32_t i, cleared = 0;
    if (!out || ndesc < 2 || ndesc > 64 || tail >= ndesc || queued > ndesc)
        return -1;
    for (i = 0; i < queued; i++) {
        uint32_t idx = (tail + i) % ndesc;
        entry[idx].buf = (void *)(uintptr_t)(idx + 1);
    }
    q.tail = tail;
    q.ndesc = ndesc;
    q.queued = queued;
    q.desc = desc;
    q.entry = entry;
    q.rx_head = (void *)1;
    oracle_released_buffers = 0;
    mt76_dma_rx_cleanup(&dev, &q);
    for (i = 0; i < queued; i++)
        cleared += entry[(tail + i) % ndesc].buf == NULL;
    out->tail = q.tail;
    out->queued = q.queued;
    out->returned_index = UINT32_MAX;
    out->entry_cleared = cleared;
    out->released_buffers = oracle_released_buffers;
    out->rx_head_cleared = q.rx_head == NULL;
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
