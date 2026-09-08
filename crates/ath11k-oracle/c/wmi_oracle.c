#include <linux/types.h>
#include <stddef.h>
#include <string.h>

/* These packed layouts and assignments are the captured skb bodies built by
 * the same-named functions in pinned ath11k/wmi.c. The wrapper substitutes the
 * allocator/send boundary only; no transport or firmware state is involved. */
struct wmi_mac_addr { u8 addr[6]; u8 pad[2]; } __packed;
struct id_cmd { u32 tlv_header; u32 vdev_id; } __packed;
struct pdev_set_param_cmd { u32 tlv_header, pdev_id, param_id, param_value; } __packed;
struct peer_cmd { u32 tlv_header, vdev_id; struct wmi_mac_addr peer; } __packed;
struct peer_create_cmd { u32 tlv_header, vdev_id; struct wmi_mac_addr peer; u32 peer_type; } __packed;
struct peer_set_param_cmd { u32 tlv_header, vdev_id; struct wmi_mac_addr peer; u32 param_id, param_value; } __packed;
struct vdev_up_cmd { u32 tlv_header, vdev_id, assoc_id; struct wmi_mac_addr bssid, tx_bssid; u32 profile_idx, profile_cnt; } __packed;
struct vdev_create_cmd { u32 tlv_header, vdev_id, vdev_type, vdev_subtype; struct wmi_mac_addr mac;
    u32 streams, pdev_id, mbssid_flags, mbssid_tx_vdev_id; } __packed;
struct txrx_streams { u32 tlv_header, band, tx, rx; } __packed;
struct vdev_start_input {
    u8 restart, hidden, pmf, crypto_disabled; u32 vdev_id, beacon_interval, dtim_period;
    u32 ssid_len; u8 ssid[32]; u32 bcn_tx_rate, noa, tx, rx, he_ops, cac, regdomain;
    u32 mbssid_flags, mbssid_tx_vdev_id; u32 channel[6];
};
struct init_chunk { u32 request_id, address, size; };
struct init_band { u32 pdev_id, start_freq, end_freq; };
struct init_input { u32 words[72], chunk_len; struct init_chunk chunks[32];
    u8 mode_valid; u32 mode, band_len; struct init_band bands[3]; };

struct oracle_wmi_capture { u32 id; size_t len; u8 bytes[8192]; };
static u32 tlv_header(u16 tag, size_t size) { return ((u32)tag << 16) | (u16)(size - 4); }
static int capture(struct oracle_wmi_capture *out, u32 id, const void *body, size_t len) {
    if (len > sizeof(out->bytes)) return -22;
    out->id = id; out->len = len; memcpy(out->bytes, body, len); return 0;
}
static struct wmi_mac_addr mac(const u8 *address) {
    struct wmi_mac_addr result = {0}; memcpy(result.addr, address, 6); return result;
}

