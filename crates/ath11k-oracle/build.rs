use std::env;
use std::path::{Path, PathBuf};

const COMMIT: &str = "509ce3d952d550f93b544c8d94c99e798f09a9b4";

fn main() {
    let manifest = PathBuf::from(env::var_os("CARGO_MANIFEST_DIR").unwrap());
    let root = env::var_os("ATH11K_REFERENCE_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            manifest
                .join("../../result-ath11k/reference")
                .join(format!("linux-{COMMIT}"))
        });
    let codec = root.join("drivers/soc/qcom/qmi_encdec.c");
    if !codec.is_file() {
        panic!(
            "pinned ath11k source is missing at {}; run `nix build .#ath11k-reference-source --out-link result-ath11k` or set ATH11K_REFERENCE_DIR",
            root.display()
        );
    }
    assert_commit(&root);
    let generated = generate_qmi_oracle(&root, &manifest);
    let wmi_builders = generate_wmi_builders(&root);
    let wmi_event_pulls = generate_wmi_event_pulls(&root);
    cc::Build::new()
        .file(codec)
        .file(generated)
        .file(wmi_builders)
        .file(wmi_event_pulls)
        .file(manifest.join("c/wmi_oracle.c"))
        .file(manifest.join("c/htt_oracle.c"))
        .file(manifest.join("c/hal_oracle.c"))
        .include(manifest.join("stubs"))
        .warnings(true)
        .flag_if_supported("-std=gnu11")
        .compile("ath11k_c_oracle");
    println!("cargo:rerun-if-env-changed=ATH11K_REFERENCE_DIR");
    println!("cargo:rerun-if-changed=c/oracle.c");
    println!("cargo:rerun-if-changed=c/wmi_oracle.c");
    println!("cargo:rerun-if-changed=c/htt_oracle.c");
    println!("cargo:rerun-if-changed=c/hal_oracle.c");
    println!("cargo:rerun-if-changed=stubs");
    println!("cargo:rerun-if-changed={}", root.join("COMMIT").display());
    println!(
        "cargo:rerun-if-changed={}",
        root.join("drivers/net/wireless/ath/ath11k/wmi.c").display()
    );
    println!(
        "cargo:rerun-if-changed={}",
        root.join("drivers/net/wireless/ath/ath11k/wmi.h").display()
    );
    println!(
        "cargo:rerun-if-changed={}",
        root.join("drivers/net/wireless/ath/ath11k/ce.c").display()
    );
}

