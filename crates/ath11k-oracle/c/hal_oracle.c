/* Typed descriptor oracle transcribed from the pinned ath11k hal_desc.h and
 * hal_{rx,tx}.c.  The Rust boundary only supplies values valid for each field. */
#include <stdint.h>
#include <string.h>

typedef uint8_t u8;
typedef uint16_t u16;
typedef uint32_t u32;
typedef uint64_t u64;

#define PREP(mask, value) ((((u32)(value)) << __builtin_ctz(mask)) & (mask))
#define GET(mask, value) (((value) & (mask)) >> __builtin_ctz(mask))
#define PACKED __attribute__((packed))

#define ADDR_LO 0xffffffffu
#define ADDR_HI 0x000000ffu
#define RBM 0x00000700u
#define COOKIE 0xfffff800u

struct buffer_addr { u32 info0, info1; } PACKED;
struct tcl_data { struct buffer_addr addr; u32 info0, info1, info2, info3, info4; } PACKED;
struct reo_entrance { struct buffer_addr addr; u32 mpdu[2], queue_lo, info0, info1, info2; } PACKED;
struct reo_destination { struct buffer_addr addr; u32 mpdu[2], msdu[2], queue_lo, info0, info1, reserved[6], info2; } PACKED;
struct wbm_release { struct buffer_addr addr; u32 info0, info1, info2, rate[2], info3; } PACKED;
struct ce_source { u32 addr_lo, addr_info, meta, flags; } PACKED;
struct ce_destination { u32 addr_lo, addr_info; } PACKED;
struct ce_status { u32 flags, hash0, hash1, meta; } PACKED;
struct reo_cmd { u32 tlv, header, address_lo, info0, info1, info2, pn[4]; } PACKED;

_Static_assert(sizeof(struct tcl_data) == 28, "hal_tcl_data_cmd layout");
_Static_assert(sizeof(struct reo_entrance) == 32, "hal_reo_entrance_ring layout");
_Static_assert(sizeof(struct reo_destination) == 64, "hal_reo_dest_ring layout");
_Static_assert(sizeof(struct wbm_release) == 32, "hal_wbm_release_ring layout");
_Static_assert(sizeof(struct reo_cmd) == 40, "hal_reo command layout");

static void set_addr(struct buffer_addr *out, u64 address, u32 cookie, u8 manager) {
    out->info0 = PREP(ADDR_LO, address);
    out->info1 = PREP(ADDR_HI, address >> 32) | PREP(RBM, manager) | PREP(COOKIE, cookie);
}

void oracle_hal_tx_setup(u8 out[28], u64 address, u16 metadata, u32 id,
                         u8 type, u8 encap, u8 encrypt, u32 len, u32 offset,
                         u32 flags0, u32 flags1, u16 addr_flags, u32 ast_index,
                         u16 ast_hash, u8 tid, u8 search, u8 lmac, u8 dscp,
                         u8 mesh, u8 manager) {
    struct tcl_data d = {0};
    set_addr(&d.addr, address, id, manager);
    d.info0 = PREP(0x1, type) | PREP(0xc, encap) | PREP(0xf0, encrypt) |
              PREP(0x3000, search) | PREP(0xc000, addr_flags) |
              PREP(0xffff0000, metadata);
    d.info1 = flags0 | PREP(0xffff, len) | PREP(0xff800000, offset);
    d.info2 = flags1 | PREP(0x03c00000, tid) | PREP(0x0c000000, lmac);
    d.info3 = PREP(0x3f, dscp) | PREP(0x03ffffc0, ast_index) |
              PREP(0x3c000000, ast_hash);
    /* WCN6750 uses the QCN9074 tx_mesh_enable callback. */
    if (mesh) d.info3 |= 0x40000000;
    memcpy(out, &d, sizeof(d));
}

static u32 reo_tlv(u32 tag) {
    return PREP(0x000003fe, tag) | PREP(0x03fffc00, 36);
}

void oracle_hal_reo_queue_stats(u8 out[40], u16 number, u64 address, u32 flags) {
    struct reo_cmd d = {0};
    d.tlv = reo_tlv(306);
    d.header = number | ((flags & 1) ? 0x10000 : 0);
    d.address_lo = (u32)address;
    d.info0 = PREP(0xff, address >> 32) | ((flags & 2) ? 0x100 : 0);
    memcpy(out, &d, sizeof(d));
}