int oracle_wmi_vdev_id(u32 kind, u32 vdev_id, struct oracle_wmi_capture *out) {
    static const u16 tags[] = { 0x57, 0x5d, 0x5e };
    static const u32 ids[] = { 0x5002, 0x5006, 0x5007 };
    if (kind >= 3) return -22;
    struct id_cmd cmd = { tlv_header(tags[kind], sizeof(cmd)), vdev_id };
    return capture(out, ids[kind], &cmd, sizeof(cmd));
}
int oracle_wmi_pdev_set_param(u32 pdev_id, u32 param_id, u32 value,
                              struct oracle_wmi_capture *out) {
    struct pdev_set_param_cmd cmd = { tlv_header(0x52, sizeof(cmd)), pdev_id, param_id, value };
    return capture(out, 0x4003, &cmd, sizeof(cmd));
}
int oracle_wmi_peer(u32 kind, u32 vdev_id, const u8 *address, u32 peer_type,
                    u32 param_id, u32 value, struct oracle_wmi_capture *out) {
    if (kind == 0) {
        struct peer_create_cmd cmd = { tlv_header(0x61, sizeof(cmd)), vdev_id, mac(address), peer_type };
        return capture(out, 0x6001, &cmd, sizeof(cmd));
    }
    if (kind == 1) {
        struct peer_cmd cmd = { tlv_header(0x62, sizeof(cmd)), vdev_id, mac(address) };
        return capture(out, 0x6002, &cmd, sizeof(cmd));
    }
    if (kind == 2) {
        struct peer_set_param_cmd cmd = { tlv_header(0x64, sizeof(cmd)), vdev_id, mac(address), param_id, value };
        return capture(out, 0x6004, &cmd, sizeof(cmd));
    }
    return -22;
}
int oracle_wmi_vdev_up(u32 vdev_id, u32 assoc_id, const u8 *bssid,
                       const u8 *tx_bssid, u32 profile_idx, u32 profile_cnt,
                       struct oracle_wmi_capture *out) {
    struct vdev_up_cmd cmd = {0};
    cmd.tlv_header = tlv_header(0x5c, sizeof(cmd)); cmd.vdev_id = vdev_id;
    cmd.assoc_id = assoc_id; cmd.bssid = mac(bssid);
    if (tx_bssid) cmd.tx_bssid = mac(tx_bssid);
    cmd.profile_idx = profile_idx; cmd.profile_cnt = profile_cnt;
    return capture(out, 0x5005, &cmd, sizeof(cmd));
}
int oracle_wmi_vdev_create(u32 vdev_id, u32 type, u32 subtype, const u8 *address,
                           u32 pdev_id, u32 mbssid_flags, u32 mbssid_tx_vdev_id,
                           u32 tx2, u32 rx2, u32 tx5, u32 rx5,
                           struct oracle_wmi_capture *out) {
    u8 body[sizeof(struct vdev_create_cmd) + 4 + 2 * sizeof(struct txrx_streams)] = {0};
    struct vdev_create_cmd *cmd = (void *)body;
    cmd->tlv_header = tlv_header(0x56, sizeof(*cmd)); cmd->vdev_id = vdev_id;
    cmd->vdev_type = type; cmd->vdev_subtype = subtype; cmd->mac = mac(address);
    cmd->streams = 2; cmd->pdev_id = pdev_id; cmd->mbssid_flags = mbssid_flags;
    cmd->mbssid_tx_vdev_id = mbssid_tx_vdev_id;
    u32 *array = (void *)(body + sizeof(*cmd)); *array = ((u32)0x12 << 16) | 32;
    struct txrx_streams *streams = (void *)(array + 1);
    streams[0] = (struct txrx_streams){ tlv_header(0x19b, sizeof(*streams)), 0, tx2, rx2 };
    streams[1] = (struct txrx_streams){ tlv_header(0x19b, sizeof(*streams)), 1, tx5, rx5 };
    return capture(out, 0x5001, body, sizeof(body));
}
int oracle_wmi_vdev_start(const struct vdev_start_input *in, struct oracle_wmi_capture *out) {
    u8 body[140] = {0}; size_t p = 0;
#define PUT32(value) do { u32 v_ = (value); memcpy(body + p, &v_, 4); p += 4; } while (0)
    PUT32(((u32)0x58 << 16) | 104); PUT32(in->vdev_id); PUT32(0);
    PUT32(in->beacon_interval); PUT32(in->dtim_period);
    u32 flags = 8 | (in->crypto_disabled ? 16 : 0);
    if (!in->restart) flags |= (in->hidden ? 1 : 0) | (in->pmf ? 2 : 0);
    PUT32(flags); PUT32(in->restart ? 0 : in->ssid_len);
    if (!in->restart) memcpy(body + p, in->ssid, in->ssid_len);
    p += 32;
    PUT32(in->bcn_tx_rate); PUT32(0); PUT32(in->noa); PUT32(0); PUT32(in->tx);
    PUT32(in->rx); PUT32(in->he_ops); PUT32(in->cac); PUT32(in->regdomain); PUT32(0);
    PUT32(in->mbssid_flags); PUT32(in->mbssid_tx_vdev_id);
    PUT32(((u32)0x50 << 16) | 24);
    for (size_t i = 0; i < 6; i++) PUT32(in->channel[i]);
    PUT32((u32)0x12 << 16);
#undef PUT32
    return capture(out, in->restart ? 0x5004 : 0x5003, body, p);
}
int oracle_wmi_scan_stop(u32 requester, u32 scan_id, u32 cancel_type,
                         u32 vdev_id, u32 pdev_id, struct oracle_wmi_capture *out) {
    u32 cmd[6] = { ((u32)0x4e << 16) | 20, requester, scan_id,
        cancel_type == 0 ? 0x04000000 : cancel_type == 1 ? 0x01000000 : 0,
        vdev_id, pdev_id };
    if (cancel_type > 2) return -22;
    return capture(out, 0x3002, cmd, sizeof(cmd));
}
int oracle_wmi_scan_start(const u32 *v, u32 event_flags, u32 control_inputs,
                          u32 adaptive_dwell, const u8 *mac_addr, const u8 *mac_mask,
                          const u32 *channels, size_t channel_len,
                          const u8 *ssid_lengths, const u8 *ssids, size_t ssid_len,
                          const u8 *bssids, size_t bssid_len,
                          const u8 *extra_ie, size_t extra_ie_len,
                          const u32 *short_hints, size_t short_hint_len,
                          const u32 *bssid_hint_freqs, size_t bssid_hint_len,
                          struct oracle_wmi_capture *out) {
    u8 body[8192] = {0}; size_t p = 0;
    size_t ie_pad = extra_ie_len <= 0xffff ? (extra_ie_len + 3) & ~(size_t)3 : 0;
    static const u32 control_bits[] = { 1, 0x10000, 0x100, 0x8000, 0x20000,
        0x40000, 4, 8, 0x10, 0x20, 2, 0x2000, 0x4000, 0x200, 0x400,
        0x800, 0x1000, 0x80000, 0x100000 };
    u32 control = (adaptive_dwell << 21) & 0xe00000;
    for (size_t i = 0; i < sizeof(control_bits) / sizeof(control_bits[0]); i++)
        if (control_inputs & (1u << i)) control |= control_bits[i];
#define PUTSCAN32(value) do { u32 x_ = (value); memcpy(body + p, &x_, 4); p += 4; } while (0)
    PUTSCAN32(((u32)0x4d << 16) | 156);
    for (size_t i = 0; i < 4; i++) PUTSCAN32(v[i]);
    PUTSCAN32(v[4] | event_flags);
    for (size_t i = 5; i <= 13; i++) PUTSCAN32(v[i]);
    PUTSCAN32(control); PUTSCAN32(v[14]); PUTSCAN32(channel_len);
    PUTSCAN32(bssid_len); PUTSCAN32(ssid_len); PUTSCAN32(extra_ie_len); PUTSCAN32(v[15]);
    memcpy(body + p, mac_addr, 6); p += 8; memcpy(body + p, mac_mask, 6); p += 8;
    p += 32; PUTSCAN32(0); PUTSCAN32(v[16]); PUTSCAN32(v[17]);
    PUTSCAN32(v[18]); PUTSCAN32(v[19]); PUTSCAN32(0);
    PUTSCAN32(((u32)0x10 << 16) | (u16)(channel_len * 4));
    for (size_t i = 0; i < channel_len; i++) PUTSCAN32(channels[i]);
    PUTSCAN32(((u32)0x13 << 16) | (u16)(ssid_len * 36));
    for (size_t i = 0; i < ssid_len; i++) {
        PUTSCAN32(ssid_lengths[i]); memcpy(body + p, ssids + i * 32, ssid_lengths[i]); p += 32;
    }
    PUTSCAN32(((u32)0x13 << 16) | (u16)(bssid_len * 8));
    for (size_t i = 0; i < bssid_len; i++) { memcpy(body + p, bssids + i * 6, 6); p += 8; }
    PUTSCAN32(((u32)0x11 << 16) | (u16)ie_pad);
    if (ie_pad) { memcpy(body + p, extra_ie, extra_ie_len); p += ie_pad; }
    if (short_hint_len) {
        PUTSCAN32(((u32)0x13 << 16) | (u16)(short_hint_len * 8));
        for (size_t i = 0; i < short_hint_len * 2; i++) PUTSCAN32(short_hints[i]);
    }
    if (bssid_hint_len) {
        PUTSCAN32(((u32)0x13 << 16) | (u16)(bssid_hint_len * 12));
        for (size_t i = 0; i < bssid_hint_len; i++) { PUTSCAN32(bssid_hint_freqs[i]); p += 8; }
    }
#undef PUTSCAN32
    return capture(out, 0x3001, body, p);
}
int oracle_wmi_install_key(u32 vdev_id, const u8 *address, u32 key_idx,
                           u32 key_flags, u32 cipher, u32 rsc_low, u32 rsc_high,
                           const u8 *key, size_t key_len, u32 txmic, u32 rxmic,
                           struct oracle_wmi_capture *out) {
    size_t aligned = (key_len + 3) & ~(size_t)3, p = 0;
    if (104 + 4 + aligned > sizeof(out->bytes)) return -22;
    u8 body[8192] = {0};
#define PUTKEY32(value) do { u32 v_ = (value); memcpy(body + p, &v_, 4); p += 4; } while (0)
    PUTKEY32(((u32)0x60 << 16) | 100); PUTKEY32(vdev_id);
    memcpy(body + p, address, 6); p += 8;
    PUTKEY32(key_idx); PUTKEY32(key_flags); PUTKEY32(cipher); PUTKEY32(rsc_low); PUTKEY32(rsc_high);
    p += 48; PUTKEY32(key_len); PUTKEY32(txmic); PUTKEY32(rxmic); PUTKEY32(0); PUTKEY32(0);
    PUTKEY32(((u32)0x11 << 16) | (u16)aligned); memcpy(body + p, key, key_len); p += aligned;
#undef PUTKEY32
    return capture(out, 0x5009, body, p);
}
int oracle_wmi_peer_assoc(const u32 *v, const u8 *address, const u32 *ppet,
                          const u8 *legacy, size_t legacy_len,
                          const u8 *ht, size_t ht_len,
                          const u32 *he, size_t he_len, u32 flags_in,
                          struct oracle_wmi_capture *out) {
    u8 body[1024] = {0}; size_t p = 0;
    size_t legacy_pad = (legacy_len + 3) & ~(size_t)3;
    size_t ht_pad = (ht_len + 3) & ~(size_t)3;
    u32 peer_flags = 0;
#define INPUT(bit) ((flags_in & (1u << (bit))) != 0)
    if (INPUT(2)) {
        static const u32 bits[] = { 2, 0x800, 0x1000, 0x2000, 0x4000000,
            0x40000000, 0x8000, 0x10000, 0x40000, 0x20000, 0x200000,
            0x2000000, 0x400, 0x400000, 0x800000 };
        for (size_t i = 0; i < sizeof(bits) / sizeof(bits[0]); i++)
            if (INPUT(i + 3)) peer_flags |= bits[i];
    }
    if (INPUT(18)) peer_flags |= 1;
    if (INPUT(19)) {
        peer_flags |= 4;
        if (!INPUT(23) && INPUT(22)) peer_flags &= ~1u;
    }
    if (INPUT(20)) peer_flags |= 0x10;
    if (INPUT(21)) peer_flags &= ~(u32)(4 | 0x10);
    if (INPUT(1)) peer_flags |= 0x8000000;
    if (!ht_len) peer_flags &= ~0x1000u;
#define PUTPA32(value) do { u32 x_ = (value); memcpy(body + p, &x_, 4); p += 4; } while (0)
    PUTPA32(((u32)0x65 << 16) | 160);
    memcpy(body + p, address, 6); p += 8;
    for (size_t i = 0; i < 3; i++) PUTPA32(v[i]);
    PUTPA32(peer_flags);
    for (size_t i = 4; i <= 8; i++) PUTPA32(v[i]);
    PUTPA32(v[3]); PUTPA32(v[11]); PUTPA32(v[9]); PUTPA32(v[10]);
    PUTPA32(0); PUTPA32(0); PUTPA32(legacy_len); PUTPA32(ht_len); PUTPA32(v[12]);
    PUTPA32(v[26]); PUTPA32(v[27]);
    for (size_t i = 0; i < 8; i++) PUTPA32(ppet[i]);
    PUTPA32(v[18]); PUTPA32(v[22]);
    for (size_t i = 23; i <= 25; i++) PUTPA32(v[i]);
    PUTPA32(he_len); PUTPA32(v[19]); PUTPA32(v[20]); PUTPA32(v[17]); PUTPA32(v[21]);
    PUTPA32(((u32)0x11 << 16) | (u16)legacy_pad);
    memcpy(body + p, legacy, legacy_len); p += legacy_pad;
    PUTPA32(((u32)0x11 << 16) | (u16)ht_pad);
    memcpy(body + p, ht, ht_len); p += ht_pad;
    PUTPA32(((u32)0x66 << 16) | 20);
    if (INPUT(0)) {
        PUTPA32(v[15]); PUTPA32(v[16]); PUTPA32(v[13]); PUTPA32(v[14]);
    } else {
        p += 16;
    }
    PUTPA32(0);
    PUTPA32(((u32)0x12 << 16) | (u16)(he_len * 12));
    for (size_t i = 0; i < he_len; i++) {
        PUTPA32(((u32)0x285 << 16) | 8); PUTPA32(he[i * 2]); PUTPA32(he[i * 2 + 1]);
    }
#undef PUTPA32
#undef INPUT
    return capture(out, 0x6005, body, p);
}
int oracle_wmi_mgmt_send(u32 vdev_id, u32 desc_id, u32 freq, u64 paddr,
                         const u8 *frame, size_t frame_len, u8 params_valid,
                         struct oracle_wmi_capture *out) {
    size_t download = frame_len < 64 ? frame_len : 64;
    size_t aligned = (download + 3) & ~(size_t)3, p = 0;
    u8 body[128] = {0};
#define PUTMGMT32(value) do { u32 v_ = (value); memcpy(body + p, &v_, 4); p += 4; } while (0)
    PUTMGMT32(((u32)0x1a6 << 16) | 32); PUTMGMT32(vdev_id); PUTMGMT32(desc_id); PUTMGMT32(freq);
    PUTMGMT32((u32)paddr); PUTMGMT32((u32)(paddr >> 32)); PUTMGMT32(frame_len);
    PUTMGMT32(download); PUTMGMT32(params_valid != 0);
    PUTMGMT32(((u32)0x11 << 16) | (u16)download); memcpy(body + p, frame, download); p += aligned;
    if (params_valid) { PUTMGMT32(((u32)0x284 << 16) | 8); PUTMGMT32(0); PUTMGMT32(1 << 21); }
#undef PUTMGMT32
    return capture(out, 0x7008, body, p);
}
int oracle_wmi_init(const struct init_input *in, struct oracle_wmi_capture *out) {
    u8 body[1024] = {0}; size_t p = 0;
#define PUTINIT32(value) do { u32 v_ = (value); memcpy(body + p, &v_, 4); p += 4; } while (0)
    PUTINIT32(((u32)0x4a << 16) | 28); p += 24; PUTINIT32(in->chunk_len);
    PUTINIT32(((u32)0x4b << 16) | 288);
    for (size_t i = 0; i < 72; i++) {
        u32 value = in->words[i];
        if ((i >= 44 && i <= 50) || i == 54 || i == 55 || (i >= 60 && i <= 66) || i == 69)
            value = 0;
        if (i == 67) value = 1 << 9;
        if (i == 68) value &= 1 << 4;
        PUTINIT32(value);
    }
    PUTINIT32(((u32)0x12 << 16) | (u16)(in->chunk_len * 16));
    for (size_t i = 0; i < in->chunk_len; i++) {
        PUTINIT32(((u32)0x4c << 16) | 16); PUTINIT32(in->chunks[i].request_id);
        PUTINIT32(in->chunks[i].address); PUTINIT32(in->chunks[i].size);
    }
    if (in->mode_valid) {
        PUTINIT32(((u32)0x203 << 16) | 12); PUTINIT32(0); PUTINIT32(in->mode); PUTINIT32(in->band_len);
        PUTINIT32(((u32)0x12 << 16) | (u16)(in->band_len * 16));
        for (size_t i = 0; i < in->band_len; i++) {
            PUTINIT32(((u32)0x24b << 16) | 12); PUTINIT32(in->bands[i].pdev_id);
            PUTINIT32(in->bands[i].start_freq); PUTINIT32(in->bands[i].end_freq);
        }
    }
    if (in->chunk_len) p += (32 - in->chunk_len) * 16;
#undef PUTINIT32
    return capture(out, 1, body, p);
}