fn generate_wmi_event_pulls(root: &Path) -> PathBuf {
    let driver = root.join("drivers/net/wireless/ath/ath11k");
    let header = std::fs::read_to_string(driver.join("wmi.h")).expect("read pinned wmi.h");
    let source = std::fs::read_to_string(driver.join("wmi.c")).expect("read pinned wmi.c");
    let types = [
        "struct wmi_tlv",
        "struct wmi_ppe_threshold",
        "struct wmi_service_ready_ext_event",
        "enum wmi_start_event_param",
        "struct wmi_vdev_start_resp_event",
        "struct wmi_mac_addr",
        "struct wmi_peer_assoc_conf_event",
        "struct wmi_peer_assoc_conf_arg",
        "struct wmi_mgmt_rx_hdr",
    ]
    .map(|marker| c_item(&header, marker))
    .join("\n\n");
    let pulls = [
        c_item(&source, "static int\nath11k_wmi_tlv_iter"),
        c_item(&source, "static int ath11k_pull_svc_ready_ext"),
        c_item(&source, "static int ath11k_pull_vdev_start_resp_tlv"),
        c_item(&source, "static int ath11k_wmi_tlv_mgmt_rx_parse"),
        c_item(&source, "static int ath11k_pull_mgmt_rx_params_tlv"),
        c_item(&source, "static int ath11k_pull_peer_assoc_conf_ev"),
    ]
    .join("\n\n");
    let generated = PathBuf::from(env::var_os("OUT_DIR").unwrap()).join("wmi_event_pulls.c");
    let prelude = r#"#include <linux/types.h>
#include <errno.h>
#include <stdlib.h>
#include <string.h>

#define __packed __attribute__((packed))
#define PSOC_HOST_MAX_NUM_SS 8
#define WMI_MAX_NUM_SS 8
#define ATH_MAX_ANTENNA 4
#ifndef EPROTO
#define EPROTO 71
#endif
#define GFP_ATOMIC 0
#define WMI_TAG_VDEV_START_RESPONSE_EVENT 0x28
#define WMI_TAG_MGMT_RX_HDR 0x2c
#define WMI_TAG_ARRAY_BYTE 0x11
#define WMI_TAG_PEER_ASSOC_CONF_EVENT 0x1b2
#define WMI_TAG_MAX 0x386
#define WMI_TLV_LEN 0xffffu
#define WMI_TLV_TAG 0xffff0000u
#define FIELD_GET(mask, value) (((value) & (mask)) >> __builtin_ctz(mask))
#define ARRAY_SIZE(value) (sizeof(value) / sizeof((value)[0]))
#define IS_ERR(value) ((value) == NULL)
#define PTR_ERR(value) (-ENOMEM)
#define ath11k_warn(...) ((void)0)
#define ath11k_err(...) ((void)0)
#define kfree(value) free(value)

struct ath11k_base { int unused; };
struct ath11k_pdev_wmi { int unused; };
struct sk_buff { u8 *data; u32 len; };
struct ath11k_ppe_threshold { u32 numss_m1, ru_bit_mask, ppet16_ppet8_ru3_ru0[8]; };
struct ath11k_service_ext_param {
    u32 default_conc_scan_config_bits, default_fw_config_bits;
    struct ath11k_ppe_threshold ppet;
    u32 he_cap_info, mpdu_density, max_bssid_rx_filters, num_hw_modes, num_phy;
};
struct mgmt_rx_event_params {
    u32 chan_freq, channel, snr; u8 rssi_ctl[4]; u32 rate, phy_mode, buf_len;
    int status; u32 flags; int rssi; u32 tsf_delta; u8 pdev_id;
};
struct wmi_tlv_mgmt_rx_parse { const struct wmi_mgmt_rx_hdr *fixed;
    const u8 *frame_buf; bool frame_buf_done; };
struct wmi_tlv_policy { size_t min_len; };
static const struct wmi_tlv_policy wmi_tlv_policies[WMI_TAG_MAX] = {
    [WMI_TAG_VDEV_START_RESPONSE_EVENT] = { .min_len = 40 },
    [WMI_TAG_MGMT_RX_HDR] = { .min_len = 68 },
    [WMI_TAG_PEER_ASSOC_CONF_EVENT] = { .min_len = 12 },
};

static u32 pull32(const u8 *p) { u32 value; memcpy(&value, p, 4); return value; }
static int ath11k_wmi_tlv_iter(struct ath11k_base *, const void *, size_t,
    int (*)(struct ath11k_base *, u16, u16, const void *, void *), void *);
static int oracle_table_iter(struct ath11k_base *ab, u16 tag, u16 len,
                             const void *ptr, void *data)
{
    const void **table = data; (void)ab; (void)len;
    if (tag < WMI_TAG_MAX) table[tag] = ptr;
    return 0;
}
static const void **ath11k_wmi_tlv_parse_alloc(struct ath11k_base *ab,
                                                struct sk_buff *skb, int gfp)
{
    const void **table = calloc(WMI_TAG_MAX, sizeof(*table)); (void)ab; (void)gfp;
    if (!table) return NULL;
    if (ath11k_wmi_tlv_iter(ab, skb->data, skb->len, oracle_table_iter, table)) {
        free(table); return NULL;
    }
    return table;
}
static void *skb_pull(struct sk_buff *skb, size_t len)
{
    if (len > skb->len) return NULL;
    skb->data += len; skb->len -= len; return skb->data;
}
static void skb_trim(struct sk_buff *skb, size_t len) { skb->len = len; }
static void *skb_put(struct sk_buff *skb, size_t len) { skb->len += len; return skb->data + skb->len - len; }
static void ath11k_ce_byte_swap(void *data, u32 len) { (void)data; (void)len; }
"#;
    let wrappers = r#"
struct oracle_connection_event { u64 fields[32]; size_t field_count; u8 frame[512]; size_t frame_len; };
int oracle_wmi_connection_event(u32 kind, const u8 *bytes, size_t length,
                                struct oracle_connection_event *out)
{
    struct ath11k_base ab = {0}; struct sk_buff skb = {(u8 *)bytes, length};
    memset(out, 0, sizeof(*out));
    if (kind == 0) {
        const u8 *p = bytes; if (length < 4) return -EINVAL;
        u16 len = pull32(p); if (len < sizeof(struct wmi_service_ready_ext_event) || len > length - 4) return -EINVAL;
        struct ath11k_service_ext_param v = {0}; int ret = ath11k_pull_svc_ready_ext(NULL, p + 4, &v);
        if (ret) return ret;
        out->fields[out->field_count++] = v.default_conc_scan_config_bits;
        out->fields[out->field_count++] = v.default_fw_config_bits;
        out->fields[out->field_count++] = v.ppet.numss_m1;
        out->fields[out->field_count++] = v.ppet.ru_bit_mask;
        for (size_t i = 0; i < 8; i++) out->fields[out->field_count++] = v.ppet.ppet16_ppet8_ru3_ru0[i];
        out->fields[out->field_count++] = v.he_cap_info;
        out->fields[out->field_count++] = v.mpdu_density;
        out->fields[out->field_count++] = v.max_bssid_rx_filters;
        return 0;
    }
    if (kind == 1) {
        struct wmi_peer_assoc_conf_arg v = {0}; int ret = ath11k_pull_peer_assoc_conf_ev(&ab, &skb, &v);
        if (ret) return ret;
        out->fields[out->field_count++] = v.vdev_id;
        for (size_t i = 0; i < 6; i++) out->fields[out->field_count++] = v.macaddr[i];
        return 0;
    }
    if (kind == 2) {
        struct wmi_vdev_start_resp_event v; int ret = ath11k_pull_vdev_start_resp_tlv(&ab, &skb, &v);
        if (ret) return ret;
        u32 fields[] = {v.vdev_id, v.requestor_id, v.resp_type, v.status, v.chain_mask,
            v.smps_mode, v.mac_id, v.cfgd_tx_streams, v.cfgd_rx_streams,
            (u32)v.max_allowed_tx_power};
        for (size_t i = 0; i < 10; i++) out->fields[out->field_count++] = fields[i];
        return 0;
    }
    if (kind == 3) {
        struct mgmt_rx_event_params v = {0}; int ret = ath11k_pull_mgmt_rx_params_tlv(&ab, &skb, &v);
        if (ret) return ret;
        u32 fields[] = {v.channel, v.snr, v.rate, v.phy_mode, (u32)v.status, v.flags,
            (u32)v.rssi, v.tsf_delta, v.pdev_id, v.chan_freq};
        for (size_t i = 0; i < 10; i++) out->fields[out->field_count++] = fields[i];
        if (v.buf_len > sizeof(out->frame)) return -EINVAL;
        memcpy(out->frame, skb.data, v.buf_len); out->frame_len = v.buf_len; return 0;
    }
    return -EINVAL;
}
"#;
    std::fs::write(
        &generated,
        format!("{prelude}\n{types}\n\n{pulls}\n{wrappers}"),
    )
    .expect("write generated WMI event pull oracle translation unit");
    generated
}

