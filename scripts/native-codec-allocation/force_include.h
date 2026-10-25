#ifndef SWITCH_TEST_CODEC_FORCE_INCLUDE_H
#define SWITCH_TEST_CODEC_FORCE_INCLUDE_H
/* Import the real libc declarations before defining translation-unit-local
 * macros. libc, Rust and the test runner keep their ordinary allocators. */
#include <stdlib.h>
#include "allocation_shim.h"
#define calloc(count, size) switch_test_codec_calloc((count), (size), __FILE__, __LINE__)
#define malloc(size) switch_test_codec_malloc((size), __FILE__, __LINE__)
#define free(pointer) switch_test_codec_free((pointer), __FILE__, __LINE__)
#endif