struct oracle_wmi_tlv { u16 tag, len; size_t offset; };
static int trace_tlvs(const u8 *bytes, size_t length, size_t base,
                      struct oracle_wmi_tlv *events, size_t *count, size_t capacity) {
    size_t offset = 0;
    while (length > 0) {
        u32 header;
        u16 len, tag;
        if (length < sizeof(header)) return -22;
        memcpy(&header, bytes + offset, sizeof(header));
        tag = header >> 16; len = header & 0xffff;
        if ((size_t)len > length - sizeof(header)) return -22;
        if (*count >= capacity) return -28;
        events[(*count)++] = (struct oracle_wmi_tlv){ tag, len, base + offset };
        if (tag == 0x12) {
            int rc = trace_tlvs(bytes + offset + 4, len, base + offset + 4,
                                events, count, capacity);
            if (rc) return rc;
        }
        size_t padded = (len + 3) & ~(size_t)3;
        offset += sizeof(header) + padded;
        length -= sizeof(header) + padded;
    }
    return 0;
}
int oracle_wmi_tlv_iter(const u8 *bytes, size_t length,
                        struct oracle_wmi_tlv *events, size_t *event_count) {
    size_t count = 0, capacity = *event_count;
    int rc = trace_tlvs(bytes, length, 0, events, &count, capacity);
    if (!rc) *event_count = count;
    return rc;
}

