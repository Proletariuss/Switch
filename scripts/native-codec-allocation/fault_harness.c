#include "allocation_shim.h"
#include "switch_evs.h"
#include <inttypes.h>
#include <pthread.h>
#include <stdalign.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

static void json_string(const char *text) {
    putchar('"');
    for (; *text; ++text) {
        unsigned char byte = (unsigned char)*text;
        if (byte == '"' || byte == '\\') printf("\\%c", byte);
        else if (byte < 32) printf("\\u%04x", byte);
        else putchar(byte);
    }
    putchar('"');
}

#ifndef SWITCH_CODEC_CONTROL
static void print_report(const SwitchTestCodecAllocReport *r) {
    printf("{\"token\":%" PRIu64 ",\"attempts\":%" PRIu64
           ",\"allocations\":%" PRIu64 ",\"frees\":%" PRIu64
           ",\"live_blocks\":%" PRIu64 ",\"live_bytes\":%" PRIu64
           ",\"allocated_bytes\":%" PRIu64 ",\"injected_failures\":%" PRIu64
           ",\"system_failures\":%" PRIu64 ",\"tracking_errors\":%" PRIu64 "}",
           r->token, r->attempts, r->allocations, r->frees, r->live_blocks,
           r->live_bytes, r->allocated_bytes, r->injected_failures,
           r->system_failures, r->tracking_errors);
}

static int constructor(int mode, uint64_t fail_at) {
    SwitchTestCodecAllocReport before, report, global;
    switch_test_codec_alloc_global(&before);
    uint64_t token = switch_test_codec_alloc_begin(fail_at);
    if (!token) return 2;
    void *state = switch_evs_create(mode);
    int returned_null = state == NULL;
    if (switch_test_codec_alloc_end(token)) return 2;
    if (state) switch_evs_destroy(state);
    if (switch_test_codec_alloc_report(token, &report)) return 2;
    switch_test_codec_alloc_global(&global);
    int ok = (fail_at ? returned_null && report.injected_failures == 1
                      : !returned_null && report.injected_failures == 0)
        && report.attempts > 0 && report.live_blocks == 0 && report.live_bytes == 0
        && report.allocations == report.frees && report.system_failures == 0
        && report.tracking_errors == 0
        && global.live_blocks == before.live_blocks && global.live_bytes == before.live_bytes
        && global.tracking_errors == before.tracking_errors
        && global.system_failures == before.system_failures;
    int retry_ok = 1;
    if (fail_at) {
        uint64_t retry = switch_test_codec_alloc_begin(0);
        void *next = retry ? switch_evs_create(mode) : NULL;
        retry_ok = next != NULL && switch_test_codec_alloc_end(retry) == 0;
        if (next) switch_evs_destroy(next);
        SwitchTestCodecAllocReport retried;
        retry_ok = retry_ok && switch_test_codec_alloc_report(retry, &retried) == 0
            && retried.live_blocks == 0 && retried.live_bytes == 0
            && retried.allocations == retried.frees && retried.injected_failures == 0
            && retried.system_failures == 0 && retried.tracking_errors == 0;
        switch_test_codec_alloc_global(&global);
        retry_ok = retry_ok && global.live_blocks == before.live_blocks
            && global.live_bytes == before.live_bytes
            && global.tracking_errors == before.tracking_errors
            && global.system_failures == before.system_failures;
    }
    printf("{\"mode\":%d,\"fail_at\":%" PRIu64 ",\"returned_null\":%s,\"retry_ok\":%s,\"report\":",
           mode, fail_at, returned_null ? "true" : "false", retry_ok ? "true" : "false");
    print_report(&report);
    printf(",\"sequence\":[");
    for (uint64_t i = 0; i < report.attempts; ++i) {
        SwitchTestCodecAllocTrace trace;
        if (switch_test_codec_alloc_trace(token, i, &trace)) { ok = 0; break; }
        if (i) putchar(',');
        printf("{\"ordinal\":%" PRIu64 ",\"bytes\":%" PRIu64 ",\"line\":%" PRIu64
               ",\"injected\":%" PRIu64 ",\"file\":", trace.ordinal, trace.bytes, trace.line, trace.injected);
        json_string(trace.file);
        printf(",\"kind\":"); json_string(trace.kind); putchar('}');
    }
    printf("],\"ok\":%s}\n", ok && retry_ok ? "true" : "false");
    return ok && retry_ok ? 0 : 1;
}

