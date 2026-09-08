#ifndef ATH11K_ORACLE_LINUX_TYPES_H
#define ATH11K_ORACLE_LINUX_TYPES_H
#include <stdbool.h>
#include <stddef.h>
#include <stdint.h>
#include <sys/types.h>
typedef uint8_t u8; typedef uint16_t u16; typedef uint32_t u32; typedef uint64_t u64;
typedef int8_t s8; typedef int16_t s16; typedef int32_t s32; typedef int64_t s64;
typedef uint16_t __le16; typedef uint32_t __le32; typedef uint64_t __le64;
#define __packed __attribute__((packed))
#endif