int oracle_hal_reo_flush_cache(u8 out[40], u16 number, u64 address, u32 flags,
                               u8 available, u8 *current) {
    struct reo_cmd d = {0};
    u8 slot = (u8)__builtin_ctz((u32)(~available) & 0xff);
    if ((flags & 4) && slot >= 3)
        return -1;
    d.tlv = reo_tlv(308);
    d.header = number | ((flags & 1) ? 0x10000 : 0);
    d.address_lo = (u32)address;
    d.info0 = PREP(0xff, address >> 32);
    if (flags & 0x20) d.info0 |= 0x100;
    if (flags & 4) {
        *current = slot;
        d.info0 |= 0x2000 | PREP(0xc00, slot);
    }
    if (flags & 0x10) d.info0 |= 0x1000;
    if (flags & 0x40) d.info0 |= 0x4000;
    memcpy(out, &d, sizeof(d));
    return 0;
}

void oracle_hal_reo_update_rx_queue(u8 out[40], u16 number, u64 address,
                                    u32 flags, u32 update0, u32 update1,
                                    u32 update2, const u32 pn[4], u16 rxq,
                                    u16 ba_window, u8 pn_size) {
    struct reo_cmd d = {0};
    d.tlv = reo_tlv(419);
    d.header = number | ((flags & 1) ? 0x10000 : 0);
    d.address_lo = (u32)address;
    /* The pinned source has no UPD_PN_ERR term in info0. */
    d.info0 = PREP(0xff, address >> 32) | (update0 & 0x6fffff00);
    d.info1 = rxq | (update1 & 0xffff0000);
    if (pn_size == 24) pn_size = 0;
    else if (pn_size == 48) pn_size = 1;
    else if (pn_size == 128) pn_size = 2;
    if (ba_window < 1) ba_window = 1;
    if (ba_window == 1) ba_window++;
    d.info2 = PREP(0xff, ba_window - 1) | PREP(0x300, pn_size) |
              (update2 & 0x01fffc00);
    memcpy(d.pn, pn, sizeof(d.pn));
    memcpy(out, &d, sizeof(d));
}

void oracle_hal_rx_buffer(u8 out[8], u64 address, u32 cookie, u8 manager) {
    struct buffer_addr d;
    set_addr(&d, address, cookie, manager);
    memcpy(out, &d, sizeof(d));
}

void oracle_hal_rx_buffer_get(const u8 input[8], u64 *address, u32 *cookie, u8 *manager) {
    struct buffer_addr d;
    memcpy(&d, input, sizeof(d));
    *address = ((u64)GET(ADDR_HI, d.info1) << 32) | GET(ADDR_LO, d.info0);
    *cookie = GET(COOKIE, d.info1);
    *manager = GET(RBM, d.info1);
}

void oracle_hal_reo_entrance(u8 out[32], u64 address, u32 cookie, u8 manager,
                             u8 msdus, u64 queue, u16 bytes, u8 destination,
                             u8 frameless, u8 reason, u8 error, u8 ring, u8 loop) {
    struct reo_entrance d = {0};
    set_addr(&d.addr, address, cookie, manager);
    d.mpdu[0] = PREP(0xff, msdus);
    d.queue_lo = (u32)queue;
    d.info0 = PREP(0xff, queue >> 32) | PREP(0x003fff00, bytes) |
              PREP(0x07c00000, destination) | (frameless ? 0x08000000 : 0);
    d.info1 = PREP(0x3, reason) | PREP(0x7c, error);
    d.info2 = PREP(0x0ff00000, ring) | PREP(0xf0000000, loop);
    memcpy(out, &d, sizeof(d));
}