/* Receive-side oracle.  The table replacement and walking rules below are a
 * userspace transcription of pinned ath11k_wmi_tlv_iter(),
 * ath11k_wmi_tlv_iter_parse(), and ath11k_wmi_tlv_parse().  The field pulls
 * follow the corresponding packed wmi_* event layouts in pinned wmi.h. */
#define EVENT_FIELD_MAX 192
#define EVENT_TLV_MAX 64
#define EVENT_TRACE_FIELD_MAX 16

struct oracle_wmi_trace_field { u16 name; u64 value; };
struct oracle_wmi_event_parse {
    u64 fields[EVENT_FIELD_MAX];
    size_t field_count;
    struct oracle_wmi_tlv tlvs[EVENT_TLV_MAX];
    size_t tlv_count;
    struct oracle_wmi_trace_field trace_fields[EVENT_TRACE_FIELD_MAX];
    size_t trace_field_count;
};

enum oracle_wmi_event_kind {
    ORACLE_SERVICE_READY, ORACLE_SERVICE_READY_EXT, ORACLE_SERVICE_READY_EXT2,
    ORACLE_SERVICE_AVAILABLE, ORACLE_READY, ORACLE_SCAN, ORACLE_VDEV_START,
    ORACLE_VDEV_STOPPED, ORACLE_VDEV_DELETE, ORACLE_PEER_ASSOC,
    ORACLE_PEER_DELETE, ORACLE_INSTALL_KEY, ORACLE_MGMT_RX, ORACLE_MGMT_TX,
    ORACLE_FW_MEM_DUMP, ORACLE_ROAM_CAPABILITY, ORACLE_PEER_CREATE,
    ORACLE_FREQ_AVOID,
};

