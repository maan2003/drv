#include <linux/kernel.h>
#include <linux/slab.h>
#include <linux/soc/qcom/qmi.h>
#include <stddef.h>
#include <string.h>

struct oracle_host_cap { u8 num_clients_valid; u32 num_clients; };
struct oracle_event { u8 kind; u8 tag; u16 reserved; size_t offset; size_t len; u64 value; };

/* This is the exact prefix of qmi_wlanfw_host_cap_req_msg_v01_ei used when
 * only num_clients is populated. Keeping the schema here avoids compiling
 * ath11k qmi.c's hardware lifecycle into the protocol oracle. */
static const struct qmi_elem_info host_cap_num_clients_ei[] = {
 { QMI_OPT_FLAG, 1, sizeof(u8), NO_ARRAY, 0x10, offsetof(struct oracle_host_cap, num_clients_valid), NULL },
 { QMI_UNSIGNED_4_BYTE, 1, sizeof(u32), NO_ARRAY, 0x10, offsetof(struct oracle_host_cap, num_clients), NULL },
 { QMI_EOTI, 0, 0, NO_ARRAY, 0, 0, NULL },
};
static const struct qmi_elem_info standard_response_ei[] = {
 { QMI_STRUCT, 1, sizeof(struct qmi_response_type_v01), NO_ARRAY, 0x02, 0,
   qmi_response_type_v01_ei },
 { QMI_EOTI, 0, 0, NO_ARRAY, 0, 0, NULL },
};

int oracle_qmi_host_cap_encode(u8 present, u32 value, u8 *out, size_t capacity,
                               struct oracle_event *events, size_t *event_count) {
    struct oracle_host_cap input = { .num_clients_valid = present, .num_clients = value };
    size_t len = capacity;
    void *message = qmi_encode_message(0, 0x34, &len, 1,
                                       host_cap_num_clients_ei, &input);
    if (IS_ERR(message)) return (int)PTR_ERR(message);
    if (len < sizeof(struct qmi_header) || len - sizeof(struct qmi_header) > capacity) {
        kfree(message);
        return -22;
    }
    len -= sizeof(struct qmi_header);
    memcpy(out, (u8 *)message + sizeof(struct qmi_header), len);
    if (events && event_count) {
        size_t n = 0;
        if (present) {
            events[n++] = (struct oracle_event){ .kind = 1, .tag = 0x10, .offset = 0, .len = 4 };
            events[n++] = (struct oracle_event){ .kind = 2, .tag = 0x10, .offset = 3, .len = 4, .value = value };
        }
        *event_count = n;
    }
    kfree(message);
    return (int)len;
}

int oracle_qmi_response_decode(const u8 *body, size_t body_len, u16 *result, u16 *error) {
    size_t total = sizeof(struct qmi_header) + body_len;
    /* qmi_decode() reads a TLV header before validating its logical length.
     * Padding is outside `total`, so it prevents a userspace OOB read without
     * changing the length observed by the pinned codec. */
    u8 *message = kzalloc(total + 3, GFP_KERNEL);
    struct qmi_response_type_v01 output = {0};
    int rc;
    if (!message) return -12;
    memcpy(message + sizeof(struct qmi_header), body, body_len);
    rc = qmi_decode_message(message, total, standard_response_ei, &output);
    if (rc >= 0) { *result = output.result; *error = output.error; }
    kfree(message);
    return rc;
}
