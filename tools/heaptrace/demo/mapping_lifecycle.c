// Linux: gcc -O0 -g -Wall -Wextra -Werror -Wl,--build-id mapping_lifecycle.c -o mapping_lifecycle
#define _GNU_SOURCE
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/mman.h>
#include <unistd.h>

enum { MiB = 1024 * 1024 };

static void hold(const char *phase) {
    puts(phase);
    fflush(stdout);
    sleep(4);
}

int main(void) {
    unsigned char *heap = malloc(16 * MiB);
    if (!heap) { perror("malloc"); return 1; }
    memset(heap, 1, 16 * MiB);

    // Guard pages isolate the measured VMA from adjacent anonymous mappings.
    long page = sysconf(_SC_PAGESIZE);
    if (page <= 0) return 1;
    size_t span = 32 * MiB + 2 * (size_t)page;
    unsigned char *base = mmap(NULL, span, PROT_NONE, MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
    if (base == MAP_FAILED) { perror("mmap"); return 1; }
    unsigned char *mapping = base + page;
    if (mprotect(mapping, 32 * MiB, PROT_READ | PROT_WRITE)) { perror("mprotect"); return 1; }
    printf("heap=%p mapping=%p\n", (void *)heap, (void *)mapping);
    hold("reserved");

    memset(mapping, 2, 32 * MiB);
    hold("touched");

    if (madvise(mapping, 24 * MiB, MADV_DONTNEED)) { perror("madvise"); return 1; }
    hold("discarded first 24 MiB");

    free(heap);
    hold("heap freed");

    if (munmap(base, span)) { perror("munmap"); return 1; }
    hold("unmapped");
    return 0;
}
