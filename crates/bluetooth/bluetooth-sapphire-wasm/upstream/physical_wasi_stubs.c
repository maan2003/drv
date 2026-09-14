// Reuse the bounded WASI logging/clock lowering without linking GoogleTest.
#define main drv_unused_test_main
#include "wasi_test_stubs.c"

int drv_unused_test_main(int argc, char** argv) {
  (void)argc;
  (void)argv;
  return 1;
}