/* Stable IDs are translated to the Rust TraceSink names by src/lib.rs. */
enum oracle_wmi_trace_name {
    TF_SR_PHY, TF_SR_MAX_MACS, TF_SR_DBS, TF_EXT_GROUPS, TF_EXT_HW_MODES,
    TF_EXT2_DMA_RINGS, TF_READY_EXTRA_MACS, TF_READY_STATUS,
    TF_SCAN_EVENT_TYPE, TF_SCAN_REASON, TF_SCAN_FREQ, TF_SCAN_REQUEST,
    TF_SCAN_ID, TF_SCAN_VDEV, TF_SCAN_TSF,
    TF_VSTART_VDEV, TF_VSTART_REQUESTOR, TF_VSTART_TYPE, TF_VSTART_STATUS,
    TF_VSTART_CHAIN, TF_VSTART_SMPS, TF_VSTART_MAC, TF_VSTART_TX,
    TF_VSTART_RX, TF_VSTART_POWER, TF_VDEV_ID,
    TF_MTX_DESC, TF_MTX_STATUS, TF_MTX_PDEV, TF_MTX_PPDU, TF_MTX_ACK_RSSI,
    TF_FW_DUMP_REQUEST, TF_FW_DUMP_COMPLETE, TF_ROAM_CAPABILITY,
};

