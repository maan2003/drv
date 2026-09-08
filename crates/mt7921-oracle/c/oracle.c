_Static_assert(sizeof(struct mt76_connac2_mcu_txd) == 64,
               "legacy MCU TXD layout changed");
_Static_assert(sizeof(struct mt76_desc) == 16, "DMA descriptor layout changed");
_Static_assert(sizeof(struct mt76_connac2_mcu_rxd) == 36,
               "MCU RXD layout changed");

/* Normalized register/branch operations from pinned mt7921e_mac_reset and
 * mt792x_wpdma_reset -> mt792x_dma_{disable,enable}.  build.rs independently
 * requires these operations in this order in the immutable v7.1.5 source.
 * Queue/NAPI mechanics are branches; register values preserve exact RMW
 * effects.  MT7921 takes the non-MT7925/non-MT7902 prefetch branch. */
struct oracle_reset_event {
    uint8_t kind; /* 1 branch, 2 write, 3 clear, 4 set, 5 read */
    uint8_t reg;
    uint16_t pad;
    uint32_t value;
};

static void oracle_reset_push(struct oracle_reset_event *events, size_t *count,
                              uint8_t kind, uint8_t reg, uint32_t value)
{
    events[(*count)++] =
        (struct oracle_reset_event){ kind, reg, 0, value };
}

int oracle_mt7921_reset(uint8_t busy_reads,
                        uint32_t global_config, uint32_t ext0,
                        uint32_t dmashdl, uint32_t reset,
                        struct oracle_reset_event *events, size_t *event_count,
                        uint32_t final_registers[4])
{
    static const uint16_t prefetch[] = {
        0x000, 0x040, 0x080, 0x0c0, 0x100,
        0x140, 0x180, 0x1c0, 0x200, 0x240, 0x280, 0x2c0, 0x340, 0x380,
    };
    const uint32_t disable = BIT(0) | BIT(2) | BIT(15) | BIT(21) |
                             BIT(27) | BIT(28);
    const uint32_t configure = BIT(6) | BIT(12) | BIT(30) | BIT(28) |
                               FIELD_PREP(GENMASK(5, 4), 3) | BIT(11) |
                               BIT(13) | BIT(15) | BIT(21);
    size_t i, count = 0;

    if (!events || !event_count || !final_registers || !busy_reads ||
        busy_reads > 101)
        return -1;

    oracle_reset_push(events, &count, 1, 1, 0); /* conn-on driver own */
    oracle_reset_push(events, &count, 2, 2, 0); /* host IRQ disable */
    oracle_reset_push(events, &count, 2, 3, 0); /* PCI MAC IRQ disable */
    oracle_reset_push(events, &count, 1, 4, 1); /* forced WPDMA reset */
    oracle_reset_push(events, &count, 1, 5, 1); /* WFSYS reset */

    global_config &= ~disable;
    oracle_reset_push(events, &count, 3, 6, global_config);
    for (i = 1; i <= busy_reads; i++)
        oracle_reset_push(events, &count, 5, 6,
                          i == busy_reads ? global_config & ~(BIT(1) | BIT(3))
                                          : global_config | BIT(1));
    ext0 &= ~BIT(6);
    oracle_reset_push(events, &count, 3, 7, ext0);
    dmashdl |= BIT(28);
    oracle_reset_push(events, &count, 4, 8, dmashdl);
    reset &= ~(BIT(4) | BIT(5));
    oracle_reset_push(events, &count, 3, 9, reset);
    reset |= BIT(4) | BIT(5);
    oracle_reset_push(events, &count, 4, 9, reset);

    oracle_reset_push(events, &count, 1, 10, 1); /* queue reset */
    for (i = 0; i < ARRAY_SIZE(prefetch); i++)
        oracle_reset_push(events, &count, 2, (uint8_t)(16 + i),
                          ((uint32_t)prefetch[i] << 16) | 4);
    oracle_reset_push(events, &count, 2, 30, UINT32_MAX); /* DTX ptr */
    oracle_reset_push(events, &count, 2, 31, 0); /* delay interrupt */
    global_config |= configure;
    oracle_reset_push(events, &count, 4, 6, global_config);
    global_config |= BIT(0) | BIT(2);
    oracle_reset_push(events, &count, 4, 6, global_config);
    oracle_reset_push(events, &count, 4, 32, BIT(1)); /* NEED_REINIT */
    oracle_reset_push(events, &count, 4, 2, 1); /* ring IRQ masks */
    oracle_reset_push(events, &count, 4, 33, BIT(0)); /* MCU wake IRQ */
    oracle_reset_push(events, &count, 1, 34, 1); /* RX refill */

    oracle_reset_push(events, &count, 2, 2, 1); /* host IRQ restore */
    oracle_reset_push(events, &count, 2, 3, 0xff);
    oracle_reset_push(events, &count, 1, 35, 1); /* top driver own */
    oracle_reset_push(events, &count, 1, 36, 1); /* firmware reload */
    /* mt7921e_mac_reset does not request firmware ownership here. */

    final_registers[0] = global_config;
    final_registers[1] = ext0;
    final_registers[2] = dmashdl;
    final_registers[3] = reset;
    *event_count = count;
    return 0;
}

