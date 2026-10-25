#include "allocation_shim.h"
#include <errno.h>
#include <pthread.h>
#include <stdlib.h>
#include <string.h>

#define MAX_SCOPES 4096u
#define MAX_TRACE 256u

typedef struct {
    SwitchTestCodecAllocReport report;
    SwitchTestCodecAllocTrace trace[MAX_TRACE];
} Scope;

typedef union Block Block;
union Block {
    max_align_t alignment;
    struct {
        Block *previous;
        Block *next;
        uint64_t token;
        size_t size;
    } data;
};
_Static_assert(_Alignof(Block) >= _Alignof(max_align_t), "tracked allocation alignment");
_Static_assert(sizeof(Block) % _Alignof(max_align_t) == 0, "aligned payload offset");
_Static_assert(sizeof(SwitchTestCodecAllocReport) == 80, "Rust/C report ABI");

static pthread_mutex_t lock = PTHREAD_MUTEX_INITIALIZER;
static Scope scopes[MAX_SCOPES];
static SwitchTestCodecAllocReport totals;
static uint64_t next_token;
static Block *live;
static _Thread_local uint64_t current_token;
static _Thread_local uint64_t current_fail_at;

static Scope *scope_for(uint64_t token) {
    return token && token <= next_token ? &scopes[token - 1] : NULL;
}

/* All registry/statistics access holds lock. Metadata itself never allocates. */
static void tracking_error(void) {
    totals.tracking_errors++;
    Scope *scope = scope_for(current_token);
    if (scope) scope->report.tracking_errors++;
}

uint64_t switch_test_codec_alloc_begin(uint64_t fail_at) {
    pthread_mutex_lock(&lock);
    if (current_token || next_token == MAX_SCOPES) {
        tracking_error();
        pthread_mutex_unlock(&lock);
        return 0;
    }
    current_token = ++next_token;
    current_fail_at = fail_at;
    scopes[current_token - 1].report.token = current_token;
    pthread_mutex_unlock(&lock);
    return current_token;
}

int switch_test_codec_alloc_end(uint64_t token) {
    pthread_mutex_lock(&lock);
    if (!token || token != current_token) {
        tracking_error();
        pthread_mutex_unlock(&lock);
        return -1;
    }
    current_token = 0;
    current_fail_at = 0;
    pthread_mutex_unlock(&lock);
    return 0;
}

int switch_test_codec_alloc_report(uint64_t token, SwitchTestCodecAllocReport *out) {
    pthread_mutex_lock(&lock);
    Scope *scope = scope_for(token);
    if (!scope || !out) {
        pthread_mutex_unlock(&lock);
        return -1;
    }
    *out = scope->report;
    pthread_mutex_unlock(&lock);
    return 0;
}

int switch_test_codec_alloc_trace(uint64_t token, uint64_t index, SwitchTestCodecAllocTrace *out) {
    pthread_mutex_lock(&lock);
    Scope *scope = scope_for(token);
    if (!scope || !out || index >= scope->report.attempts || index >= MAX_TRACE) {
        pthread_mutex_unlock(&lock);
        return -1;
    }
    *out = scope->trace[index];
    pthread_mutex_unlock(&lock);
    return 0;
}

void switch_test_codec_alloc_global(SwitchTestCodecAllocReport *out) {
    if (!out) return;
    pthread_mutex_lock(&lock);
    *out = totals;
    pthread_mutex_unlock(&lock);
}

static void *allocate(size_t bytes, int clear, int overflow, const char *file, uint64_t line) {
    pthread_mutex_lock(&lock);
    Scope *scope = scope_for(current_token);
    totals.attempts++;
    int inject = 0;
    if (scope) {
        uint64_t ordinal = ++scope->report.attempts;
        inject = current_fail_at && ordinal == current_fail_at;
        if (ordinal <= MAX_TRACE) {
            scope->trace[ordinal - 1] = (SwitchTestCodecAllocTrace){
                ordinal, bytes, line, (uint64_t)inject, file, clear ? "calloc" : "malloc"
            };
        } else {
            tracking_error();
        }
    }
    if (inject) {
        totals.injected_failures++;
        scope->report.injected_failures++;
        pthread_mutex_unlock(&lock);
        errno = ENOMEM;
        return NULL;
    }
    if (overflow || bytes > SIZE_MAX - sizeof(Block)) {
        totals.system_failures++;
        if (scope) scope->report.system_failures++;
        pthread_mutex_unlock(&lock);
        errno = ENOMEM;
        return NULL;
    }
    /* The real allocation stays inside the registry lock, so reports cannot
     * observe a half-registered block or a free racing its ownership record. */
    Block *block = clear ? calloc(1, sizeof(Block) + bytes) : malloc(sizeof(Block) + bytes);
    if (!block) {
        totals.system_failures++;
        if (scope) scope->report.system_failures++;
        pthread_mutex_unlock(&lock);
        return NULL;
    }
    block->data.token = current_token;
    block->data.size = bytes;
    /* previous points toward the list head; next toward its tail. */
    block->data.previous = NULL;
    block->data.next = live;
    if (live) live->data.previous = block;
    live = block;
    totals.allocations++;
    totals.live_blocks++;
    totals.live_bytes += bytes;
    totals.allocated_bytes += bytes;
    if (scope) {
        scope->report.allocations++;
        scope->report.live_blocks++;
        scope->report.live_bytes += bytes;
        scope->report.allocated_bytes += bytes;
    }
    pthread_mutex_unlock(&lock);
    return block + 1;
}

void *switch_test_codec_calloc(size_t count, size_t size, const char *file, uint64_t line) {
    int overflow = size && count > SIZE_MAX / size;
    return allocate(overflow ? SIZE_MAX : count * size, 1, overflow, file, line);
}

void *switch_test_codec_malloc(size_t size, const char *file, uint64_t line) {
    return allocate(size, 0, 0, file, line);
}

void switch_test_codec_free(void *pointer, const char *file, uint64_t line) {
    (void)file;
    (void)line;
    if (!pointer) return;
    pthread_mutex_lock(&lock);
    Block *block = live;
    while (block && (void *)(block + 1) != pointer) block = block->data.next;
    if (!block) {
        /* Foreign/double-free pointers are never dereferenced or forwarded to
         * libc. The explicit tracking error fails the fault harness instead. */
        tracking_error();
        pthread_mutex_unlock(&lock);
        return;
    }
    if (block->data.previous) block->data.previous->data.next = block->data.next;
    else live = block->data.next;
    if (block->data.next) block->data.next->data.previous = block->data.previous;
    Scope *scope = scope_for(block->data.token);
    totals.frees++;
    totals.live_blocks--;
    totals.live_bytes -= block->data.size;
    if (scope) {
        scope->report.frees++;
        scope->report.live_blocks--;
        scope->report.live_bytes -= block->data.size;
    }
    free(block);
    pthread_mutex_unlock(&lock);
}
