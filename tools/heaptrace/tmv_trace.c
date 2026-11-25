/* Linux/glibc diagnostic allocator interposer. Wire format: 8 LE u64s. */
#define _GNU_SOURCE
#include <errno.h>
#include <execinfo.h>
#include <fcntl.h>
#include <pthread.h>
#include <stdint.h>
#include <stdatomic.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <unistd.h>
#include <endian.h>
#include <link.h>
#include <sys/syscall.h>
#if !defined(__GLIBC__) || __SIZEOF_POINTER__ != 8
#error "tmvtrace requires 64-bit Linux/glibc"
#endif
extern void *__libc_malloc(size_t);
extern void *__libc_calloc(size_t,size_t);
extern void *__libc_realloc(void *,size_t);
extern void __libc_free(void *);
extern void *__libc_memalign(size_t,size_t);
static pthread_mutex_t lock = PTHREAD_MUTEX_INITIALIZER;
static __thread int busy;
static __thread void *stack_pcs[32];
static __thread int stack_count;
static int stack_depth=16;
static _Atomic int fd = -1;
static pid_t owner;
static uint64_t seq;
static char maps_path[4096];
static char ids_path[4096];
/* Metadata/diagnostics can be interrupted or written only partially. */
static int write_all(int out, const char *buf, size_t left) {
    while(left) {
        ssize_t n=write(out,buf,left);
        if(n<0 && errno==EINTR) continue;
        if(n<=0) return -1;
        buf+=n; left-=(size_t)n;
    }
    return 0;
}
/* Serializing allocator calls as well as records prevents address reuse races. */
static int enter(int capture) {
    if (busy || fd < 0 || getpid() != owner) return 0;
    busy = 1;
    /* Unwind BEFORE acquiring the allocator log lock: unwinding may use loader locks. */
    stack_count = capture && stack_depth ? backtrace(stack_pcs,stack_depth) : 0;
    pthread_mutex_lock(&lock);
    if (fd < 0) { pthread_mutex_unlock(&lock); busy=0; return 0; }
    return 1;
}
static void leave(int active) {
    if (active) { pthread_mutex_unlock(&lock); busy = 0; }
}
static void event(uint64_t op, void *p, uintptr_t old, size_t size, void *pc) {
    uint64_t words[8] = {0x3150414548564d54ULL, op, ++seq, (uint64_t)owner,
        (uintptr_t)p, old, size, (uintptr_t)pc};
    for (int i=0;i<8;i++) words[i]=htole64(words[i]);
    const char *buf=(const char *)words;
    size_t left=sizeof(words);
    while(left) {
        ssize_t n=syscall(SYS_write,fd,buf,left);
        if(n<0 && errno==EINTR) continue;
        if(n<=0) {
            /* No END marker: reader must not call this a complete trace. */
            const char msg[]="tmvtrace: write failed; trace incomplete\n";
            (void)syscall(SYS_write,2,msg,sizeof(msg)-1);
            (void)syscall(SYS_close,fd); fd=-1; break;
        }
        buf+=n; left-=(size_t)n;
    }
}
/* Stack prefix records (op 5): old=index, size=count, pc=return address.
 * The next allocation/reallocation consumes the prefix. Whole groups are serialized. */