fn generate_wmi_builders(root: &Path) -> PathBuf {
    let driver = root.join("drivers/net/wireless/ath/ath11k");
    let header = std::fs::read_to_string(driver.join("wmi.h")).expect("read pinned wmi.h");
    let source = std::fs::read_to_string(driver.join("wmi.c")).expect("read pinned wmi.c");
    let ce_source = std::fs::read_to_string(driver.join("ce.c")).expect("read pinned ce.c");

    let header_defines = [
        "#define PSOC_HOST_MAX_NUM_SS",
        "#define MAX_HE_NSS",
        "#define WMI_MAX_NUM_SS",
        "#define WMI_TLV_CMD(",
        "#define WMI_TLV_LEN",
        "#define WMI_TLV_TAG",
        "#define TLV_HDR_SIZE",
        "#define WMI_MAX_HECAP_PHY_SIZE",
        "#define WMI_HOST_MAX_HECAP_PHY_SIZE",
        "#define WMI_HOST_MAX_HE_RATE_SET",
        "#define WMI_MAX_SUPPORTED_RATES",
        "#define WMI_MGMT_SEND_DOWNLD_LEN",
        "#define WMI_TX_PARAMS_DWORD1_CFR_CAPTURE",
        "#define WMI_SKB_HEADROOM",
    ]
    .map(|marker| c_line(&header, marker))
    .join("\n");
    let header_items = [
        "struct wmi_cmd_hdr",
        "struct wmi_tlv",
        "enum wmi_cmd_group",
        "enum wmi_tlv_cmd_id",
        "enum wmi_tlv_peer_flags",
        "enum wmi_tlv_tag",
        "struct ath11k_ppe_threshold",
        "struct wmi_ppe_threshold",
        "struct wmi_mac_addr",
        "struct wmi_key_seq_counter",
        "struct wmi_vdev_install_key_cmd",
        "struct wmi_vdev_install_key_arg",
        "struct wmi_rate_set_arg",
        "struct peer_assoc_params",
        "struct  wmi_peer_assoc_complete_cmd",
        "struct wmi_vht_rate_set",
        "struct wmi_he_rate_set",
        "struct wmi_mgmt_send_params",
        "struct wmi_mgmt_send_cmd",
    ]
    .map(|marker| c_item(&header, marker))
    .join("\n\n");
    let source_items = [
        c_item(&source, "struct sk_buff *ath11k_wmi_alloc_skb"),
        c_item(&source, "static u32 ath11k_wmi_mgmt_get_freq"),
        c_item(&source, "int ath11k_wmi_mgmt_send"),
        c_item(&source, "int ath11k_wmi_vdev_install_key"),
        c_item(&source, "static inline void\nath11k_wmi_copy_peer_flags"),
        c_item(&source, "int ath11k_wmi_send_peer_assoc_cmd"),
        c_item(&ce_source, "void ath11k_ce_byte_swap"),
    ]
    .join("\n\n");
    let generated = PathBuf::from(env::var_os("OUT_DIR").unwrap()).join("wmi_builders.c");
    let prelude = r#"#include <linux/types.h>
#include <errno.h>
#include <stdlib.h>
#include <string.h>

#define BIT(n) (1UL << (n))
#define GENMASK(h, l) (((~0UL) << (l)) & (~0UL >> (31 - (h))))
#define FIELD_PREP(mask, value) (((u32)(value) << __builtin_ctzl(mask)) & (mask))
#define roundup(value, alignment) (((value) + (alignment) - 1) & ~((alignment) - 1))
#define sizeof_field(type, member) sizeof(((type *)0)->member)
#define lower_32_bits(value) ((u32)(value))
#define upper_32_bits(value) ((u32)((value) >> 32))
#define IS_ALIGNED(value, alignment) (((value) & ((alignment) - 1)) == 0)
#define IS_ENABLED(option) (option)
#define CONFIG_CPU_BIG_ENDIAN 0
#define ETH_ALEN 6
#define IEEE80211_TX_CTL_TX_OFFCHAN BIT(0)
#define ATH11K_FLAG_HW_CRYPTO_DISABLED 0
#define ATH11K_DBG_WMI 0
#define swab32(value) __builtin_bswap32(value)

struct ieee80211_tx_info { u32 flags; };
struct ath11k_skb_cb { u64 paddr; };
struct sk_buff {
    u8 *head;
    u8 *data;
    u32 len;
    u32 capacity;
    struct ieee80211_tx_info tx_info;
    struct ath11k_skb_cb ath11k_cb;
};
#define IEEE80211_SKB_CB(skb) (&(skb)->tx_info)
#define ATH11K_SKB_CB(skb) (&(skb)->ath11k_cb)

struct ath11k_hw_params { bool support_off_channel_tx; };
struct ath11k_base { struct ath11k_hw_params hw_params; unsigned long dev_flags; };
struct oracle_wmi_capture;
struct ath11k_wmi_base { struct ath11k_base *ab; struct oracle_wmi_capture *capture; };
struct ath11k_pdev_wmi { struct ath11k_wmi_base *wmi_ab; };
struct ath11k_scan { bool is_roc; u32 roc_freq; };
struct ath11k { struct ath11k_base *ab; struct ath11k_pdev_wmi *wmi; struct ath11k_scan scan; };

#define test_bit(bit, address) ((*(address) & BIT(bit)) != 0)
#define ether_addr_copy(destination, source) memcpy((destination), (source), ETH_ALEN)
#define ath11k_warn(...) ((void)0)
#define ath11k_dbg(...) ((void)0)

struct oracle_wmi_capture { u32 id; size_t len; u8 bytes[8192]; };

static struct sk_buff *ath11k_htc_alloc_skb(struct ath11k_base *ab, u32 len)
{
    struct sk_buff *skb = calloc(1, sizeof(*skb));
    (void)ab;
    if (!skb) return NULL;
    skb->head = calloc(1, len);
    if (!skb->head) { free(skb); return NULL; }
    skb->data = skb->head;
    skb->capacity = len;
    return skb;
}
static void skb_reserve(struct sk_buff *skb, u32 len) { skb->data += len; skb->capacity -= len; }
static void *skb_put(struct sk_buff *skb, u32 len) { void *tail = skb->data + skb->len; skb->len += len; return tail; }
static void dev_kfree_skb(struct sk_buff *skb) { if (skb) { free(skb->head); free(skb); } }
static int ath11k_wmi_cmd_send(struct ath11k_pdev_wmi *wmi, struct sk_buff *skb, u32 id)
{
    struct oracle_wmi_capture *capture = wmi->wmi_ab->capture;
    if (!capture || skb->len > sizeof(capture->bytes))
        return -EINVAL;
    capture->id = id;
    capture->len = skb->len;
    memcpy(capture->bytes, skb->data, skb->len);
    dev_kfree_skb(skb);
    return 0;
}
void ath11k_ce_byte_swap(void *mem, u32 len);
"#;
    let wrappers = r#"
static void oracle_context(struct oracle_wmi_capture *out, struct ath11k *ar,
                           struct ath11k_base *ab, struct ath11k_pdev_wmi *wmi,
                           struct ath11k_wmi_base *wmi_ab)
{
    memset(out, 0, sizeof(*out)); memset(ar, 0, sizeof(*ar)); memset(ab, 0, sizeof(*ab));
    memset(wmi, 0, sizeof(*wmi)); memset(wmi_ab, 0, sizeof(*wmi_ab));
    ar->ab = ab; ar->wmi = wmi; wmi->wmi_ab = wmi_ab; wmi_ab->ab = ab; wmi_ab->capture = out;
}

int oracle_wmi_install_key(u32 vdev_id, const u8 *address, u32 key_idx,
                           u32 key_flags, u32 cipher, u32 rsc_low, u32 rsc_high,
                           const u8 *key, size_t key_len, u32 txmic, u32 rxmic,
                           struct oracle_wmi_capture *out)
{
    struct ath11k ar; struct ath11k_base ab; struct ath11k_pdev_wmi wmi; struct ath11k_wmi_base wmi_ab;
    struct wmi_vdev_install_key_arg arg = {0};
    size_t aligned = roundup(key_len, sizeof(u32));
    u8 *padded = calloc(1, aligned ? aligned : 1);
    int ret;
    if (!padded) return -ENOMEM;
    memcpy(padded, key, key_len);
    oracle_context(out, &ar, &ab, &wmi, &wmi_ab);
    arg.vdev_id = vdev_id; arg.macaddr = address; arg.key_idx = key_idx;
    arg.key_flags = key_flags; arg.key_cipher = cipher; arg.key_len = key_len;
    arg.key_txmic_len = txmic; arg.key_rxmic_len = rxmic;
    arg.key_rsc_counter = ((u64)rsc_high << 32) | rsc_low; arg.key_data = padded;
    ret = ath11k_wmi_vdev_install_key(&ar, &arg);
    free(padded); return ret;
}

int oracle_wmi_peer_assoc(const u32 *v, const u8 *address, const u32 *ppet,
                          const u8 *legacy, size_t legacy_len,
                          const u8 *ht, size_t ht_len,
                          const u32 *he, size_t he_len, u32 flags,
                          struct oracle_wmi_capture *out)
{
    struct ath11k ar; struct ath11k_base ab; struct ath11k_pdev_wmi wmi; struct ath11k_wmi_base wmi_ab;
    struct peer_assoc_params p = {0}; size_t i;
    oracle_context(out, &ar, &ab, &wmi, &wmi_ab);
    p.vdev_id=v[0]; p.peer_new_assoc=v[1]; p.peer_associd=v[2]; p.peer_rate_caps=v[3];
    p.peer_caps=v[4]; p.peer_listen_intval=v[5]; p.peer_ht_caps=v[6]; p.peer_max_mpdu=v[7];
    p.peer_mpdu_density=v[8]; p.peer_vht_caps=v[9]; p.peer_phymode=v[10]; p.peer_nss=v[11];
    p.peer_bw_rxnss_override=v[12]; p.rx_max_rate=v[13]; p.rx_mcs_set=v[14];
    p.tx_max_rate=v[15]; p.tx_mcs_set=v[16]; p.min_data_rate=v[17];
    p.peer_he_cap_macinfo[0]=v[18]; p.peer_he_cap_macinfo[1]=v[19];
    p.peer_he_cap_macinfo_internal=v[20]; p.peer_he_caps_6ghz=v[21]; p.peer_he_ops=v[22];
    for (i=0;i<3;i++) p.peer_he_cap_phyinfo[i]=v[23+i];
    p.peer_ppet.numss_m1=v[26]; p.peer_ppet.ru_bit_mask=v[27];
    for (i=0;i<8;i++) p.peer_ppet.ppet16_ppet8_ru3_ru0[i]=ppet[i];
    memcpy(p.peer_mac, address, ETH_ALEN);
    p.peer_legacy_rates.num_rates=legacy_len; memcpy(p.peer_legacy_rates.rates, legacy, legacy_len);
    p.peer_ht_rates.num_rates=ht_len; memcpy(p.peer_ht_rates.rates, ht, ht_len);
    p.peer_he_mcs_count=he_len;
    for (i=0;i<he_len;i++) { p.peer_he_rx_mcs_set[i]=he[2*i]; p.peer_he_tx_mcs_set[i]=he[2*i+1]; }
#define FLAG(field, bit) p.field = !!(flags & BIT(bit))
    FLAG(vht_capable,0); FLAG(is_pmf_enabled,1); FLAG(is_wme_set,2); FLAG(qos_flag,3);
    FLAG(apsd_flag,4); FLAG(ht_flag,5); FLAG(bw_40,6); FLAG(bw_80,7); FLAG(bw_160,8);
    FLAG(stbc_flag,9); FLAG(ldpc_flag,10); FLAG(static_mimops_flag,11);
    FLAG(dynamic_mimops_flag,12); FLAG(spatial_mux_flag,13); FLAG(vht_flag,14);
    FLAG(he_flag,15); FLAG(twt_requester,16); FLAG(twt_responder,17); FLAG(auth_flag,18);
    FLAG(need_ptk_4_way,19); FLAG(need_gtk_2_way,20); FLAG(safe_mode_enabled,21); FLAG(is_assoc,22);
#undef FLAG
    if (flags & BIT(23)) ab.dev_flags |= BIT(ATH11K_FLAG_HW_CRYPTO_DISABLED);
    return ath11k_wmi_send_peer_assoc_cmd(&ar, &p);
}

int oracle_wmi_mgmt_send(u32 vdev_id, u32 desc_id, u32 freq, u64 paddr,
                         const u8 *frame, size_t frame_len, u8 params_valid,
                         struct oracle_wmi_capture *out)
{
    struct ath11k ar; struct ath11k_base ab; struct ath11k_pdev_wmi wmi; struct ath11k_wmi_base wmi_ab;
    struct sk_buff skb = {0};
    oracle_context(out, &ar, &ab, &wmi, &wmi_ab);
    ab.hw_params.support_off_channel_tx = true; ar.scan.is_roc = true; ar.scan.roc_freq = freq;
    skb.data = (u8 *)frame; skb.len = frame_len; skb.tx_info.flags = IEEE80211_TX_CTL_TX_OFFCHAN;
    skb.ath11k_cb.paddr = paddr;
    return ath11k_wmi_mgmt_send(&ar, vdev_id, desc_id, &skb, params_valid != 0);
}
"#;
    std::fs::write(
        &generated,
        format!("{prelude}\n{header_defines}\n\n{header_items}\n\n{source_items}\n{wrappers}"),
    )
    .expect("write generated WMI builder oracle translation unit");
    generated
}