#define CHECK(condition) do { if (!(condition)) { \
    fprintf(stderr, "shim selfcheck failed at line %d: %s\n", __LINE__, #condition); return 1; \
} } while (0)

typedef struct { uint64_t token; void *pointer; int status; } ThreadCheck;
static void *thread_check(void *opaque) {
    ThreadCheck *check = opaque;
    check->token = switch_test_codec_alloc_begin(0);
    check->pointer = switch_test_codec_calloc(3, 7, __FILE__, __LINE__);
    check->status = switch_test_codec_alloc_end(check->token);
    return NULL;
}

static int selfcheck(int ledger_limit) {
    if (ledger_limit) {
        for (unsigned i = 0; i < 4096; ++i) {
            uint64_t token = switch_test_codec_alloc_begin(0);
            CHECK(token == i + 1);
            CHECK(switch_test_codec_alloc_end(token) == 0);
        }
        CHECK(switch_test_codec_alloc_begin(0) == 0);
        SwitchTestCodecAllocReport first, last, global;
        CHECK(switch_test_codec_alloc_report(1, &first) == 0);
        CHECK(switch_test_codec_alloc_report(4096, &last) == 0);
        CHECK(first.token == 1 && last.token == 4096);
        switch_test_codec_alloc_global(&global);
        CHECK(global.tracking_errors == 1 && global.live_blocks == 0 && global.live_bytes == 0);
        puts("{\"selfcheck\":\"ledger-limit\",\"ok\":true}");
        return 0;
    }
    uint64_t first = switch_test_codec_alloc_begin(0);
    CHECK(first != 0);
    CHECK(switch_test_codec_alloc_begin(1) == 0);
    CHECK(switch_test_codec_alloc_end(first + 1) != 0);
    unsigned char *pointer = switch_test_codec_calloc(8, sizeof(max_align_t), __FILE__, __LINE__);
    CHECK(pointer && (uintptr_t)pointer % alignof(max_align_t) == 0);
    for (size_t i = 0; i < 8 * sizeof(max_align_t); ++i) CHECK(pointer[i] == 0);
    CHECK(switch_test_codec_alloc_end(first) == 0);
    uint64_t second = switch_test_codec_alloc_begin(1);
    CHECK(second && second != first);
    switch_test_codec_free(pointer, __FILE__, __LINE__);
    CHECK(!switch_test_codec_calloc(1, 5, __FILE__, __LINE__));
    CHECK(switch_test_codec_alloc_end(second) == 0);
    SwitchTestCodecAllocReport a, b, global;
    CHECK(switch_test_codec_alloc_report(first, &a) == 0);
    CHECK(switch_test_codec_alloc_report(second, &b) == 0);
    CHECK(a.tracking_errors == 2 && a.allocations == 1 && a.frees == 1 && !a.live_bytes);
    CHECK(b.injected_failures == 1 && b.allocations == 0 && b.frees == 0 && !b.live_blocks);
    CHECK(switch_test_codec_alloc_report(0, &a) != 0);
    CHECK(switch_test_codec_alloc_report(first, NULL) != 0);
    CHECK(!switch_test_codec_calloc(SIZE_MAX, 2, __FILE__, __LINE__));
    void *once = switch_test_codec_malloc(11, __FILE__, __LINE__);
    CHECK(once != NULL);
    switch_test_codec_free(once, __FILE__, __LINE__);
    switch_test_codec_free(once, __FILE__, __LINE__);
    int foreign = 17;
    switch_test_codec_free(&foreign, __FILE__, __LINE__);
    CHECK(foreign == 17);
    uint64_t main_thread = switch_test_codec_alloc_begin(1);
    CHECK(main_thread != 0);
    ThreadCheck child = {0};
    pthread_t thread;
    CHECK(pthread_create(&thread, NULL, thread_check, &child) == 0);
    CHECK(pthread_join(thread, NULL) == 0);
    CHECK(child.token && child.pointer && child.status == 0);
    CHECK(!switch_test_codec_malloc(9, __FILE__, __LINE__));
    CHECK(switch_test_codec_alloc_end(main_thread) == 0);
    switch_test_codec_free(child.pointer, __FILE__, __LINE__);
    CHECK(switch_test_codec_alloc_report(child.token, &a) == 0);
    CHECK(a.attempts == 1 && a.allocations == 1 && a.frees == 1 && !a.live_bytes);
    CHECK(a.injected_failures == 0 && a.tracking_errors == 0);
    CHECK(switch_test_codec_alloc_report(main_thread, &b) == 0);
    CHECK(b.injected_failures == 1 && !b.allocations && !b.live_blocks);
    switch_test_codec_alloc_global(&global);
    CHECK(global.allocations == global.frees && !global.live_blocks && !global.live_bytes);
    CHECK(global.tracking_errors == 4 && global.system_failures == 1);
    puts("{\"selfcheck\":\"scope-alignment-overflow-foreign-free-tls\",\"ok\":true}");
    return 0;
}
#endif

