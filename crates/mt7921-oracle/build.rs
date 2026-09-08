use std::env;
use std::path::{Path, PathBuf};

const TAG: &str = "v7.1.5";
const COMMIT: &str = "155b42bec9cbb6b8cdc47dd9bd09503a81fbe493";

fn main() {
    let manifest = PathBuf::from(env::var_os("CARGO_MANIFEST_DIR").unwrap());
    let root = env::var_os("MT76_REFERENCE_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| manifest.join("../../result-mt76/reference/linux-v7.1.5"));
    if !root.join("COMMIT").is_file() {
        panic!(
            "pinned mt76 source is missing at {}; run `nix build .#mt76-reference-source --out-link result-mt76` or set MT76_REFERENCE_DIR",
            root.display()
        );
    }
    assert_identity(&root);
    let generated = generate_oracle(&root, &manifest);
    cc::Build::new()
        .file(generated)
        .warnings(true)
        .flag_if_supported("-std=gnu11")
        .compile("mt76_c_oracle");
    println!("cargo:rerun-if-env-changed=MT76_REFERENCE_DIR");
    println!("cargo:rerun-if-changed=c/oracle.c");
    println!("cargo:rerun-if-changed={}", root.join("COMMIT").display());
    println!("cargo:rerun-if-changed={}", root.join("TAG").display());
}

fn generate_oracle(root: &Path, manifest: &Path) -> PathBuf {
    let mt76 = root.join("drivers/net/wireless/mediatek/mt76");
    let mcu = std::fs::read_to_string(mt76.join("mt76_connac_mcu.c")).expect("read MCU C");
    let mt7921_mcu = std::fs::read_to_string(mt76.join("mt7921/mcu.c")).expect("read MT7921 MCU C");
    let dma = std::fs::read_to_string(mt76.join("dma.c")).expect("read DMA C");
    let mcu_function = item(
        &mcu,
        "int mt76_connac2_mcu_fill_message(",
        "EXPORT_SYMBOL_GPL(mt76_connac2_mcu_fill_message);",
    );
    let dma_function = item(
        &dma,
        "static int\nmt76_dma_add_buf(",
        "static void\nmt76_dma_tx_cleanup_idx",
    );
    let dma_rx_function = item(
        &dma,
        "static int\nmt76_dma_add_rx_buf(",
        "static int\nmt76_dma_add_buf(",
    );
    let dma_get_buf_function = item(
        &dma,
        "static void *\nmt76_dma_get_buf(",
        "static void *\nmt76_dma_dequeue(",
    );
    let dma_dequeue_function = item(
        &dma,
        "static void *\nmt76_dma_dequeue(",
        "static int\nmt76_dma_tx_queue_skb_raw(",
    );
    let dma_rx_cleanup_function = item(
        &dma,
        "static void\nmt76_dma_rx_cleanup(",
        "static void\nmt76_dma_rx_reset(",
    );
    let response_function = item(
        &mt7921_mcu,
        "int mt7921_mcu_parse_response(",
        "EXPORT_SYMBOL_GPL(mt7921_mcu_parse_response);",
    );
    let wrapper = std::fs::read_to_string(manifest.join("c/oracle.c")).expect("read wrapper");
    let generated = PathBuf::from(env::var_os("OUT_DIR").unwrap()).join("mt76_oracle.c");
    std::fs::write(
        &generated,
        format!(
            "{}\n{mcu_function}\n{dma_rx_function}\n{dma_function}\n{response_function}\n{dma_get_buf_function}\n{dma_dequeue_function}\n{dma_rx_cleanup_function}\n{wrapper}",
            prelude()
        ),
    )
    .expect("write generated oracle");
    generated
}

fn item<'a>(text: &'a str, start: &str, end: &str) -> &'a str {
    let start = text
        .find(start)
        .unwrap_or_else(|| panic!("pinned source lost {start}"));
    let end = text[start..]
        .find(end)
        .map(|n| start + n)
        .unwrap_or_else(|| panic!("pinned source lost {end}"));
    &text[start..end]
}

fn assert_identity(root: &Path) {
    let commit = std::fs::read_to_string(root.join("COMMIT")).expect("read COMMIT");
    let tag = std::fs::read_to_string(root.join("TAG")).expect("read TAG");
    assert_eq!(commit.trim(), COMMIT, "wrong mt76 reference commit");
    assert_eq!(tag.trim(), TAG, "wrong mt76 reference tag");
}

fn prelude() -> &'static str {
    r#"#include <stdbool.h>
