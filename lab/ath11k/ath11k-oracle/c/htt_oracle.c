#include <linux/types.h>
#include <stddef.h>
#include <string.h>
#define ETH_ALEN 6
#include "rx_desc.h"

/* Compile the pinned header rather than duplicating Rust's offset table. */
_Static_assert(sizeof(struct hal_rx_desc_qcn9074) == 384, "QCN9074 RX size");
_Static_assert(offsetof(struct hal_rx_desc_qcn9074, hdr_status) == 264, "RX header");
_Static_assert(offsetof(struct hal_rx_desc_qcn9074, msdu_payload) == 384, "RX payload");

struct oracle_htt_srng {
    u8 pdev_id, ring_id, ring_type, entry_words; u64 base, head, tail, msi;
    u16 size_words, batch_words, timer, low; u32 msi_data;
    u8 msi_swap, host_swap, tlv_swap, low_enable;
};
struct oracle_htt_rx_select { u8 pdev_id, ring_id, status_swap, packet_swap;
    u16 buffer_size; u32 tlvs, management_0, management_1, control, data; };
struct oracle_htt_event { u8 kind, major, minor, vdev_id; u16 peer_id;
    u8 address[6]; u16 ast_hash, hardware_peer_id; u8 v2; };
struct oracle_htt_completion { u8 status, reinject_reason; s8 ack_rssi;
    u8 peer_valid; u16 peer_id; };
struct oracle_qcn_rx { u8 first_msdu, last_msdu, l3_padding, msdu_done;
    u8 msdu_length_error, fcs_error, decrypt_error, tkip_mic_error;
    u8 mpdu_errors, ip_checksum_failed, l4_checksum_failed;
    u8 multicast_broadcast, decrypted; u16 msdu_length; u8 decap_type, mesh_control_present, ldpc, sgi, mcs;
    u8 bandwidth, packet_type, spatial_stream_bitmap, nss; u32 frequency;
    u8 tid; u16 peer; u8 sequence_valid, frame_valid; u16 sequence_number;
    u8 encryption_valid, encryption_type; u16 phy_ppdu_id;
    u8 mpdu_start_valid, address2_valid, address2[6]; };