fn c_line<'a>(text: &'a str, marker: &str) -> &'a str {
    let start = text
        .find(marker)
        .unwrap_or_else(|| panic!("pinned source lost marker {marker}"));
    let end = text[start..]
        .find('\n')
        .map(|offset| start + offset)
        .unwrap_or(text.len());
    &text[start..end]
}

fn c_item<'a>(text: &'a str, marker: &str) -> &'a str {
    let start = text
        .find(marker)
        .unwrap_or_else(|| panic!("pinned source lost marker {marker}"));
    let brace = text[start..]
        .find('{')
        .map(|offset| start + offset)
        .unwrap_or_else(|| panic!("pinned source item has no body: {marker}"));
    let mut depth = 0;
    for (offset, byte) in text.as_bytes()[brace..].iter().enumerate() {
        match byte {
            b'{' => depth += 1,
            b'}' => {
                depth -= 1;
                if depth == 0 {
                    let end = brace + offset + 1;
                    if !marker.contains('*')
                        && (marker.starts_with("struct ") || marker.starts_with("enum "))
                    {
                        let semicolon = text[end..]
                            .find(';')
                            .map(|offset| end + offset + 1)
                            .unwrap_or_else(|| {
                                panic!("pinned source item has no semicolon: {marker}")
                            });
                        return &text[start..semicolon];
                    }
                    return &text[start..end];
                }
            }
            _ => {}
        }
    }
    panic!("unterminated pinned source item: {marker}")
}

