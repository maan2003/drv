#ifndef ATH11K_ORACLE_LINUX_KERNEL_H
#define ATH11K_ORACLE_LINUX_KERNEL_H
#include <limits.h>
#include <stdint.h>
#include <stdio.h>
#define U8_MAX UINT8_MAX
#define pr_err(...) ((void)fprintf(stderr, __VA_ARGS__))
#define ERR_PTR(error) ((void *)(intptr_t)(error))
#define IS_ERR(ptr) ((uintptr_t)(ptr) >= (uintptr_t)-4095)
#define PTR_ERR(ptr) ((long)(intptr_t)(ptr))
#endif