static void words(u8 *out, const u32 *in, size_t count) { memcpy(out, in, count * 4); }
void oracle_htt_version(u8 *out) { memset(out, 0, 4); }
void oracle_htt_srng_encode(const struct oracle_htt_srng *in, u8 *out) {
    u32 w[13] = {0};
    w[0] = 0x0b | ((u32)in->pdev_id << 8) | ((u32)in->ring_id << 16) | ((u32)in->ring_type << 24);
    w[1] = in->base; w[2] = in->base >> 32;
    w[3] = in->size_words | ((u32)in->entry_words << 16);
    if (in->ring_type == 1) w[3] |= 1 << 25;
    w[3] |= ((u32)in->msi_swap << 27) | ((u32)in->host_swap << 28) | ((u32)in->tlv_swap << 29);
    w[4] = in->head; w[5] = in->head >> 32; w[6] = in->tail; w[7] = in->tail >> 32;
    w[8] = in->msi; w[9] = in->msi >> 32; w[10] = in->msi_data;
    w[11] = (in->batch_words & 0x7fff) | ((u32)in->timer << 16);
    if (in->low_enable) w[12] = in->low;
    words(out, w, 13);
}
void oracle_htt_rx_select_encode(const struct oracle_htt_rx_select *in, u8 *out) {
    u32 w[7] = { 0x0c | ((u32)in->pdev_id << 8) | ((u32)in->ring_id << 16) |
        ((u32)in->status_swap << 24) | ((u32)in->packet_swap << 25), in->buffer_size,
        in->management_0, in->management_1, in->control, in->data, in->tlvs };
    words(out, w, 7);
}
void oracle_htt_ppdu_stats_encode(u8 pdev_mask, u16 tlv_mask, u8 out[4]) {
    u32 w = 0x11 | (((u32)pdev_mask & 0x7f) << 9) | ((u32)tlv_mask << 16);
    words(out, &w, 1);
}
void oracle_htt_ext_stats_encode(u8 pdev_mask, u8 stats_type,
                                 const u32 params[4], u64 cookie, u8 out[32]) {
    u32 w[8] = { 0x10 | ((u32)pdev_mask << 8) | ((u32)stats_type << 16),
        params[0], params[1], params[2], params[3], 0,
        (u32)cookie, (u32)(cookie >> 32) };
    words(out, w, 8);
}
int oracle_htt_event_decode(const u8 *bytes, size_t len, struct oracle_htt_event *out) {
    u32 w[4] = {0}; if (len < 4) return -22; memcpy(w, bytes, len < 16 ? len : 16);
    u8 type = w[0]; memset(out, 0, sizeof(*out)); out->kind = type;
    if (type == 0) { out->minor = w[0] >> 8; out->major = w[0] >> 16; return 0; }
    if (type == 3 || type == 0x1e) {
        if (len < (type == 3 ? 12 : 16)) return -22; out->vdev_id = w[0] >> 8; out->peer_id = w[0] >> 16;
        memcpy(out->address, &w[1], 4); memcpy(out->address + 4, &w[2], 2);
        out->v2 = type == 0x1e; out->hardware_peer_id = out->v2 ? w[2] >> 16 : 0;
        if (out->v2) out->ast_hash = w[3]; return 0;
    }
    if (type == 4 || type == 0x1f) {
        out->peer_id = w[0] >> 16; out->v2 = type == 0x1f; return 0; }
    return 0;
}
int oracle_htt_completion_decode(const u8 *bytes, size_t len, struct oracle_htt_completion *out) {
    u32 w[3]; if (len < 24) return -22; memcpy(w, bytes + 8, 12);
    out->status = (w[0] >> 9) & 0xf; out->reinject_reason = (w[0] >> 13) & 0xf;
    out->ack_rssi = w[1] >> 24; out->peer_valid = (w[2] >> 21) & 1; out->peer_id = w[2];
    return 0;
}
static u16 get16(const u8 *p) { u16 v; memcpy(&v, p, 2); return v; }
static u32 get32(const u8 *p) { u32 v; memcpy(&v, p, 4); return v; }
int oracle_qcn9074_rx_decode(const u8 *b, size_t len, struct oracle_qcn_rx *o) {
    if (len < sizeof(struct hal_rx_desc_qcn9074)) return -22;
    u16 end4 = get16(b + offsetof(struct hal_rx_desc_qcn9074, msdu_end.info4)); u32 a1 = get32(b + offsetof(struct hal_rx_desc_qcn9074, attention.info1)), a2 = get32(b + offsetof(struct hal_rx_desc_qcn9074, attention.info2));
    u32 m1 = get32(b + offsetof(struct hal_rx_desc_qcn9074, msdu_start.info1)), m2 = get32(b + offsetof(struct hal_rx_desc_qcn9074, msdu_start.info2)), m3 = get32(b + offsetof(struct hal_rx_desc_qcn9074, msdu_start.info3));
    u32 mpdu9 = get32(b + offsetof(struct hal_rx_desc_qcn9074, mpdu_start.info9)), mpdu11 = get32(b + offsetof(struct hal_rx_desc_qcn9074, mpdu_start.info11));
    memset(o, 0, sizeof(*o)); o->first_msdu = (end4 >> 12) & 1; o->last_msdu = (end4 >> 13) & 1;
    o->l3_padding = (end4 >> 10) & 3; o->msdu_done = a2 >> 31; o->msdu_length_error = (a1 >> 17) & 1;
    o->fcs_error = a1 >> 31; o->decrypt_error = (a1 >> 29) & 1; o->tkip_mic_error = (a1 >> 28) & 1;
    o->mpdu_errors = o->fcs_error | (o->decrypt_error << 1) | (o->tkip_mic_error << 2) |
        (((a1 >> 12) & 1) << 3) | (((a1 >> 16) & 1) << 4) |
        (((a1 >> 17) & 1) << 5) | (((a1 >> 27) & 1) << 6);
    o->ip_checksum_failed = (a1 >> 19) & 1; o->l4_checksum_failed = (a1 >> 18) & 1;
    o->multicast_broadcast = (a1 >> 2) & 1; o->decrypted = ((a2 >> 10) & 7) == 0;
    o->msdu_length = m1 & 0x3fff; o->decap_type = (m2 >> 8) & 3;
    o->mesh_control_present = (m2 >> 22) & 1; o->ldpc = (m2 >> 23) & 1;
    o->sgi = (m3 >> 13) & 3; o->mcs = (m3 >> 15) & 0xf; o->bandwidth = (m3 >> 19) & 3;
    o->packet_type = (m3 >> 8) & 0xf; o->spatial_stream_bitmap = m3 >> 24;
    o->nss = __builtin_popcount(o->spatial_stream_bitmap); o->frequency = get32(b + offsetof(struct hal_rx_desc_qcn9074, msdu_start.phy_meta_data));
    o->tid = (mpdu9 >> 15) & 0xf; o->peer = get16(b + offsetof(struct hal_rx_desc_qcn9074, mpdu_start.sw_peer_id)); o->sequence_valid = (mpdu11 >> 6) & 1;
    o->frame_valid = mpdu11 & 1; o->sequence_number = (mpdu11 >> 20) & 0xfff;
    o->encryption_valid = (mpdu11 >> 9) & 1;
    o->encryption_type = o->encryption_valid ? (mpdu9 >> 2) & 0xf : 7;
    o->phy_ppdu_id = get16(b + offsetof(struct hal_rx_desc_qcn9074, mpdu_start.phy_ppdu_id));
    o->mpdu_start_valid = ((get32(b + offsetof(struct hal_rx_desc_qcn9074, mpdu_start_tag)) >> 1) & 0x1ff) == 207;
    o->address2_valid = (mpdu11 >> 3) & 1;
    memcpy(o->address2, b + offsetof(struct hal_rx_desc_qcn9074, mpdu_start.addr2), sizeof(o->address2));
    return 0;
}