int oracle_power_control(bool firmware, bool aspm, uint8_t success_attempt,
                         uint8_t success_read,
                         struct oracle_power_event *events, size_t *event_count,
                         uint64_t *elapsed_us)
{
    struct mt792x_dev dev = {0};
    int result;
    if (!events || !event_count || !elapsed_us || success_attempt > 10)
        return -99;
    oracle_power_event_count = 0;
    oracle_power_time_us = 0;
    oracle_power_attempt = 0;
    oracle_power_read = 0;
    oracle_power_success_attempt = success_attempt;
    oracle_power_success_read = success_read;
    oracle_power_firmware = firmware;
    dev.aspm_supported = aspm;
    result = firmware ? mt792xe_mcu_fw_pmctrl(&dev)
                      : __mt792xe_mcu_drv_pmctrl(&dev);
    memcpy(events, oracle_power_events,
           oracle_power_event_count * sizeof(*events));
    *event_count = oracle_power_event_count;
    *elapsed_us = oracle_power_time_us;
    return result;
}

/* Execute the pinned request builders before the already-extracted Connac2
 * envelope builder. This keeps request assignments independent of Rust. */
int oracle_download_command(uint8_t kind, uint8_t sequence, uint32_t address,
                            uint32_t length, uint32_t mode,
                            uint8_t *out, size_t out_capacity)
{
    struct mt76_dev dev = {0};
    struct sk_buff skb;
    uint8_t storage[128] = {0};
    uint8_t payload[16];
    size_t payload_len;
    int wait_seq = 0, result;

    if (!out || !sequence || sequence > 15)
        return -1;
    oracle_mcu_payload_len = 0;
    switch (kind) {
    case 0:
        result = mt76_connac_mcu_init_download(&dev, address, length, mode);
        break;
    case 1:
        result = mt76_connac_mcu_patch_sem_ctrl(&dev, true);
        break;
    case 2:
        result = mt76_connac_mcu_patch_sem_ctrl(&dev, false);
        break;
    case 3:
        result = mt76_connac_mcu_start_patch(&dev);
        break;
    case 4:
        result = mt76_connac_mcu_start_firmware(&dev, address, mode);
        break;
    default:
        return -2;
    }
    if (result)
        return result;
    payload_len = oracle_mcu_payload_len;
    memcpy(payload, oracle_mcu_payload, payload_len);
    memcpy(storage + 64, payload, payload_len);
    skb.data = storage + 64;
    skb.len = payload_len;
    dev.mcu.msg_seq = sequence - 1;
    if (mt76_connac2_mcu_fill_message(&dev, &skb, oracle_mcu_command,
                                      &wait_seq) || wait_seq != sequence ||
        skb.len > out_capacity)
        return -3;
    memcpy(out, skb.data, skb.len);
    return (int)skb.len;
}

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

/* Execute pinned mt7921_mcu_set_chan_info and retain the request body passed
 * to mt76_mcu_send_msg.  The wrapper admits only the channel forms exposed by
 * mt7921-core: SET_RX_PATH or CHANNEL_SWITCH with normal/off-channel reason. */
int oracle_mt7921_channel_info(uint16_t primary, uint16_t center,
                               uint8_t bandwidth, uint16_t center2,
                               uint8_t band, uint8_t antenna_mask,
                               bool channel_switch, bool offchannel,
                               uint8_t out[76])
{
    struct mt792x_dev dev = {0};
    struct mt76_phy mphy = {0};
    struct ieee80211_channel channel = {0};
    struct ieee80211_hw hw = {0};
    int cmd;

    if (!out || !primary || primary > 255 || !center || center > 255 ||
        center2 > 255 || band > 1 || antenna_mask != 3 ||
        !(bandwidth <= 3 || bandwidth == 6) ||
        (bandwidth == 0 && center != primary) ||
        (bandwidth != 6 && center2) || (bandwidth == 6 && !center2))
        return -1;
    channel.band = band;
    channel.hw_value = primary;
    mphy.dev = &dev.mt76;
    mphy.chandef.chan = &channel;
    mphy.chandef.center_freq1 = center == 14 ? 2484 :
        (band ? 5000 : 2407) + center * 5;
    mphy.chandef.center_freq2 = center2 ? 5000 + center2 * 5 : 0;
    mphy.chandef.width = center2 ? NL80211_CHAN_WIDTH_80P80 : 0;
    mphy.antenna_mask = antenna_mask;
    mphy.offchannel = offchannel;
    dev.mt76.hw = &hw;
    dev.phy.mt76 = &mphy;
    dev.phy.dev = &dev;
    oracle_channel_bw = bandwidth;
    oracle_reg_can_beacon = true;
    oracle_mcu_payload_len = 0;
    cmd = channel_switch ? MCU_EXT_CMD(CHANNEL_SWITCH) : MCU_EXT_CMD(SET_RX_PATH);
    if (mt7921_mcu_set_chan_info(&dev.phy, cmd) || oracle_mcu_payload_len != 76)
        return -2;
    memcpy(out, oracle_mcu_payload, 76);
    return oracle_mcu_command;
}

