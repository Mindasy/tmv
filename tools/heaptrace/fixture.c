#define _GNU_SOURCE
#include <assert.h>
#include <errno.h>
#include <pthread.h>
#include <stdint.h>
#include <stdlib.h>
#include <unistd.h>
static void *retained;
__attribute__((noinline)) void retain_large(void) { retained=malloc(1024*1024); assert(retained); }
__attribute__((noinline)) void transient(void) { void *p=malloc(123); assert(p); free(p); }
static void *worker(void *unused) {
    (void)unused;
    for(int i=0;i<1000;i++) {
        void *p=calloc(2,16); assert(p);
        p=realloc(p,128); assert(p); free(p);
    }
    return 0;
}
int main(void) {
    retain_large(); transient();
    pthread_t a,b;
    assert(!pthread_create(&a,0,worker,0)); assert(!pthread_create(&b,0,worker,0));
    pthread_join(a,0); pthread_join(b,0);
    void *p=malloc(17); assert(p);
    errno=E2BIG; free(p); assert(errno==E2BIG);
    assert(posix_memalign(&p,3,16)==EINVAL);
    assert(posix_memalign(&p,64,128)==0); free(p);
    p=malloc(16); assert(p);
    volatile size_t impossible=SIZE_MAX;
    void *q=realloc(p,impossible); assert(!q); free(p);
    p=malloc(16); assert(p); q=realloc(p,0); assert(!q);
    return retained==0;
}