int oracle_reo_msdu_continuation(const u8 *b, size_t len) {
    if (len < 64) return -22;
    return (get32(b + 16) >> 2) & 1;
}

static size_t oracle_80211_hdrlen(const u8 *b, size_t len) {
    u16 fc;
    if (len < 24) return 0;
    fc = get16(b);
    size_t hdrlen = (fc & 0x0300) == 0x0300 ? 30 : 24;
    if ((fc & 0x008c) == 0x0088) hdrlen += 2 + ((fc & 0x8000) ? 4 : 0);
    return len >= hdrlen ? hdrlen : 0;
}

static void oracle_address_offsets(u16 fc, size_t *da, size_t *sa) {
    switch (fc & 0x0300) {
    case 0x0000: *da = 4; *sa = 10; break;
    case 0x0100: *da = 16; *sa = 10; break;
    case 0x0200: *da = 4; *sa = 16; break;
    default: *da = 16; *sa = 24; break;
    }
}

int oracle_undecap_nwifi(const u8 *b, size_t len, const u8 *first_hdr,
                         size_t first_len, u8 first, u8 tid, u8 mesh,
                         u8 enctype, u8 decrypted, u8 *out, size_t capacity) {
    size_t native_len = oracle_80211_hdrlen(b, len), hdrlen, da, sa, out_da, out_sa;
    size_t crypto = (!decrypted && (enctype == 2 || enctype == 4 || enctype == 6 ||
        enctype == 8 || enctype == 9 || enctype == 10)) ? 8 : 0;
    u16 fc, qos;
    if (!native_len) return -22;
    if (!first) {
        if (capacity < len + 2 + crypto) return -28;
        memcpy(out, b, native_len);
        fc = (get16(out) | 0x0080) & ~0x8000;
        if (decrypted) fc &= ~0x4000;
        memcpy(out, &fc, 2);
        qos = tid | (mesh ? 0x0100 : 0);
        memcpy(out + native_len, &qos, 2);
        memcpy(out + native_len + 2, b + native_len, crypto);
        memcpy(out + native_len + 2 + crypto, b + native_len, len - native_len);
        return len + 2 + crypto;
    }
    hdrlen = oracle_80211_hdrlen(first_hdr, first_len);
    if (!hdrlen || first_len < hdrlen + crypto ||
        capacity < hdrlen + crypto + len - native_len) return -22;
    memcpy(out, first_hdr, hdrlen);
    fc = get16(out);
    if ((fc & 0x008c) == 0x0088) {
        size_t qos_offset = (fc & 0x0300) == 0x0300 ? 30 : 24;
        out[qos_offset] &= ~0x80;
    }
    oracle_address_offsets(get16(b), &da, &sa);
    oracle_address_offsets(fc, &out_da, &out_sa);
    memcpy(out + out_da, b + da, 6);
    memcpy(out + out_sa, b + sa, 6);
    if (decrypted) out[1] &= ~0x40;
    memcpy(out + hdrlen, first_hdr + hdrlen, crypto);
    memcpy(out + hdrlen + crypto, b + native_len, len - native_len);
    return hdrlen + crypto + len - native_len;
}

int oracle_tx_encap_nwifi(const u8 *b, size_t len, u8 priority,
                          u8 *out, size_t capacity, u8 *tid) {
    u16 fc;
    size_t qos;
    if (len < 24 || capacity < len) return -22;
    fc = get16(b);
    if ((fc & 0x000c) != 0x0008) return -95;
    *tid = 16;
    if ((fc & 0x008c) != 0x0088) {
        memcpy(out, b, len);
        return len;
    }
    qos = (fc & 0x0300) == 0x0300 ? 30 : 24;
    if (len < qos + 2) return -22;
    *tid = priority & 0x0f;
    memcpy(out, b, qos);
    memcpy(out + qos, b + qos + 2, len - qos - 2);
    fc &= ~0x0080;
    memcpy(out, &fc, sizeof(fc));
    return len - 2;
}

/* action: 0 = retain/ignore, 1 = free silently, 2 = free and report. */
int oracle_tx_completion_decision(const u8 *b, size_t len, u8 *status,
                                  u8 *acked, s8 *ack_rssi,
                                  u8 *peer_valid, u16 *peer) {
    u32 info0, info1, info2;
    u8 source;
    if (len < 32) return -22;
    info0 = get32(b + 8); info1 = get32(b + 12); info2 = get32(b + 16);
    source = info0 & 7;
    *status = 0; *acked = 0; *ack_rssi = 0; *peer_valid = 0; *peer = 0xffff;
    if (source == 3) {
        *status = (info0 >> 9) & 0xf;
        if (*status <= 2) {
            *acked = *status == 0;
            *ack_rssi = info1 >> 24;
            *peer_valid = (info2 >> 21) & 1;
            if (*peer_valid) *peer = info2;
            return 2;
        }
        if (*status == 3 || *status == 4) return 1;
        return 0;
    }
    if (source != 0) return 0;
    *status = (info0 >> 13) & 0xf;
    *acked = *status == 0;
    *ack_rssi = info2;
    *peer_valid = 1;
    *peer = get32(b + 28);
    return 2;
}
