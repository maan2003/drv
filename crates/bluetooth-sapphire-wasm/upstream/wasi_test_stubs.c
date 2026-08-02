// Project test-runtime glue for Pigweed commit
// c14c119c51a82f6e044f81b7dad0a322091d4121. This file does not modify
// Sapphire behavior; it replaces ambient WASI process and descriptor imports
// with the bounded test host capabilities used by the Wasmtime runner.

#include <stddef.h>
#include <stdint.h>
#include <string.h>
#include <wasi/wasip1.h>

#include "wasi_test_args.h"

__attribute__((import_module("drv:test"), import_name("log")))
extern void drv_test_log(uint32_t fd, const uint8_t* bytes, uint32_t length);

__attribute__((import_module("drv:test"), import_name("exit")))
extern void drv_test_exit(uint32_t status);

__attribute__((import_module("drv:bluetooth-sapphire/controller@0.1.0"),
               import_name("send")))
extern uint32_t drv_controller_send(uint32_t kind,
                                    const uint8_t* bytes,
                                    uint32_t length);

extern int main(int argc, char** argv);

__attribute__((export_name("drv_test_entry"))) int drv_test_entry(void) {
  static const uint8_t reset_command[] = {0x03, 0x0c, 0x00};
  uint32_t status = drv_controller_send(0, reset_command, sizeof(reset_command));
  if (status != 0) {
    return (int)status;
  }
  return main(drv_gtest_arg_count, drv_gtest_args);
}

__attribute__((export_name("drv_controller_packet"))) int
drv_controller_packet(uint32_t kind, const uint8_t* bytes, uint32_t length) {
  static const uint8_t reset_complete[] = {0x04, 0x0e, 0x04, 0x01,
                                           0x03, 0x0c, 0x00};
  return kind != 0 || length != sizeof(reset_complete) ||
         memcmp(bytes, reset_complete, sizeof(reset_complete)) != 0;
}

__wasi_errno_t __imported_wasi_snapshot_preview1_args_get(uint8_t** argv,
                                                          uint8_t* buffer) {
  for (size_t i = 0; i < drv_gtest_arg_count; ++i) {
    size_t size = strlen(drv_gtest_args[i]) + 1;
    argv[i] = buffer;
    memcpy(buffer, drv_gtest_args[i], size);
    buffer += size;
  }
  return __WASI_ERRNO_SUCCESS;
}

__wasi_errno_t __imported_wasi_snapshot_preview1_args_sizes_get(
    __wasi_size_t* count, __wasi_size_t* size) {
  *count = drv_gtest_arg_count;
  *size = 0;
  for (size_t i = 0; i < drv_gtest_arg_count; ++i) {
    *size += strlen(drv_gtest_args[i]) + 1;
  }
  return __WASI_ERRNO_SUCCESS;
}

__wasi_errno_t __imported_wasi_snapshot_preview1_environ_get(
    uint8_t** environ, uint8_t* buffer) {
  (void)environ;
  (void)buffer;
  return __WASI_ERRNO_SUCCESS;
}

__wasi_errno_t __imported_wasi_snapshot_preview1_environ_sizes_get(
    __wasi_size_t* count, __wasi_size_t* size) {
  *count = 0;
  *size = 0;
  return __WASI_ERRNO_SUCCESS;
}

__wasi_errno_t __imported_wasi_snapshot_preview1_fd_close(__wasi_fd_t fd) {
  (void)fd;
  return __WASI_ERRNO_BADF;
}

__wasi_errno_t __imported_wasi_snapshot_preview1_fd_fdstat_get(
    __wasi_fd_t fd, __wasi_fdstat_t* stat) {
  if (fd > 2) {
    return __WASI_ERRNO_BADF;
  }
  memset(stat, 0, sizeof(*stat));
  stat->fs_filetype = __WASI_FILETYPE_CHARACTER_DEVICE;
  if (fd == 1 || fd == 2) {
    stat->fs_rights_base = __WASI_RIGHTS_FD_WRITE;
  }
  return __WASI_ERRNO_SUCCESS;
}

__wasi_errno_t __imported_wasi_snapshot_preview1_fd_read(
    __wasi_fd_t fd,
    const __wasi_iovec_t* iovecs,
    size_t iovecs_length,
    __wasi_size_t* read) {
  (void)fd;
  (void)iovecs;
  (void)iovecs_length;
  *read = 0;
  return __WASI_ERRNO_BADF;
}

__wasi_errno_t __imported_wasi_snapshot_preview1_fd_seek(
    __wasi_fd_t fd,
    __wasi_filedelta_t offset,
    __wasi_whence_t whence,
    __wasi_filesize_t* position) {
  (void)fd;
  (void)offset;
  (void)whence;
  *position = 0;
  return __WASI_ERRNO_BADF;
}

__wasi_errno_t __imported_wasi_snapshot_preview1_fd_write(
    __wasi_fd_t fd,
    const __wasi_ciovec_t* iovecs,
    size_t iovecs_length,
    __wasi_size_t* written) {
  if (fd != 1 && fd != 2) {
    *written = 0;
    return __WASI_ERRNO_BADF;
  }
  __wasi_size_t total = 0;
  for (size_t i = 0; i < iovecs_length; ++i) {
    drv_test_log(fd, iovecs[i].buf, iovecs[i].buf_len);
    total += iovecs[i].buf_len;
  }
  *written = total;
  return __WASI_ERRNO_SUCCESS;
}

_Noreturn void __imported_wasi_snapshot_preview1_proc_exit(
    __wasi_exitcode_t status) {
  drv_test_exit(status);
  __builtin_trap();
}
