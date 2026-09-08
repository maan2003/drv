_Static_assert(sizeof(struct mt76_connac2_mcu_txd) == 64,
               "legacy MCU TXD layout changed");
_Static_assert(sizeof(struct mt76_desc) == 16, "DMA descriptor layout changed");

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
