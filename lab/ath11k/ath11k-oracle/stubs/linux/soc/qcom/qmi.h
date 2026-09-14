#ifndef ATH11K_ORACLE_QMI_H
#define ATH11K_ORACLE_QMI_H
#include <linux/types.h>
#include <stddef.h>
#if __BYTE_ORDER__ == __ORDER_LITTLE_ENDIAN__
#define __cpu_to_le16(v) ((u16)(v))
#define __cpu_to_le32(v) ((u32)(v))
#define __cpu_to_le64(v) ((u64)(v))
#define __le16_to_cpu(v) ((u16)(v))
#define __le32_to_cpu(v) ((u32)(v))
#define __le64_to_cpu(v) ((u64)(v))
#else
#define __cpu_to_le16(v) __builtin_bswap16(v)
#define __cpu_to_le32(v) __builtin_bswap32(v)
#define __cpu_to_le64(v) __builtin_bswap64(v)
#define __le16_to_cpu(v) __builtin_bswap16(v)
#define __le32_to_cpu(v) __builtin_bswap32(v)
#define __le64_to_cpu(v) __builtin_bswap64(v)
#endif
#define cpu_to_le16(v) __cpu_to_le16(v)
struct qmi_header { u8 type; __le16 txn_id; __le16 msg_id; __le16 msg_len; } __packed;
enum qmi_elem_type { QMI_EOTI, QMI_OPT_FLAG, QMI_DATA_LEN, QMI_UNSIGNED_1_BYTE,
 QMI_UNSIGNED_2_BYTE, QMI_UNSIGNED_4_BYTE, QMI_UNSIGNED_8_BYTE,
 QMI_SIGNED_2_BYTE_ENUM, QMI_SIGNED_4_BYTE_ENUM, QMI_STRUCT, QMI_STRING };
enum qmi_array_type { NO_ARRAY, STATIC_ARRAY, VAR_LEN_ARRAY };
struct qmi_elem_info { enum qmi_elem_type data_type; u32 elem_len; u32 elem_size;
 enum qmi_array_type array_type; u8 tlv_type; u32 offset;
 const struct qmi_elem_info *ei_array; };
struct qmi_response_type_v01 { u16 result; u16 error; };
#define QMI_COMMON_TLV_TYPE 0
extern const struct qmi_elem_info qmi_response_type_v01_ei[];
void *qmi_encode_message(int type, unsigned int msg_id, size_t *len,
 unsigned int txn_id, const struct qmi_elem_info *ei, const void *c_struct);
int qmi_decode_message(const void *buf, size_t len,
 const struct qmi_elem_info *ei, void *c_struct);
#endif