#include <stdint.h>
#include <stddef.h>
#include <string.h>
typedef uint8_t u8; typedef uint16_t u16; typedef uint32_t u32;
typedef uint16_t __le16; typedef uint32_t __le32; typedef uint64_t dma_addr_t;
#define __packed __attribute__((packed))
#define __aligned(x) __attribute__((aligned(x)))
#define BIT(n) (1U << (n))
#define GENMASK(h, l) (((~0U) << (l)) & (~0U >> (31 - (h))))
#define __bf_shf(x) (__builtin_ffs((unsigned int)(x)) - 1)
#define FIELD_PREP(mask, val) (((u32)(val) << __bf_shf(mask)) & (mask))
#define FIELD_GET(mask, val) (((u32)(val) & (mask)) >> __bf_shf(mask))
#define cpu_to_le16(x) ((u16)(x))
#define cpu_to_le32(x) ((u32)(x))
#define WRITE_ONCE(x, val) ((x) = (val))
#define HZ 1000
#define CONFIG_ARCH_DMA_ADDR_T_64BIT 1
#define __MCU_CMD_FIELD_ID GENMASK(7, 0)
#define __MCU_CMD_FIELD_EXT_ID GENMASK(15, 8)
#define __MCU_CMD_FIELD_QUERY BIT(16)
#define __MCU_CMD_FIELD_UNI BIT(17)
#define __MCU_CMD_FIELD_CE BIT(18)
#define __MCU_CMD_FIELD_WA BIT(19)
#define MCU_CMD_FW_SCATTER 0xee
#define MCU_CMD(x) MCU_CMD_##x
#define MT_TXD0_Q_IDX GENMASK(31, 25)
#define MT_TXD0_PKT_FMT GENMASK(24, 23)
#define MT_TXD0_TX_BYTES GENMASK(15, 0)
#define MT_TXD1_LONG_FORMAT BIT(31)
#define MT_TXD1_HDR_FORMAT GENMASK(17, 16)
#define MT_TX_TYPE_CMD 2
#define MT_TX_MCU_PORT_RX_Q0 0x20
#define MT_HDR_FORMAT_CMD 1
#define MT_TX_PORT_IDX_MCU 1
#define MCU_PQ_ID(p, q) (((p) << 15) | ((q) << 10))
#define MCU_PKT_ID 0xa0
#define MCU_CMD_UNI_EXT_ACK 7
enum { MCU_Q_QUERY, MCU_Q_SET, MCU_Q_RESERVED, MCU_Q_NA };
enum { MCU_S2D_H2N, MCU_S2D_C2N, MCU_S2D_H2C, MCU_S2D_H2N_AND_H2C };
struct mt76_connac2_mcu_txd { __le32 txd[8]; __le16 len; __le16 pq_id; u8 cid; u8 pkt_type; u8 set_query; u8 seq; u8 uc_d2b0_rev; u8 ext_cid; u8 s2d_index; u8 ext_cid_ack; u32 rsv[5]; } __packed __aligned(4);
struct mt76_connac2_mcu_uni_txd { __le32 txd[8]; __le16 len; __le16 cid; u8 rsv; u8 pkt_type; u8 frag_n; u8 seq; __le16 checksum; u8 s2d_index; u8 option; u8 rsv1[4]; } __packed __aligned(4);
struct sk_buff { u8 *data; unsigned int len; };
static inline void *skb_push(struct sk_buff *skb, unsigned int len) { skb->data -= len; skb->len += len; memset(skb->data, 0, len); return skb->data; }
static inline void *skb_pull(struct sk_buff *skb, unsigned int len) { skb->data += len; skb->len -= len; return skb->data; }
struct mt76_queue;
struct mt76_dev;
struct mt76_driver_ops { int (*rx_rro_add_msdu_page)(struct mt76_dev *, struct mt76_queue *, dma_addr_t, void *); };
struct mt76_dev { void *dev, *dma_dev; struct { unsigned int timeout; u8 msg_seq; } mcu; struct { struct { int unused; } wed; } mmio; struct mt76_queue *q_rx; struct mt76_driver_ops *drv; };
struct mt76_connac2_mcu_rxd { __le32 rxd[6]; __le16 len; __le16 pkt_type_id; u8 eid; u8 seq; u8 option; u8 rsv; u8 ext_eid; u8 rsv1[2]; u8 s2d_index; u8 tlv[]; } __packed __aligned(4);
struct mt76_desc { __le32 buf0; __le32 ctrl; __le32 buf1; __le32 info; } __packed __aligned(4);
struct mt76_wed_rro_desc { __le32 buf0; __le32 buf1; };
struct mt76_txwi_cache { int qid; dma_addr_t dma_addr; void *ptr; };
struct mt76_queue_buf { dma_addr_t addr; u16 len; bool skip_unmap; };
struct mt76_queue_entry { bool skip_buf0, skip_buf1; dma_addr_t dma_addr[2]; u16 dma_len[2]; void *txwi; void *skb; void *buf; u16 wcid; };
struct mt76_queue { int head, tail, ndesc, queued; u32 flags, magic_cnt; unsigned int buf_size; void *page_pool, *rx_head; int lock; struct mt76_desc *desc; struct mt76_queue_entry *entry; };
static inline bool mt76_queue_is_wed_rro_ind(struct mt76_queue *q) { (void)q; return false; }
static inline bool mt76_queue_is_wed_rro_rxdmad_c(struct mt76_queue *q) { (void)q; return false; }
static inline bool mt76_queue_is_wed_rx(struct mt76_queue *q) { (void)q; return false; }
static inline bool mt76_queue_is_wed_rro_data(struct mt76_queue *q) { (void)q; return false; }
static inline bool mt76_queue_is_wed_rro_msdu_pg(struct mt76_queue *q) { (void)q; return false; }
static inline bool mt76_queue_is_wed_rro(struct mt76_queue *q) { (void)q; return false; }
static inline bool mt76_queue_is_npu(struct mt76_queue *q) { (void)q; return false; }
static inline struct mt76_txwi_cache *mt76_get_rxwi(struct mt76_dev *d) { (void)d; return NULL; }
static inline struct mt76_txwi_cache *mt76_rx_token_release(struct mt76_dev *d, u32 token) { (void)d; (void)token; return NULL; }
static inline int mt76_rx_token_consume(struct mt76_dev *d, void *a, void *b, dma_addr_t c) { (void)d; (void)a; (void)b; (void)c; return -1; }
static inline void mt76_put_rxwi(struct mt76_dev *d, void *p) { (void)d; (void)p; }
#define ENOMEM 12
#define EAGAIN 11
#define ETIMEDOUT 110
#define le16_to_cpu(x) ((u16)(x))
#define le32_to_cpu(x) ((u32)(x))
#define dev_err(...) ((void)0)
static inline void mt792x_reset(struct mt76_dev *dev) { (void)dev; }
#define MCU_CMD_PATCH_SEM_CONTROL 0x10
#define MCU_CMD_PATCH_FINISH_REQ 0x07
#define MCU_CMD_THERMAL_CTRL 0x2c
#define MCU_CMD_DEV_INFO_UPDATE 0x01
#define MCU_CMD_BSS_INFO_UPDATE 0x02
#define MCU_CMD_STA_REC_UPDATE 0x03
#define MCU_CMD_SUSPEND 0x05
#define MCU_CMD_OFFLOAD 0x06
#define MCU_CMD_HIF_CTRL 0x07
#define MCU_CMD_REG_READ 0xc0
#define MCU_CMD_WF_RF_PIN_CTRL 0xbd
#define MCU_EXT_CMD(x) (MCU_CMD_##x | __MCU_CMD_FIELD_EXT_ID)
#define MCU_UNI_CMD(x) (MCU_CMD_##x | __MCU_CMD_FIELD_UNI)
#define MCU_CE_QUERY(x) (MCU_CMD_##x | __MCU_CMD_FIELD_CE | __MCU_CMD_FIELD_QUERY)
struct mt76_connac_mcu_uni_event { u8 cid; u8 pad[3]; __le32 status; } __packed;
struct mt76_connac_mcu_reg_event { __le32 reg; __le32 val; } __packed;
struct mt7921_wf_rf_pin_ctrl_event { u8 result; } __packed;
#define MT_QFLAG_WED_RRO_EN BIT(0)
#define MT_DMA_CTL_TOKEN GENMASK(31, 16)
#define MT_DMA_CTL_TO_HOST BIT(8)
#define MT_DMA_CTL_WO_DROP BIT(9)
#define MT_DMA_CTL_DMA_DONE BIT(31)
#define MT_DMA_MAGIC_MASK GENMASK(3, 0)
#define MT_DMA_MAGIC_CNT 16
#define DMA_DUMMY_DATA ((void *)1)
#define MT_DMA_CTL_SD_LEN0 GENMASK(29, 16)
#define MT_DMA_CTL_LAST_SEC0 BIT(30)
#define MT_DMA_CTL_SD_LEN1 GENMASK(13, 0)
#define MT_DMA_CTL_LAST_SEC1 BIT(14)
#define MT_DMA_CTL_SDP0_H GENMASK(3, 0)
#define MT_DMA_CTL_SDP1_H GENMASK(19, 16)
#define MT_TXD_LEN_LAST BIT(15)
#define MT_MSDU_ID_VALID BIT(15)
#define MT_HDR_FORMAT_802_3 0
#define MT_HDR_FORMAT_802_11 2
#define MT_TX_TYPE_CT 0
#define MT_LMAC_ALTX0 0x10
#define MT_TXD1_WLAN_IDX GENMASK(9, 0)
#define MT_TXD1_HDR_INFO GENMASK(15, 11)
#define MT_TXD1_ETH_802_3 BIT(15)
#define MT_TXD1_HDR_FORMAT GENMASK(17, 16)
#define MT_TXD1_TID GENMASK(22, 20)
#define MT_TXD2_SUB_TYPE GENMASK(3, 0)
#define MT_TXD2_FRAME_TYPE GENMASK(5, 4)
#define MT_TXD2_HTC_VLD BIT(13)
#define MT_TXD2_FIX_RATE BIT(31)
#define MT_TXD3_PROTECT_FRAME BIT(1)
#define MT_TXD3_REM_TX_COUNT GENMASK(15, 11)
#define MT_TXD3_BA_DISABLE BIT(28)
#define MT_TXD5_PID GENMASK(7, 0)
#define MT_TXD5_TX_STATUS_HOST BIT(10)
#define MT_TXD6_FIXED_BW BIT(2)
#define MT_TXD6_TX_RATE GENMASK(29, 16)
#define MT_TXD7_SUB_TYPE GENMASK(19, 16)
#define MT_TXD7_TYPE GENMASK(21, 20)
#define MT_RXD0_PKT_FLAG GENMASK(19, 16)
#define MT_RXD0_PKT_TYPE GENMASK(31, 27)
#define MT_RXD1_NORMAL_GROUP_1 BIT(11)
#define MT_RXD1_NORMAL_GROUP_2 BIT(12)
#define MT_RXD1_NORMAL_GROUP_3 BIT(13)
#define MT_RXD1_NORMAL_GROUP_4 BIT(14)
#define MT_RXD1_NORMAL_GROUP_5 BIT(15)
#define MT_RXD2_NORMAL_HDR_OFFSET GENMASK(15, 14)
#define MT_RXD3_NORMAL_CH_FREQ GENMASK(15, 8)
#define MT_PRXV_RCPI0 GENMASK(7, 0)
#define MT_PRXV_RCPI1 GENMASK(15, 8)
static inline void *mt76_dma_get_rxdmad_c_buf(struct mt76_dev *d, struct mt76_queue *q, int i, int *l, bool *m) { (void)d; (void)q; (void)i; (void)l; (void)m; return NULL; }
static inline void mt76_dma_should_drop_buf(bool *drop, u32 ctrl, u32 buf1, u32 info) { (void)drop; (void)ctrl; (void)buf1; (void)info; }
#define READ_ONCE(x) (x)
#define SKB_WITH_OVERHEAD(x) (x)
#define DMA_FROM_DEVICE 0
static inline int page_pool_get_dma_dir(void *p) { (void)p; return 0; }
static inline void dma_sync_single_for_cpu(void *d, dma_addr_t a, unsigned int l, int dir) { (void)d; (void)a; (void)l; (void)dir; }
#define RRO_IND_DATA1_MAGIC_CNT_MASK GENMASK(3, 0)
#define RRO_RXDMAD_DATA3_MAGIC_CNT_MASK GENMASK(3, 0)
#define MT_DMA_WED_IND_CMD_CNT 16
struct mt76_wed_rro_ind { __le32 data1; };
struct mt76_rro_rxdmad_c { __le32 data3; };
#define spin_lock_bh(x) ((void)(x))
#define spin_unlock_bh(x) ((void)(x))
static unsigned int oracle_released_buffers;
static inline void mt76_put_page_pool_buf(void *p, bool allow_direct) { (void)p; (void)allow_direct; oracle_released_buffers++; }
static inline void mt76_npu_queue_cleanup(struct mt76_dev *d, struct mt76_queue *q) { (void)d; (void)q; }
static inline bool mt76_npu_device_active(struct mt76_dev *d) { (void)d; return false; }
static inline bool mtk_wed_device_active(void *wed) { (void)wed; return false; }
static inline void dev_kfree_skb(void *p) { (void)p; }
"#
}
