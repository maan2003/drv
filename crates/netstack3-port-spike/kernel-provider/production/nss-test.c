// SPDX-License-Identifier: GPL-2.0-only
#define _GNU_SOURCE
#include <netdb.h>
#include <arpa/inet.h>
#include <dlfcn.h>
#include <errno.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <pthread.h>
#include <sys/wait.h>
#include <unistd.h>
static void check(int ok, const char *what) {
    if (!ok) { fprintf(stderr, "FAIL NSS %s\n", what); exit(1); }
}
static void lookup(void) {
    struct addrinfo hints = { .ai_family = AF_UNSPEC, .ai_socktype = SOCK_STREAM }, *result;
    int rc = getaddrinfo("example.test", "8080", &hints, &result);
    if (rc) fprintf(stderr, "getaddrinfo: %s\n", gai_strerror(rc));
    check(rc == 0, "getaddrinfo through loaded Rust NSS");
    int v4 = 0, v6 = 0;
    for (struct addrinfo *a = result; a; a = a->ai_next) {
        if (a->ai_family == AF_INET) v4++;
        if (a->ai_family == AF_INET6) v6++;
    }
    freeaddrinfo(result);
    check(v4 && v6, "A and AAAA results");
}
static void *thread(void *unused) { (void)unused; lookup(); return NULL; }
int main(int argc, char **argv) {
    if (argc == 2 && !strcmp(argv[1], "absent")) {
        struct addrinfo hints = { .ai_family = AF_INET, .ai_socktype = SOCK_STREAM }, *result;
        check(getaddrinfo("example.test", NULL, &hints, &result) != 0, "offline lookup must fail closed");
        puts("PASS_NSS_OFFLINE_NO_FALLBACK");
        return 0;
    }
    void *module = dlopen("libnss_drv.so.2", RTLD_NOW | RTLD_LOCAL);
    check(module != NULL, "dlopen");
    typedef int (*lookup_fn)(const char *, int, struct hostent *, char *, size_t, int *, int *);
    lookup_fn fn = (lookup_fn)dlsym(module, "_nss_drv_gethostbyname2_r");
    check(fn != NULL, "NSS symbol");
    for (int family = 0; family < 2; family++) {
        struct hostent host;
        char buffer[2048]; int error = 0, herror = 0;
        memset(buffer, 0xa5, sizeof(buffer));
        int rc = fn("example.test", family ? AF_INET6 : AF_INET, &host, buffer + 1, 1, &error, &herror);
        if (rc != -2) fprintf(stderr, "NSS rc=%d errno=%d herrno=%d family=%d\n", rc, error, herror, family);
        check(rc == -2 && error == ERANGE && herror == TRY_AGAIN, "short buffer retry");
        rc = fn("example.test", family ? AF_INET6 : AF_INET, &host, buffer + 1, sizeof(buffer) - 2, &error, &herror);
        check(rc == 1 && error == 0 && herror == 0, "misaligned caller buffer");
        check(host.h_aliases[0] == NULL && host.h_addr_list[0] != NULL && host.h_addr_list[1] == NULL, "full pointer terminators");
        check(!strcmp(host.h_name, "example.test"), "owned name");
        check((unsigned char)buffer[0] == 0xa5 && (unsigned char)buffer[sizeof(buffer) - 1] == 0xa5, "buffer canaries");
    }
    lookup();
    pthread_t threads[4];
    for (int i = 0; i < 4; i++) check(!pthread_create(&threads[i], NULL, thread, NULL), "thread create");
    for (int i = 0; i < 4; i++) check(!pthread_join(threads[i], NULL), "thread join");
    pid_t pid = fork(); check(pid >= 0, "fork");
    if (!pid) { lookup(); _exit(0); }
    int status; check(waitpid(pid, &status, 0) == pid && WIFEXITED(status) && WEXITSTATUS(status) == 0, "lookup after fork");
    struct addrinfo hints = { .ai_family = AF_INET, .ai_socktype = SOCK_STREAM }, *result;
    check(getaddrinfo("missing.test", NULL, &hints, &result) == EAI_NONAME, "NXDOMAIN mapping");
    check(getaddrinfo("tcp.example.test", NULL, &hints, &result) == 0, "truncated UDP falls back to TCP");
    freeaddrinfo(result);
    dlclose(module);
    puts("PASS_NSS_RUST_GLIBC_DNS_ABI_THREADS_FORK_TCP");
    return 0;
}