static int read_pcm(FILE *file, int16_t pcm[320]) {
    unsigned char bytes[640];
    if (fread(bytes, 1, sizeof(bytes), file) != sizeof(bytes)) return 0;
    for (int i = 0; i < 320; ++i) pcm[i] = (int16_t)(bytes[i * 2] | (unsigned)bytes[i * 2 + 1] << 8);
    return 1;
}

static int vectors(int mode, const char *fixtures, const char *prefix) {
    const int frozen_oracle = mode == -1 || mode == 2;
    char path[4096];
    snprintf(path, sizeof(path), "%s/input.pcm", fixtures);
    FILE *input = fopen(path, "rb");
    snprintf(path, sizeof(path), "%s.bits", prefix);
    FILE *encoded_file = fopen(path, "wb");
    snprintf(path, sizeof(path), "%s.pcm", prefix);
    FILE *decoded_file = fopen(path, "wb");
    FILE *reference = NULL, *decoded_reference = NULL;
    if (mode == -1 || mode == 2) {
        snprintf(path, sizeof(path), "%s/%s", fixtures, mode == -1 ? "reference.evs" : "amrwb1265.evs");
        reference = fopen(path, "rb");
        snprintf(path, sizeof(path), "%s/%s", fixtures, mode == -1 ? "decoded.pcm" : "amrwb1265.pcm");
        decoded_reference = fopen(path, "rb");
    }
    if (!input || !encoded_file || !decoded_file || ((mode == -1 || mode == 2) && (!reference || !decoded_reference))) return 2;
    if (reference) {
        unsigned char header[16];
        const unsigned char expected[16] = {'#','!','E','V','S','_','M','C','1','.','0','\n',0,0,0,1};
        if (fread(header, 1, sizeof(header), reference) != sizeof(header)
            || memcmp(header, expected, sizeof(header))) return 2;
    }
#ifndef SWITCH_CODEC_CONTROL
    uint64_t encoder_scope = switch_test_codec_alloc_begin(0);
#endif
    void *encoder = switch_evs_create(mode);
#ifndef SWITCH_CODEC_CONTROL
    if (!encoder_scope || switch_test_codec_alloc_end(encoder_scope)) return 2;
    uint64_t decoder_scope = switch_test_codec_alloc_begin(0);
#endif
    void *decoder = switch_evs_create(mode);
#ifndef SWITCH_CODEC_CONTROL
    if (!decoder_scope || switch_test_codec_alloc_end(decoder_scope)) return 2;
#endif
    if (!encoder || !decoder) return 1;
    int ok = 1;
    for (int frame = 0; frame < 50 && ok; ++frame) {
        int16_t pcm[320], decoded[320], expected[320];
        uint8_t bits[320] = {0}, frozen[320] = {0};
        if (!read_pcm(input, pcm)) { ok = 0; break; }
        int count = switch_evs_encode(encoder, pcm, bits);
        if (count <= 0 || count > 480) { ok = 0; break; }
        size_t bytes = (size_t)(count + 7) / 8;
        unsigned char length[2] = {(unsigned char)count, (unsigned char)(count >> 8)};
        if (fwrite(length, 1, 2, encoded_file) != 2 || fwrite(bits, 1, bytes, encoded_file) != bytes) { ok = 0; break; }
        const uint8_t *source = bits;
        if (reference) {
            int toc = fgetc(reference);
            if (toc != (mode == -1 ? 4 : 0x32) || fread(frozen, 1, bytes, reference) != bytes
                || memcmp(bits, frozen, bytes)) { ok = 0; break; }
            source = frozen; /* Independent decoder oracle, not fresh encoding. */
        }
        if (switch_evs_decode(decoder, source, count, mode == -1 ? 4 : mode, 0, decoded) != 320) { ok = 0; break; }
        if (decoded_reference && (!read_pcm(decoded_reference, expected) || memcmp(decoded, expected, sizeof(expected)))) { ok = 0; break; }
        for (int i = 0; i < 320; ++i) {
            unsigned char pair[2] = {(unsigned char)decoded[i], (unsigned char)((uint16_t)decoded[i] >> 8)};
            if (fwrite(pair, 1, 2, decoded_file) != 2) { ok = 0; break; }
        }
    }
    if (fgetc(input) != EOF || (reference && fgetc(reference) != EOF)
        || (decoded_reference && fgetc(decoded_reference) != EOF)) ok = 0;
    if (ferror(input) || (reference && ferror(reference))
        || (decoded_reference && ferror(decoded_reference))) ok = 0;
    switch_evs_destroy(encoder);
    switch_evs_destroy(decoder);
    if (fclose(input)) ok = 0;
    if (fclose(encoded_file)) ok = 0;
    if (fclose(decoded_file)) ok = 0;
    if (reference && fclose(reference)) ok = 0;
    if (decoded_reference && fclose(decoded_reference)) ok = 0;
#ifndef SWITCH_CODEC_CONTROL
    SwitchTestCodecAllocReport a, b, global;
    if (switch_test_codec_alloc_report(encoder_scope, &a) || switch_test_codec_alloc_report(decoder_scope, &b)) ok = 0;
    switch_test_codec_alloc_global(&global);
    if (a.live_blocks || a.live_bytes || b.live_blocks || b.live_bytes || global.live_blocks
        || global.live_bytes || global.system_failures || global.tracking_errors) ok = 0;
#endif
    printf("{\"vectors\":true,\"mode\":%d,\"frames\":50,\"independent_frozen_oracle\":%s,\"ok\":%s}\n",
           mode, frozen_oracle ? "true" : "false", ok ? "true" : "false");
    return ok ? 0 : 1;
}

int main(int argc, char **argv) {
    setvbuf(stdout, NULL, _IONBF, 0);
#ifndef SWITCH_CODEC_CONTROL
    if (argc == 2 && !strcmp(argv[1], "selfcheck")) return selfcheck(0);
    if (argc == 2 && !strcmp(argv[1], "ledger-limit")) return selfcheck(1);
#endif
    if (argc < 3) return 2;
    char *end = NULL;
    long mode = strtol(argv[2], &end, 10);
    if (!end || end == argv[2] || *end || mode < -1 || mode > 8) return 2;
    if (argc == 5 && !strcmp(argv[1], "vectors")) return vectors((int)mode, argv[3], argv[4]);
#ifndef SWITCH_CODEC_CONTROL
    if (argc == 3 && !strcmp(argv[1], "enumerate")) return constructor((int)mode, 0);
    if (argc == 4 && !strcmp(argv[1], "fail")) {
        uint64_t nth = strtoull(argv[3], &end, 10);
        if (!end || *end || !nth) return 2;
        return constructor((int)mode, nth);
    }
#endif
    return 2;
}
