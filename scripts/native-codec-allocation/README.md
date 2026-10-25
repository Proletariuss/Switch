# Isolated native allocation-failure evidence

These files are test infrastructure. `scripts/build-evs.py` does not include
them. Only this separate builder force-includes `force_include.h` in the pinned
native translation units. libc, Rust and the rest of the process retain their
ordinary allocators. There is no environment-controlled injection setting.

The parent owner starts build and execution stages sequentially. The default
output is `Switch_MAC/build/native-codec-allocation/after`, with its own
`evs/libswitch_evs.a`; this never writes `Switch_MAC/build/evs/libswitch_evs.a`.
The Rust `native-codec-fault-tests` feature must select only that debug archive;
release builds with the feature are rejected by the crate build script.

## C ABI and ownership

`allocation_shim.h` is the ABI source. `begin(N)` must execute on the actual
constructor thread; `N=0` records without failing, and `N>0` returns NULL at
exactly the Nth wrapped malloc/calloc attempt in that scope. A nonzero token
identifies the scope. Nested scopes or a full 4,096-token ledger return zero.
`end(token)`, `report(token,out)` and `trace(token,index,out)` return zero on
success. Reports are ten consecutive uint64 fields, matching Rust `repr(C)`.

`end` disarms TLS, but never resets ownership counters. An aligned allocation
header retains its original token. A test-only mutex protects an intrusive live
registry: free first finds the exact live pointer, then updates its original
token even on another thread or after that thread exits. Foreign/double-free
pointers are not dereferenced or forwarded to libc; they increment the explicit
global tracking error, which clean tests must reject. Payload alignment is at
least max_align_t; calloc payloads are zeroed and multiplication/prefix-size
overflow returns NULL before any real large allocation.

Counts describe actual wrapped native allocation blocks and requested payload
bytes, excluding allocator rounding, shim headers and unrelated libc/Rust heap
use. Every header shares the one underlying real allocation and is freed with
it. Reports remain available after native worker join. Always check global
system/tracking errors as well as per-token counters: an invalid free after TLS
end cannot reliably be attributed to an already-freed token. There are no
allocating bookkeeping nodes. The trace capacity is 256 attempts per token;
exceeding it is an explicit test failure, never a truncated passing denominator.

## Stages

From the `src` directory:

```
python3 -B scripts/test-native-codec-allocation.py build
python3 -B scripts/test-native-codec-allocation.py enumerate --sandbox-profile PROFILE
python3 -B scripts/test-native-codec-allocation.py faults --sandbox-profile PROFILE
python3 -B scripts/test-native-codec-allocation.py vectors --sandbox-profile PROFILE --control-archive FROZEN_PRODUCTION_ARCHIVE
```

`all` performs build, enumeration, injector selfchecks, every enumerated fault
and vectors. Build commands stay outside sandbox-exec because xcrun needs its
normal compiler services; only native harness/control children use the supplied
profile. The root owner verifies the no-device/no-network canaries first. Every
child has core dumps disabled and a bounded timeout. No real OOM is induced.

Each enumeration runs EVS and WB0–8, saves source-site/size/ordinal traces and
freezes `allocation-plan.json` before attempting every actual point. Each fault
must return NULL, retain no blocks/bytes, report one injection, match the exact
enumerated prefix, and successfully construct/destroy the next codec with
injection off. Each case is a separate process so signals are observable and
injector selfchecks cannot contaminate clean counters. Selfchecks deliberately
exercise rejected scopes, overflow and invalid frees; those expected error
counts are confined to their own subprocess.

The 50-frame vector stage compares actual encoded bits and decoded PCM for all
ten profiles against a separately linked frozen production archive. EVS and
WB2 additionally compare encoder bytes and decode the checked-in independent
reference bitstreams against their frozen PCM. It uses separate encoder and
decoder states, checks framing/EOF and stores output SHA evidence. The test
archive is forbidden as its own control.

For a separately preserved, unmodified source snapshot, use a separate output
directory and `before-proof --expect-unsafe --fail-at N`. Enumeration must first
succeed; one selected failure then must produce the explicitly expected signal
(default SIGSEGV). The result is labelled `expected_unsafe_observed`, never a
passing fault test. A different signal, timeout or safe return does not satisfy
that proof. Baseline sources and archives are read-only inputs.

This proves explicit native allocation failure behavior, not arbitrary runtime
hang recovery, OS process kills, Rust global allocator aborts, thread-spawn
failure, operator acceptance or physical audio quality. Overall P5 remains in
progress and P6 final acceptance is incomplete; 294/294 interface ownership is
a separate measurement.
