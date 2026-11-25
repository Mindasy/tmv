#define _POSIX_C_SOURCE 200809L
#include <assert.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <unistd.h>
#define MIB (1024UL * 1024UL)
__attribute__((noinline)) void *cache_create(void) {
    void *p=malloc(32*MIB); assert(p); memset(p,0x11,32*MIB); return p;
}
__attribute__((noinline)) void *sessions_create(void) {
    void *p=malloc(8*MIB); assert(p); memset(p,0x22,8*MIB); return p;
}
__attribute__((noinline)) void *sessions_grow(void *p) {
    p=realloc(p,24*MIB); assert(p); memset(p,0x33,24*MIB); return p;
}
__attribute__((noinline)) void temporary_work(void) {
    void *p=malloc(64*MIB); assert(p); memset(p,0x44,64*MIB); free(p);
}
__attribute__((noinline)) void *cache_initialize(void) { return cache_create(); }
__attribute__((noinline)) void *sessions_initialize(void) { return sessions_create(); }
int main(void) {
    setvbuf(stdout,NULL,_IONBF,0);
    void *cache=cache_initialize(), *sessions=sessions_initialize();
    temporary_work();
    puts("PHASE1 cache=32MiB sessions=8MiB temporary=freed"); sleep(6);
    sessions=sessions_grow(sessions);
    puts("PHASE2 cache=32MiB sessions=24MiB"); sleep(6);
    free(cache);
    puts("PHASE3 cache=freed sessions=24MiB"); sleep(6);
    free(sessions);
    puts("DONE all business allocations freed");
    return 0;
}