static u32 get32(const u8 *p) { u32 v; memcpy(&v, p, 4); return v; }
static u64 getmac(const u8 *p) {
    u64 v = 0; for (size_t i = 0; i < 6; i++) v |= (u64)p[i] << (8 * i); return v;
}
static int push_field(struct oracle_wmi_event_parse *out, u64 value) {
    if (out->field_count == EVENT_FIELD_MAX) return -28;
    out->fields[out->field_count++] = value; return 0;
}
static int push_trace_field(struct oracle_wmi_event_parse *out, u16 name, u64 value) {
    if (out->trace_field_count == EVENT_TRACE_FIELD_MAX) return -28;
    out->trace_fields[out->trace_field_count++] =
        (struct oracle_wmi_trace_field){ name, value };
    return 0;
}
static int push_words(struct oracle_wmi_event_parse *out, const u8 *p,
                      size_t count, u16 first_trace_name) {
    for (size_t i = 0; i < count; i++) {
        u32 value = get32(p + i * 4); int ret = push_field(out, value);
        if (ret) return ret;
        ret = push_trace_field(out, first_trace_name + i, value);
        if (ret) return ret;
    }
    return 0;
}

static size_t event_min_len(u16 tag) {
    switch (tag) {
    case 0x20: return 128; case 0x1ac: return 76;
    case 0x212: case 0x214: return 4; case 0x28: return 40;
    case 0x1c3: return 12; case 0x29: return 4; case 0x2c: return 68;
    case 0x1a7: return 20; case 0x24: return 28; case 0x2a: return 24;
    case 0x23: return 52; case 0x22f: return 20; case 0x1b2: return 12;
    case 0x1c2: case 0x385: case 0x47: return 4;
    case 0x18d: return 8; case 0x364: return 16; default: return 0;
    }
}

