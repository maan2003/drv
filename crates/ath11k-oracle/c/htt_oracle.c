#include <linux/types.h>
#include <stddef.h>
#include <string.h>

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
    u8 multicast_broadcast, decrypted; u16 msdu_length; u8 decap_type, ldpc, sgi, mcs;
    u8 bandwidth, packet_type, spatial_stream_bitmap, nss; u32 frequency;
    u8 tid; u16 peer; u8 sequence_valid, frame_valid; u16 sequence_number;
    u8 encryption_valid, encryption_type; u16 phy_ppdu_id; };

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
int oracle_htt_event_decode(const u8 *bytes, size_t len, struct oracle_htt_event *out) {
    u32 w[4] = {0}; if (len < 4) return -22; memcpy(w, bytes, len < 16 ? len : 16);
    u8 type = w[0]; memset(out, 0, sizeof(*out)); out->kind = type;
    if (type == 0) { out->minor = w[0] >> 8; out->major = w[0] >> 16; return 0; }
    if (type == 3 || type == 0x1e) {
        if (len < 16) return -22; out->vdev_id = w[0] >> 8; out->peer_id = w[0] >> 16;
        memcpy(out->address, &w[1], 4); memcpy(out->address + 4, &w[2], 2);
        out->hardware_peer_id = w[2] >> 16; out->v2 = type == 0x1e;
        if (out->v2) out->ast_hash = w[3]; return 0;
    }
    if (type == 4 || type == 0x1f) { if (len < 12) return -22;
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
    if (len < 388) return -22;
    u16 end4 = get16(b + 46); u32 a1 = get32(b + 80), a2 = get32(b + 84);
    u32 m1 = get32(b + 96), m2 = get32(b + 100), m3 = get32(b + 112);
    u32 mpdu9 = get32(b + 168), mpdu11 = get32(b + 184);
    memset(o, 0, sizeof(*o)); o->first_msdu = (end4 >> 12) & 1; o->last_msdu = (end4 >> 13) & 1;
    o->l3_padding = (end4 >> 10) & 3; o->msdu_done = a2 >> 31; o->msdu_length_error = (a1 >> 17) & 1;
    o->fcs_error = a1 >> 31; o->decrypt_error = (a1 >> 29) & 1; o->tkip_mic_error = (a1 >> 28) & 1;
    o->multicast_broadcast = (a1 >> 2) & 1; o->decrypted = ((a2 >> 10) & 7) == 0;
    o->msdu_length = m1 & 0x3fff; o->decap_type = (m2 >> 8) & 3; o->ldpc = (m2 >> 23) & 1;
    o->sgi = (m3 >> 13) & 3; o->mcs = (m3 >> 15) & 0xf; o->bandwidth = (m3 >> 19) & 3;
    o->packet_type = (m3 >> 8) & 0xf; o->spatial_stream_bitmap = m3 >> 24;
    o->nss = __builtin_popcount(o->spatial_stream_bitmap); o->frequency = get32(b + 120);
    o->tid = (mpdu9 >> 15) & 0xf; o->peer = get16(b + 182); o->sequence_valid = (mpdu11 >> 6) & 1;
    o->frame_valid = mpdu11 & 1; o->sequence_number = (mpdu11 >> 20) & 0xfff;
    o->encryption_valid = (mpdu11 >> 9) & 1; o->encryption_type = (mpdu9 >> 2) & 0xf;
    o->phy_ppdu_id = get16(b + 178); return 0;
}
