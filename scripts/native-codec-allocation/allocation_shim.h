#ifndef SWITCH_TEST_CODEC_ALLOCATION_SHIM_H
#define SWITCH_TEST_CODEC_ALLOCATION_SHIM_H
#include <stddef.h>
#include <stdint.h>

/* Test-only ABI. Never compile or link this shim into a production archive. */
typedef struct {
    uint64_t token;
    uint64_t attempts;
    uint64_t allocations;
    uint64_t frees;
    uint64_t live_blocks;
    uint64_t live_bytes;
    uint64_t allocated_bytes;
    uint64_t injected_failures;
    uint64_t system_failures;
    uint64_t tracking_errors;
} SwitchTestCodecAllocReport;

typedef struct {
    uint64_t ordinal;
    uint64_t bytes;
    uint64_t line;
    uint64_t injected;
    const char *file;
    const char *kind;
} SwitchTestCodecAllocTrace;

/* begin must run on the thread which calls the native constructor. fail_at=0
 * records without injecting. Nested scopes are rejected; token 0 is failure.
 * end/report/trace return 0 on success. end only disarms TLS: the token's
 * allocation ownership survives until every corresponding free completes. */
uint64_t switch_test_codec_alloc_begin(uint64_t fail_at);
int switch_test_codec_alloc_end(uint64_t token);
int switch_test_codec_alloc_report(uint64_t token, SwitchTestCodecAllocReport *out);
int switch_test_codec_alloc_trace(uint64_t token, uint64_t index, SwitchTestCodecAllocTrace *out);
void switch_test_codec_alloc_global(SwitchTestCodecAllocReport *out);

void *switch_test_codec_calloc(size_t count, size_t size, const char *file, uint64_t line);
void *switch_test_codec_malloc(size_t size, const char *file, uint64_t line);
void switch_test_codec_free(void *pointer, const char *file, uint64_t line);
#endif