static void allocation_event(uint64_t op, void *p, uintptr_t old, size_t size, void *pc) {
    int first=-1;
    for(int i=0;i<stack_count;i++) if(stack_pcs[i]==pc) {first=i;break;}
    if(first>=0) {
        for(int i=first;i<stack_count;i++) event(5,0,(uintptr_t)(i-first),(size_t)(stack_count-first),stack_pcs[i]);
    } else if(stack_depth) {event(5,0,0,1,pc);}
    event(op,p,old,size,pc);
}
#define PC __builtin_extract_return_addr(__builtin_return_address(0))
void *malloc(size_t n) {
    int a=enter(1); void *p=__libc_malloc(n); int e=errno;
    if(a && p) allocation_event(1,p,0,n,PC);
    leave(a); errno=e; return p;
}
void *calloc(size_t n,size_t s) {
    int a=enter(1); void *p=__libc_calloc(n,s); int e=errno;
    if(a && p) allocation_event(1,p,0,n*s,PC);
    leave(a); errno=e; return p;
}
void free(void *p) {
    int e=errno, a=enter(0);
    if(a && p) event(2,p,0,0,PC);
    __libc_free(p); leave(a); errno=e;
}
void *realloc(void *p,size_t n) {
    int a=enter(1); uintptr_t old=(uintptr_t)p;
    void *r=__libc_realloc(p,n); int e=errno;
    /* glibc realloc(p,0) frees p, even when the result is NULL. */
    if(a && (r || (old && !n))) allocation_event(3,r,old,n,PC);
    leave(a); errno=e; return r;
}
void *memalign(size_t alignment,size_t n) {
    int a=enter(1); void *p=__libc_memalign(alignment,n); int e=errno;
    if(a && p) allocation_event(1,p,0,n,PC);
    leave(a); errno=e; return p;
}
void *aligned_alloc(size_t alignment,size_t n) {
    int a=enter(1); void *p=__libc_memalign(alignment,n); int e=errno;
    if(a && p) allocation_event(1,p,0,n,PC);
    leave(a); errno=e; return p;
}
int posix_memalign(void **out,size_t alignment,size_t n) {
    if(alignment<sizeof(void *) || (alignment & (alignment-1))) return EINVAL;
    int saved=errno, a=enter(1); void *p=__libc_memalign(alignment,n);
    if(a && p) allocation_event(1,p,0,n,PC);
    leave(a); errno=saved;
    if(!p) return ENOMEM;
    *out=p; return 0;
}
/* Capture build IDs from loaded ELF notes, not from possibly replaced files. */
static int module_id(struct dl_phdr_info *info, size_t ignored, void *arg) {
    (void)ignored;
    int out=*(int *)arg;
    for (int i=0;i<info->dlpi_phnum;i++) {
        const ElfW(Phdr) *ph=&info->dlpi_phdr[i];
        if(ph->p_type!=PT_NOTE) continue;
        const unsigned char *p=(const unsigned char *)(info->dlpi_addr+ph->p_vaddr);
        size_t left=ph->p_memsz;
        while(left>=sizeof(ElfW(Nhdr))) {
            ElfW(Nhdr) h; memcpy(&h,p,sizeof(h));
            size_t ns=((size_t)h.n_namesz+3)&~(size_t)3;
            size_t ds=((size_t)h.n_descsz+3)&~(size_t)3;
            if(ns>left-sizeof(h) || ds>left-sizeof(h)-ns) break;
            if(h.n_type==NT_GNU_BUILD_ID && h.n_namesz==4 && !memcmp(p+sizeof(h),"GNU",4) && h.n_descsz<=64) {
                char path[4096], line[4300], hex[129];
                const char *name=info->dlpi_name;
                if(!name || !*name) {
                    ssize_t n=readlink("/proc/self/exe",path,sizeof(path)-1);
                    if(n<0) break;
                    path[n]=0; name=path;
                }
                const unsigned char *id=p+sizeof(h)+ns;
                for(unsigned j=0;j<h.n_descsz;j++) snprintf(hex+j*2,3,"%02x",id[j]);
                hex[h.n_descsz*2]=0;
                int n=snprintf(line,sizeof(line),"%s %s\n",hex,name);
                if(n>0 && n<(int)sizeof(line) && write_all(out,line,(size_t)n)<0) return 1;
                break;
            }
            size_t step=sizeof(h)+ns+ds; p+=step; left-=step;
        }
    }
    return 0;
}
static void save_ids(void) {
    int out=open(ids_path,O_WRONLY|O_CREAT|O_TRUNC|O_CLOEXEC|O_NOFOLLOW,0600);
    if(out>=0) { dl_iterate_phdr(module_id,&out); close(out); }
}
static void save_maps(void) {
    int in=open("/proc/self/maps",O_RDONLY|O_CLOEXEC);
    int out=open(maps_path,O_WRONLY|O_CREAT|O_TRUNC|O_CLOEXEC|O_NOFOLLOW,0600);
    if(in>=0 && out>=0) {
        char buf[4096]; ssize_t n;
        while((n=read(in,buf,sizeof(buf)))>0) {
            ssize_t done=0;
            while(done<n) { ssize_t w=write(out,buf+done,(size_t)(n-done)); if(w<=0) break; done+=w; }
        }
    }
    if(in>=0) close(in);
    if(out>=0) close(out);
    save_ids();
}
__attribute__((constructor)) static void start(void) {
    busy=1;
    const char *depth=getenv("TMV_STACK_DEPTH");
    if(depth) { char *end; long n=strtol(depth,&end,10); if(*depth && !*end && n>=0 && n<=32) stack_depth=(int)n; }
    /* Prime glibc/libgcc unwinder while recursive allocations are excluded. */
    if(stack_depth) (void)backtrace(stack_pcs,32);
    const char *prefix=getenv("TMV_TRACE");
    if(prefix && *prefix) {
        char path[4096]; owner=getpid();
        int len=snprintf(path,sizeof(path),"%s.%d.bin",prefix,owner);
        int ml=snprintf(maps_path,sizeof(maps_path),"%s.%d.maps",prefix,owner);
        int il=snprintf(ids_path,sizeof(ids_path),"%s.%d.ids",prefix,owner);
        if(il>0 && il<(int)sizeof(ids_path) && len>0 && len<(int)sizeof(path) && ml>0 && ml<(int)sizeof(maps_path))
            fd=open(path,O_WRONLY|O_CREAT|O_EXCL|O_CLOEXEC,0600);
        if(fd>=0) { event(0,0,0,0,0); save_maps(); }
        /* A diagnostic cannot recover if stderr itself is unavailable. */
        else { const char msg[]="tmvtrace: cannot create trace file\n"; (void)write_all(2,msg,sizeof(msg)-1); }
    }
    busy=0;
}
__attribute__((destructor)) static void stop(void) {
    if(getpid()!=owner || fd<0) return;
    busy=1; save_maps(); busy=0;
    int a=enter(0);
    if(a) { event(4,0,0,0,0); if(fd>=0) (void)syscall(SYS_close,fd); fd=-1; }
    leave(a);
}
