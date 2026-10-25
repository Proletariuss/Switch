# Forwarding state envelope compatibility — P2A preparation

This crate implements the SWF1 envelope boundary and an in-memory Go-compatible
queue admission mutation. It is not connected to `switchd-rs`, a production
queue file, or a provider. Successful decryption or queue insertion is not
permission to submit or replay a message.

## Wire format and safety boundary

`SWF1 || nonce[12] || AES-256-GCM(JSON bytes, AAD="SWF1") || tag[16]`.
The key is exactly 32 bytes. The public encoder gets a fresh random nonce from
the operating system; callers cannot supply/reuse a nonce. A failed entropy
request returns an error. Unsupported, truncated, oversized, tampered, wrong-key
or invalid-JSON input never yields an empty queue.

`StateKey` and the owned plaintext `JsonDocument` buffer use `Zeroizing` and
redacted `Debug`; neither implements `Clone` or `Serialize`. Existing locked
AES/AES-GCM dependencies have their zeroize features enabled. Callers must still
protect their original key buffers and any copies they make of plaintext.
This is memory-lifetime hygiene, not a guarantee against arbitrary memory dumps.

Limits are checked before ciphertext allocation. Default and hard maximum are
64 MiB of plaintext; a caller can require a smaller limit. This is a new explicit
resource policy, **not** an old Go wire-format limit. Queues above it must remain
with the old owner until an explicit safe migration policy is agreed; never
silently reset or truncate them. JSON is syntax-checked without a full Value
tree, with an explicit maximum of 128 nested containers (including the root);
larger nesting is also a resource-policy rejection, not a reset. Root objects
only are accepted; authenticated `null`/arrays/scalars are
rejected rather than normalized to empty queues.

All original JSON bytes remain intact, including order, whitespace, unknown or
duplicate fields, HTML escapes, Unicode, numeric spelling, zero/offset/fractional
timestamps, frozen Telegram chunks, digests and result-unknown markers. A
read-only `JsonDocument::inspect_go_queue()` projection now parses the known
Go fields needed to review queue counts, per-channel completion/unknown state,
destination binding presence, and Telegram checkpoint shape. It avoids copying
SMS bodies, chunk strings, error codes or destination hashes into its summary;
it verifies frozen message/chunk digests using Go-compatible JSON encoding.
Schema
inspection never serializes or mutates the original document, so unknown and
duplicate fields remain byte-preserved. Duplicate known fields or invalid
typed values fail the projection without changing the still-available source
document.

`DestinationBinding::telegram()` now calculates the existing Go destination
HMAC (`HMAC-SHA256` over the NUL-delimited, trimmed bot token and chat ID) using
the SWF1 state key. Rust and Go assert the same fixed synthetic vector, and
Rust's `Debug` output redacts the binding. This is a compatibility primitive
only: the crate still does not bind or mutate production queue deliveries.

`DestinationBinding::bark()` now applies Go-compatible trimming and HTTP(S)
URL string normalization before hashing the custom server and trimmed device
key. Cross-language fixtures compare 22 accepted/rejected URL forms against
the Go `net/url` and production target-identity path, including case, ports,
userinfo, IPv6/zone IDs, percent encoding, Unicode paths/hosts, and malformed
inputs. This closes the queue-binding primitive; it does not migrate Bark
configuration validation or HTTP delivery.

The `JsonDocument::enqueue` API implements the Go queue's incoming/outgoing
filter, message-key hash, seen deduplication, 10,000-ID pruning and initial
pending record construction. It records disabled destinations as
not_requested and does not contact providers. Existing root fields and
queue-map values are spliced from their original raw JSON, preserving unknown
fields, numeric spellings, whitespace inside values and frozen delivery
evidence. Duplicate keys in the root or mutated maps fail closed. SMS inputs,
serialized pending records and the updated plaintext buffer use zeroizing
storage. `QueueStore` adds an encrypted file boundary for synthetic and future
Rust callers: it takes a nonblocking exclusive advisory lock for the store
lifetime, requires a non-group/world-writable parent, rejects symlinked or
broadly accessible queue/lock files, writes a fresh SWF1 envelope to a
same-directory 0600 temporary file, syncs it, atomically
renames, and syncs the parent directory. It loads and validates existing state
before accepting work, persists before acknowledging a new seen ID, and
requires a reopen after a rename succeeds but directory durability is
uncertain. Tests cover restart recovery, duplicate non-rewrite, lock exclusion,
wrong keys, file and parent permissions, and final-component symlinks. The lock coordinates
only callers using this Rust store; the legacy Go writer does not use this
lock. No production caller is connected yet, and Go/Rust must never write the
same live queue concurrently during a future cutover.

## Reproducible verification

From `src`, run `sh scripts/test-forwarding-state-compat.sh`. It uses pinned,
locally installed toolchains, `cargo --locked --offline`, disabled Go package
network lookup, fixture-only Go tests and a disposable `Switch/build/tmp`
directory. No real SMS, token, endpoint, Keychain or queue is accessed.

Committed `tests/fixtures/go-state.json` and `go-state.swf1.hex` were produced by
the actual Go `encryptedState.save()` in `state.go`, through
`TestRustStateGoldenFixture`. The synthetic key is bytes `00` through `1f`.
The fixture contains a multi-chunk Telegram checkpoint with one acknowledged
chunk and an unknown next result, a permanent Bark failure, a legacy pending
entry with no destination identity/progress, and terminal diagnostics.

Rust decrypts the fixture byte-for-byte and, **in a private synthetic unit test
only**, reproduces the exact Go ciphertext with that same key/nonce. Its
read-only projection also recomputes the Go `encoding/json` SHA-256 message and
frozen-chunk checkpoints, including Go HTML escaping and `time.Time` formatting.
The script writes a freshly encrypted Rust result with future JSON fields; Go's
real `openEncryptedStateWithKey` reads it and `telegramProgress.valid` validates
the old digests. An independent Go GCM decode checks exact raw-byte preservation.

Regeneration is deliberate: run the Go fixture test with
`SWITCH_FORWARDING_GENERATE_FIXTURES=1` using the isolated workspace environment.
The nonce changes on regeneration, so review both fixture files together. The
normal script never rewrites committed fixtures.

## Explicitly not migrated

- Production queue owner, cross-language writer coordination during cutover,
  coverage of all historical digest
  variants beyond the synthetic golden, first-enable cursor/seed handoff,
  retry/unknown-result policy. Queue admission is not connected to `switchd-rs`.
- Crash/reopen recovery under process-kill testing, Keychain or CLI sidecar-key
  lifecycle.
- Bark configuration validation and HTTP delivery; Telegram HTTP/proxy/PAC
  behavior, chunk checkpoint mutations and HTTP response validation; Bark
  request encryption (CBC is separate from this GCM envelope).
- Any daemon route, runtime provider, readiness/ownership count or production
  fallback. Existing Go remains the only forwarding owner.
