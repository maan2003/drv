/* Included after the protocol structs and elem_info tables extracted verbatim
 * from the pinned ath11k qmi.[ch] by build.rs. */

struct oracle_event { u8 kind; u8 tag; u16 reserved; size_t offset; size_t len; u64 value; };

static int oracle_encode(const struct qmi_elem_info *ei, const void *input,
                         unsigned int message_id, size_t maximum,
                         u8 *out, size_t capacity) {
    size_t len = maximum;
    void *message = qmi_encode_message(0, message_id, &len, 1, ei, input);
    if (IS_ERR(message)) return (int)PTR_ERR(message);
    if (len < sizeof(struct qmi_header) || len - sizeof(struct qmi_header) > capacity) {
        kfree(message);
        return -22;
    }
    len -= sizeof(struct qmi_header);
    memcpy(out, (u8 *)message + sizeof(struct qmi_header), len);
    kfree(message);
    return (int)len;
}

int oracle_qmi_host_cap_encode(const struct qmi_wlanfw_host_cap_req_msg_v01 *input,
                               u8 *out, size_t capacity) {
    return oracle_encode(qmi_wlanfw_host_cap_req_msg_v01_ei, input,
                         QMI_WLANFW_HOST_CAP_REQ_V01,
                         QMI_WLANFW_HOST_CAP_REQ_MSG_V01_MAX_LEN, out, capacity);
}

int oracle_qmi_ind_register_encode(const struct qmi_wlanfw_ind_register_req_msg_v01 *input,
                                   u8 *out, size_t capacity) {
    return oracle_encode(qmi_wlanfw_ind_register_req_msg_v01_ei, input,
                         QMI_WLANFW_IND_REGISTER_REQ_V01,
                         QMI_WLANFW_IND_REGISTER_REQ_MSG_V01_MAX_LEN, out, capacity);
}

int oracle_qmi_respond_memory_encode(const struct qmi_wlanfw_respond_mem_req_msg_v01 *input,
                                     u8 *out, size_t capacity) {
    return oracle_encode(qmi_wlanfw_respond_mem_req_msg_v01_ei, input,
                         QMI_WLANFW_RESPOND_MEM_REQ_V01,
                         QMI_WLANFW_RESPOND_MEM_REQ_MSG_V01_MAX_LEN, out, capacity);
}

int oracle_qmi_bdf_download_encode(const struct qmi_wlanfw_bdf_download_req_msg_v01 *input,
                                   u8 *out, size_t capacity) {
    return oracle_encode(qmi_wlanfw_bdf_download_req_msg_v01_ei, input,
                         QMI_WLANFW_BDF_DOWNLOAD_REQ_V01,
                         QMI_WLANFW_BDF_DOWNLOAD_REQ_MSG_V01_MAX_LEN, out, capacity);
}

int oracle_qmi_m3_info_encode(const struct qmi_wlanfw_m3_info_req_msg_v01 *input,
                              u8 *out, size_t capacity) {
    return oracle_encode(qmi_wlanfw_m3_info_req_msg_v01_ei, input,
                         QMI_WLANFW_M3_INFO_REQ_V01,
                         QMI_WLANFW_M3_INFO_REQ_MSG_V01_MAX_MSG_LEN, out, capacity);
}

int oracle_qmi_wlan_mode_encode(const struct qmi_wlanfw_wlan_mode_req_msg_v01 *input,
                                u8 *out, size_t capacity) {
    return oracle_encode(qmi_wlanfw_wlan_mode_req_msg_v01_ei, input,
                         QMI_WLANFW_WLAN_MODE_REQ_V01,
                         QMI_WLANFW_WLAN_MODE_REQ_MSG_V01_MAX_LEN, out, capacity);
}

int oracle_qmi_wlan_config_encode(const struct qmi_wlanfw_wlan_cfg_req_msg_v01 *input,
                                  u8 *out, size_t capacity) {
    return oracle_encode(qmi_wlanfw_wlan_cfg_req_msg_v01_ei, input,
                         QMI_WLANFW_WLAN_CFG_REQ_V01,
                         QMI_WLANFW_WLAN_CFG_REQ_MSG_V01_MAX_LEN, out, capacity);
}

int oracle_qmi_wlan_ini_encode(const struct qmi_wlanfw_wlan_ini_req_msg_v01 *input,
                               u8 *out, size_t capacity) {
    return oracle_encode(qmi_wlanfw_wlan_ini_req_msg_v01_ei, input,
                         QMI_WLANFW_WLAN_INI_REQ_V01,
                         QMI_WLANFW_WLAN_INI_REQ_MSG_V01_MAX_LEN, out, capacity);
}

int oracle_qmi_empty_encode(unsigned int kind, u8 *out, size_t capacity) {
    struct qmi_wlanfw_cap_req_msg_v01 input = {0};
    const struct qmi_elem_info *ei = kind == 0 ? qmi_wlanfw_cap_req_msg_v01_ei
                                                : qmi_wlanfw_device_info_req_msg_v01_ei;
    unsigned int id = kind == 0 ? QMI_WLANFW_CAP_REQ_V01 : QMI_WLANFW_DEVICE_INFO_REQ_V01;
    return oracle_encode(ei, &input, id, 0, out, capacity);
}

int oracle_qmi_decode_reencode(const u8 *body, size_t body_len, unsigned int kind,
                               void *output, size_t output_len,
                               u8 *encoded, size_t capacity) {
    const struct qmi_elem_info *ei;
    size_t maximum;
    size_t total = sizeof(struct qmi_header) + body_len;
    u8 *message = kzalloc(total + 3, GFP_KERNEL);
    int rc;
    if (!message) return -12;
    memset(output, 0, output_len);
    memcpy(message + sizeof(struct qmi_header), body, body_len);
    switch (kind) {
    case 0: ei = qmi_wlanfw_host_cap_resp_msg_v01_ei; maximum = QMI_WLANFW_HOST_CAP_RESP_MSG_V01_MAX_LEN; break;
    case 1: ei = qmi_wlanfw_ind_register_resp_msg_v01_ei; maximum = QMI_WLANFW_IND_REGISTER_RESP_MSG_V01_MAX_LEN; break;
    case 2: ei = qmi_wlanfw_cap_resp_msg_v01_ei; maximum = QMI_WLANFW_CAP_RESP_MSG_V01_MAX_LEN; break;
    case 3: ei = qmi_wlfw_device_info_resp_msg_v01_ei; maximum = QMI_WLANFW_CAP_RESP_MSG_V01_MAX_LEN; break;
    case 4: ei = qmi_wlanfw_request_mem_ind_msg_v01_ei; maximum = QMI_WLANFW_REQUEST_MEM_IND_MSG_V01_MAX_LEN; break;
    case 5: ei = qmi_wlanfw_mem_ready_ind_msg_v01_ei; maximum = 0; break;
    case 6: ei = qmi_wlanfw_fw_ready_ind_msg_v01_ei; maximum = 0; break;
    case 7: ei = qmi_wlanfw_cold_boot_cal_done_ind_msg_v01_ei; maximum = 0; break;
    case 8: ei = qmi_wlfw_fw_init_done_ind_msg_v01_ei; maximum = 0; break;
    default: kfree(message); return -22;
    }
    rc = qmi_decode_message(message, total, ei, output);
    kfree(message);
    if (rc < 0) return rc;
    return oracle_encode(ei, output, 0, maximum, encoded, capacity);
}