static int parse_event_table(const u8 *bytes, size_t length,
                             const u8 **table, u16 *lengths,
                             struct oracle_wmi_event_parse *out) {
    size_t offset = 0;
    while (length) {
        if (length < 4) return -22;
        u32 header = get32(bytes + offset); u16 len = header; u16 tag = header >> 16;
        if ((size_t)len > length - 4 || event_min_len(tag) > len) return -22;
        if (out->tlv_count == EVENT_TLV_MAX) return -28;
        out->tlvs[out->tlv_count++] = (struct oracle_wmi_tlv){ tag, len, offset };
        if (tag < 0x386) { table[tag] = bytes + offset + 4; lengths[tag] = len; }
        offset += 4 + len; length -= 4 + len;
    }
    return 0;
}

static int nested_count(const u8 *bytes, size_t length, u16 required_tag,
                        size_t minimum, size_t *count) {
    *count = 0;
    while (length) {
        if (length < 4) return -22;
        u32 header = get32(bytes); u16 len = header; u16 tag = header >> 16;
        if ((size_t)len > length - 4 || tag != required_tag || len < minimum) return -22;
        (*count)++; bytes += 4 + len; length -= 4 + len;
    }
    return 0;
}

int oracle_wmi_event_parse(u32 kind, const u8 *bytes, size_t length,
                           struct oracle_wmi_event_parse *out) {
    const u8 *table[0x386] = {0}; u16 lengths[0x386] = {0};
    memset(out, 0, sizeof(*out));
    int ret = parse_event_table(bytes, length, table, lengths, out);
    if (ret) return ret;
#define REQUIRE(tag) do { if (!table[(tag)]) return -71; } while (0)
#define FIELD(value) do { ret = push_field(out, (value)); if (ret) return ret; } while (0)
    if (kind == ORACLE_SERVICE_READY) {
        REQUIRE(0x20); const u8 *p = table[0x20];
        FIELD(1); /* fixed present */
        static const u8 offsets[] = { 0,4,8,12,16,20,24,28,32,36,40,44,48,52,
            56,60,68,72,76,104,108,112,116,120,124 };
        for (size_t i = 0; i < sizeof(offsets); i++) FIELD(get32(p + offsets[i]));
        FIELD(table[0x10] != NULL);
        if (table[0x10]) { if (lengths[0x10] < 128) return -22;
            for (size_t i = 0; i < 32; i++) FIELD(get32(table[0x10] + i * 4)); }
        push_trace_field(out, TF_SR_PHY, get32(p + 28));
        push_trace_field(out, TF_SR_MAX_MACS, get32(p + 104));
        return push_trace_field(out, TF_SR_DBS, get32(p + 112));
    }
    if (kind == ORACLE_SERVICE_READY_EXT) {
        REQUIRE(0x1ac); const u8 *p = table[0x1ac]; FIELD(1);
        for (size_t i = 0; i < 19; i++) FIELD(get32(p + i * 4));
        FIELD(table[0x212] != NULL); if (table[0x212]) FIELD(get32(table[0x212]));
        FIELD(table[0x214] != NULL); if (table[0x214]) FIELD(get32(table[0x214]));
        size_t groups = 0, hw_modes = 0; size_t off = 0;
        while (off < length) { u32 h = get32(bytes + off); u16 len = h, tag = h >> 16;
            if (tag == 0x12) { groups++; if (groups == 1) {
                ret = nested_count(bytes + off + 4, len, 0x211, 12, &hw_modes); if (ret) return ret;
                size_t n = 0; const u8 *q = bytes + off + 4;
                while (n++ < hw_modes) { u16 nl = get32(q); FIELD(get32(q + 4));
                    FIELD(get32(q + 8)); FIELD(get32(q + 12)); q += 4 + nl; }
            }} off += 4 + len;
        }
        FIELD(groups); FIELD(hw_modes);
        push_trace_field(out, TF_EXT_GROUPS, groups);
        return push_trace_field(out, TF_EXT_HW_MODES, hw_modes);
    }
    if (kind == ORACLE_SERVICE_READY_EXT2) {
        REQUIRE(0x12); size_t count;
        ret = nested_count(table[0x12], lengths[0x12], 0x2bf, 20, &count); if (ret) return ret;
        FIELD(count); const u8 *p = table[0x12];
        for (size_t i = 0; i < count; i++) { u16 len = get32(p);
            if (get32(p + 8) >= 2) return -22;
            for (size_t w = 0; w < 5; w++) { FIELD(get32(p + 4 + w * 4)); }
            p += 4 + len; }
        return push_trace_field(out, TF_EXT2_DMA_RINGS, count);
    }
    if (kind == ORACLE_SERVICE_AVAILABLE) {
        REQUIRE(0x22f); for (size_t i = 0; i < 5; i++) FIELD(get32(table[0x22f] + i * 4));
        FIELD(table[0x10] != NULL); if (table[0x10]) { if (lengths[0x10] < 16) return -22;
            for (size_t i = 0; i < 4; i++) FIELD(get32(table[0x10] + i * 4)); }
        return 0;
    }
    if (kind == ORACLE_READY) {
        REQUIRE(0x23); const u8 *p = table[0x23]; size_t count = get32(p + 40);
        FIELD(1); FIELD(getmac(p + 24)); FIELD(get32(p + 32));
        FIELD(lengths[0x23] >= 60); if (lengths[0x23] >= 60) FIELD(get32(p + 56));
        FIELD(count); if (table[0x13]) for (size_t i = 0; i < count && i * 8 + 8 <= lengths[0x13]; i++) FIELD(getmac(table[0x13] + i * 8));
        push_trace_field(out, TF_READY_EXTRA_MACS,
            table[0x13] ? (lengths[0x13] / 8 < count ? lengths[0x13] / 8 : count) : 0);
        return push_trace_field(out, TF_READY_STATUS, get32(p + 32));
    }
    if (kind == ORACLE_SCAN) { REQUIRE(0x24); return push_words(out, table[0x24], 7, TF_SCAN_EVENT_TYPE); }
    if (kind == ORACLE_VDEV_START) { REQUIRE(0x28); return push_words(out, table[0x28], 10, TF_VSTART_VDEV); }
    if (kind == ORACLE_VDEV_STOPPED) { REQUIRE(0x29); return push_words(out, table[0x29], 1, TF_VDEV_ID); }
    if (kind == ORACLE_VDEV_DELETE) { REQUIRE(0x1c2); return push_words(out, table[0x1c2], 1, TF_VDEV_ID); }
    if (kind == ORACLE_PEER_ASSOC || kind == ORACLE_PEER_DELETE) {
        u16 tag = kind == ORACLE_PEER_ASSOC ? 0x1b2 : 0x1c3; REQUIRE(tag);
        FIELD(get32(table[tag])); FIELD(getmac(table[tag] + 4)); return 0;
    }
    if (kind == ORACLE_INSTALL_KEY) { REQUIRE(0x2a); const u8 *p = table[0x2a];
        FIELD(get32(p)); FIELD(getmac(p + 4)); FIELD(get32(p + 12));
        FIELD(get32(p + 16)); FIELD(get32(p + 20)); return 0; }
    if (kind == ORACLE_MGMT_RX) { REQUIRE(0x2c); const u8 *p = table[0x2c];
        static const u8 offsets[] = { 0,4,8,12,20,40,44,48,60,64 };
        for (size_t i = 0; i < sizeof(offsets); i++) FIELD(get32(p + offsets[i]));
        size_t frame_len = get32(p + 16); REQUIRE(0x11); FIELD(frame_len);
        /* ath11k_pull_mgmt_rx_params_tlv checks the skb tail, not ARRAY_BYTE len. */
        const u8 *end = bytes + length; if (table[0x11] + frame_len > end) return -22;
        for (size_t i = 0; i < frame_len; i++) { FIELD(table[0x11][i]); }
        return 0;
    }
    if (kind == ORACLE_MGMT_TX) { REQUIRE(0x1a7); return push_words(out, table[0x1a7], 5, TF_MTX_DESC); }
    if (kind == ORACLE_FW_MEM_DUMP) {
        REQUIRE(0x18d); return push_words(out, table[0x18d], 2, TF_FW_DUMP_REQUEST);
    }
    if (kind == ORACLE_ROAM_CAPABILITY) {
        REQUIRE(0x385); return push_words(out, table[0x385], 1, TF_ROAM_CAPABILITY);
    }
    if (kind == ORACLE_PEER_CREATE) {
        REQUIRE(0x364); const u8 *p = table[0x364];
        FIELD(get32(p)); FIELD(getmac(p + 4)); FIELD(get32(p + 12)); return 0;
    }
    if (kind == ORACLE_FREQ_AVOID) {
        REQUIRE(0x47); REQUIRE(0x12); size_t count = get32(table[0x47]), actual;
        ret = nested_count(table[0x12], lengths[0x12], 0x48, 8, &actual);
        if (ret || actual != count) return -22;
        FIELD(count); const u8 *p = table[0x12];
        for (size_t i = 0; i < count; i++) {
            u16 len = get32(p); FIELD(get32(p + 4)); FIELD(get32(p + 8)); p += 4 + len;
        }
        return 0;
    }
    return -22;
#undef FIELD
#undef REQUIRE
}