fn generate_qmi_oracle(root: &Path, manifest: &Path) -> PathBuf {
    let driver = root.join("drivers/net/wireless/ath/ath11k");
    let header = std::fs::read_to_string(driver.join("qmi.h")).expect("read pinned qmi.h");
    let source = std::fs::read_to_string(driver.join("qmi.c")).expect("read pinned qmi.c");
    let structs = between(
        &header,
        "#define QMI_WLANFW_HOST_CAP_REQ_MSG_V01_MAX_LEN",
        "int ath11k_qmi_firmware_start",
    );
    let tables = between(
        &source,
        "static const struct qmi_elem_info qmi_wlanfw_host_cap_req_msg_v01_ei[]",
        "/* clang stack usage explodes if this is inlined */",
    );
    let wrappers = std::fs::read_to_string(manifest.join("c/oracle.c")).expect("read oracle.c");
    let generated = PathBuf::from(env::var_os("OUT_DIR").unwrap()).join("qmi_oracle.c");
    let prelude = r#"#include <linux/kernel.h>
#include <linux/slab.h>
#include <linux/soc/qcom/qmi.h>
#include <stddef.h>
#include <string.h>
#define ATH11K_QMI_WLANFW_MAX_TIMESTAMP_LEN_V01 32
#define ATH11K_QMI_WLANFW_MAX_BUILD_ID_LEN_V01 128
#define ATH11K_QMI_WLANFW_MAX_NUM_MEM_SEG_V01 52
#define QMI_WLANFW_MAX_DATA_SIZE_V01 6144
"#;
    std::fs::write(
        generated.as_path(),
        format!("{prelude}\n{structs}\n{tables}\n{wrappers}"),
    )
    .expect("write generated QMI oracle translation unit");
    generated
}

fn between<'a>(text: &'a str, start: &str, end: &str) -> &'a str {
    let start = text
        .find(start)
        .unwrap_or_else(|| panic!("pinned source lost marker {start}"));
    let end = text[start..]
        .find(end)
        .map(|offset| start + offset)
        .unwrap_or_else(|| panic!("pinned source lost marker {end}"));
    &text[start..end]
}

fn assert_commit(root: &Path) {
    let actual =
        std::fs::read_to_string(root.join("COMMIT")).expect("reference must contain COMMIT");
    assert_eq!(actual.trim(), COMMIT, "wrong ath11k C reference commit");
}