/* Execute pinned mt76_connac_mcu_set_channel_domain over enabled channel
 * records. The public Rust command rejects disabled and 6 GHz entries, so the
 * wrapper's typed domain is precisely ordered 2/5 GHz records. */
int oracle_channel_domain(const uint8_t alpha2[2], const uint8_t *bands,
                          const uint16_t *channels, const uint32_t *flags,
                          uint8_t count, uint8_t *out)
{
    struct mt76_dev dev = {0};
    struct mt76_phy phy = {0};
    struct ieee80211_channel ch2[64] = {{0}}, ch5[64] = {{0}};
    int n2 = 0, n5 = 0, i;

    if (!alpha2 || !bands || !channels || !flags || !out || count > 64)
        return -1;
    for (i = 0; i < count; i++) {
        struct ieee80211_channel *channel;
        if (bands[i] == 0)
            channel = &ch2[n2++];
        else if (bands[i] == 1)
            channel = &ch5[n5++];
        else
            return -1;
        channel->hw_value = channels[i];
        channel->flags = flags[i];
    }
    memcpy(dev.alpha2, alpha2, 2);
    phy.dev = &dev;
    phy.sband_2g.sband.channels = ch2;
    phy.sband_2g.sband.n_channels = n2;
    phy.sband_5g.sband.channels = ch5;
    phy.sband_5g.sband.n_channels = n5;
    oracle_mcu_payload_len = 0;
    if (mt76_connac_mcu_set_channel_domain(&phy) ||
        oracle_mcu_payload_len != 12 + (size_t)count * 8)
        return -2;
    memcpy(out, oracle_mcu_payload, oracle_mcu_payload_len);
    return oracle_mcu_payload_len;
}

/* Execute pinned __mt7921_mcu_set_clc for one valid matching rule and retain
 * the exact SET_CLC request body. */
int oracle_clc_set(uint8_t index, uint8_t environment, uint8_t capability,
                   const uint8_t alpha2[2], const uint8_t rule_type[2],
                   uint8_t environment_6ghz, uint8_t acpi_configuration,
                   uint8_t mtcl_configuration, const uint8_t *data,
                   uint16_t data_len, uint8_t *out)
{
    uint8_t storage[4096] = {0};
    struct mt7921_clc *clc = (struct mt7921_clc *)storage;
    struct mt7921_clc_rule *rule = (struct mt7921_clc_rule *)clc->data;
    struct mt792x_dev dev = {0};
    size_t total = 16 + 6 + (size_t)data_len + 16;

    if (!out || !alpha2 || !rule_type || !data || index > 1 || !data_len ||
        total > sizeof(storage) || capability > 1)
        return -1;
    clc->len = cpu_to_le32(total);
    clc->idx = index;
    memcpy(rule->alpha2, alpha2, 2);
    memcpy(rule->type, rule_type, 2);
    rule->len = cpu_to_le16(data_len);
    memcpy(rule->data, data, data_len);
    dev.phy.mt76 = &dev.mt76.phy;
    dev.phy.dev = &dev;
    dev.phy.power_type = environment_6ghz;
    dev.phy.chip_cap = capability ? MT792x_CHIP_CAP_CLC_EVT_EN : 0;
    oracle_acpi_flags = acpi_configuration;
    oracle_mtcl_conf = mtcl_configuration;
    oracle_power_limits = false;
    oracle_mcu_payload_len = 0;
    if (__mt7921_mcu_set_clc(&dev, (u8 *)alpha2, environment, clc, index) ||
        oracle_mcu_payload_len != 76 + (size_t)data_len)
        return -2;
    memcpy(out, oracle_mcu_payload, oracle_mcu_payload_len);
    return oracle_mcu_payload_len;
}
