# P2A forwarding: retire the unreferenced Go runtime

## Decision

The Go `backend/internal/smsforward` runtime had no imports from non-test Go
production files. Rust already owns forwarding configuration, SWF1 queue access,
provider delivery, retry/unknown-result policy, Telegram chunk checkpoints,
and the authenticated daemon service. This change removes the unused second Go
executor; it does not migrate a live endpoint and does not increase the P2A
backend migration percentage.

The Go-only forwarding test suite was an executable compatibility oracle built
around the retired implementation. Its durable expectations are retained in
Rust tests. The Go-produced `go-state.json` and `go-state.swf1.hex` fixtures stay
as frozen historical input, and `proletarius-forwarding` verifies decryption,
exact plaintext preservation, re-encryption, and old queue projections. A tiny
Go test peer remains only to verify both directions of the shared `.lock` /
`flock` contract; it contains no queue, provider, HTTP, or retry implementation.

## Compatibility regression added

The legacy slow-delivery tests required provider I/O not to hold the queue
mutex, unrelated enqueue/status work to stay responsive, in-flight cancellation
to be rejected, and concurrent flushes not to submit the same task twice. Rust
already released the queue lock around HTTP I/O, but had no end-to-end test for
the entire concurrency contract. The new
`worker::tests::slow_provider_io_keeps_queue_responsive_and_serializes_duplicate_workers`
uses a gated loopback provider and a small frozen expectation fixture. It
asserts admission/status progress while the response is blocked, the durable
in-flight guard, one delivery, and no pending duplicate after the serialized
second flush.

## Removed and retained boundaries

- Removed the unreferenced Go forwarding controller, queue, providers, config
  parsing, proxy handling, state encryption, diagnostics, and their duplicate
  Go behavior suite.
- Retained the Go-produced SWF1 golden fixture and Rust checks that consume it.
- Retained `backend/internal/forwardinglockcompat`, containing only Go tests for
  lock acquisition/release against a Rust holder in each direction.
- Updated the functional and compatibility scripts to run the Rust owner and
  the minimal lock peer. No Telegram/Bark endpoint, real SMS, or device is
  accessed.

## Validation and boundary

The old Go suite passed before deletion with race detection. The new Rust
concurrency test and all `proletarius-forwarding` tests are the replacement
behavior gates. The cross-process lock script remains a separate Go↔Rust
interop gate. Provider behavior still uses local synthetic endpoints; this
retirement does not claim real Telegram/Bark delivery validation. The P2A gates
used a shared Switch Cargo registry and Go module cache while keeping build
artifacts inside this worktree:

```sh
SWITCH_FORWARDING_CARGO_HOME=/Users/proletarius/Desktop/Switch/build/cargo-home \
SWITCH_FORWARDING_GOMODCACHE=/Users/proletarius/Desktop/Switch/build/go-mod \
  sh scripts/test-forwarding-lock-interop.sh
SWITCH_FORWARDING_CARGO_HOME=/Users/proletarius/Desktop/Switch/build/cargo-home \
  sh scripts/test-forwarding-state-compat.sh
```

The lock test binaries are built before the interop processes start; marker
polling is configurable from 1 to 12 seconds with
`SWITCH_FORWARDING_LOCK_WAIT_SECONDS` (default 10 seconds).

The full functional gate also passed from this managed worktree. Its short
temporary directory was exposed through a symlink but resolved to this
worktree's `build/t`, keeping artifacts isolated while respecting macOS Unix
socket path limits. The integration runner now accepts a source checkout at
`Switch/worktrees/<name>/src` and keeps its build, runtime, and reports beside
that worktree. The shared Go module cache is selected with
`SWITCH_FORWARDING_GOMODCACHE`.
