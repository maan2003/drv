#ifndef ATH11K_ORACLE_LINUX_SLAB_H
#define ATH11K_ORACLE_LINUX_SLAB_H
#include <stdlib.h>
#define GFP_KERNEL 0
static inline void *kzalloc(size_t n, int flags) { (void)flags; return calloc(1, n); }
static inline void kfree(void *p) { free(p); }
#endif