void oracle_hal_reo_destination(u8 out[64], u64 address, u32 cookie, u8 manager,
                                u64 queue, u8 type, u8 reason, u8 error, u16 rxq,
                                u8 valid, u8 opcode, u8 slot, u8 ring, u8 loop) {
    struct reo_destination d = {0};
    set_addr(&d.addr, address, cookie, manager);
    d.queue_lo = (u32)queue;
    d.info0 = PREP(0xff, queue >> 32) | PREP(0x100, type) |
              PREP(0x600, reason) | PREP(0xf800, error) | PREP(0xffff0000, rxq);
    d.info1 = (valid ? 1 : 0) | PREP(0x1e, opcode) | PREP(0x1fe0, slot);
    d.info2 = PREP(0x0ff00000, ring) | PREP(0xf0000000, loop);
    memcpy(out, &d, sizeof(d));
}

void oracle_hal_wbm_release(u8 out[32], u64 address, u32 cookie, u8 manager,
                            u8 source, u8 action, u8 type, u8 index, u8 tqm_reason,
                            u8 rx_reason, u8 rx_error, u8 reo_reason, u8 reo_error,
                            u8 internal, u32 status, u8 count, u8 rssi, u8 valid,
                            u8 first, u8 last, u8 amsdu, u8 notification,
                            u32 timestamp, u16 peer, u8 tid, u8 ring, u8 loop) {
    struct wbm_release d = {0};
    set_addr(&d.addr, address, cookie, manager);
    d.info0 = PREP(0x7, source) | PREP(0x38, action) | PREP(0x1c0, type) |
      PREP(0x1e00, index) | PREP(0x1e000, tqm_reason) | PREP(0x60000, rx_reason) |
      PREP(0xf80000, rx_error) | PREP(0x03000000, reo_reason) |
      PREP(0x7c000000, reo_error) | (internal ? 0x80000000 : 0);
    d.info1 = PREP(0x00ffffff, status) | PREP(0x7f000000, count);
    d.info2 = PREP(0xff, rssi) | (valid ? 0x100 : 0) | (first ? 0x200 : 0) |
      (last ? 0x400 : 0) | (amsdu ? 0x800 : 0) | (notification ? 0x1000 : 0) |
      PREP(0xffffe000, timestamp);
    d.info3 = PREP(0xffff, peer) | PREP(0x000f0000, tid) |
      PREP(0x0ff00000, ring) | PREP(0xf0000000, loop);
    memcpy(out, &d, sizeof(d));
}

void oracle_hal_wbm_msdu_link(u8 out[32], const u8 source[32], u8 action) {
    struct wbm_release d = {0}, s;
    memcpy(&s, source, sizeof(s));
    d.addr = s.addr;
    d.info0 |= PREP(0x7, 4) | PREP(0x38, action) | PREP(0x1c0, 1);
    memcpy(out, &d, sizeof(d));
}

void oracle_hal_ce_source(u8 out[16], u64 address, u32 len, u32 id, u8 swap) {
    struct ce_source d = {0};
    d.addr_lo = (u32)address;
    d.addr_info = PREP(0xff, address >> 32) | (swap ? 0x200 : 0) | PREP(0xffff0000, len);
    d.meta = PREP(0xffff, id);
    memcpy(out, &d, sizeof(d));
}
void oracle_hal_ce_destination(u8 out[8], u64 address) {
    struct ce_destination d = {(u32)address, PREP(0xff, address >> 32)};
    memcpy(out, &d, sizeof(d));
}
u32 oracle_hal_ce_status_take_length(u8 inout[16]) {
    struct ce_status d;
    memcpy(&d, inout, sizeof(d));
    u32 len = GET(0xffff0000, d.flags);
    d.flags &= ~0xffff0000u;
    memcpy(inout, &d, sizeof(d));
    return len;
}

void oracle_hal_rx_wcn6750_fields(u8 mpdu[92], u16 peer, u16 length,
                                  u8 duration[56], u32 usecs) {
    u32 word;
    memset(mpdu, 0, 92);
    /* WCN6750 monitor parsing selects hal_rx_mpdu_info_ipq8074. */
    word = PREP(0xffff0000, peer);
    memcpy(mpdu + 4, &word, sizeof(word));
    word = PREP(0x3fff, length);
    memcpy(mpdu + 52, &word, sizeof(word));
    memset(duration, 0, 56);
    word = PREP(0x00ffffff, usecs);
    memcpy(duration + 36, &word, sizeof(word));
}
