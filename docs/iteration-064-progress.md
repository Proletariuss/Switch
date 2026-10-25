# Iteration 064 — Celmux SIP core reuse and Rust profile rename route

Date: 2026-10-05  
Goal: continue the approved full-Rust backend migration; use `/Users/proletarius/Downloads/celmux-src/` as a read-only reference; no DMG.

## Celmux reuse

The supplied snapshot is a Rust workspace with substantial overlap in eUICC/LPA, AT, SIP/IMS, VoWiFi, and device management. Switch already reuses the MIT CCID/eUICC/LPA protocol code and adapts Celmux's AT transaction boundaries inside Switch's single serial-port owner. This iteration added the transport-independent SIP parser/message model, dialog/CSeq/route-set handling, and RFC 3261/6026 client/server transaction state machines to a new `proletarius-sip` crate.

Only `crates/sip/core/src/lib.rs`, `dialog.rs`, and `transaction.rs` were reused. Their file headers identify MIT. The Celmux crate manifest says BSD-2-Clause, while `auth.rs` is itself BSD-2-Clause; `auth.rs` was intentionally not copied because Switch already has its own IMS AKA/Digest implementation. No Celmux SIP socket/runtime, IMS service, VoWiFi session owner, QMI stack, or UI was copied. The exact source hashes and cross-module reuse decisions are recorded in [Celmux 复用评估](optimization/2026-10-04-celmux-reuse-review.md), and the complete MIT notice is in `THIRD_PARTY_NOTICES.md`.

Four new offline contract tests cover fragmented/coalesced SIP streams with keepalives, conflicting/oversized framing, dialog route-set order and ACK/BYE CSeq, and INVITE client/server ACK/CANCEL/timer transitions. This crate is not yet called by the production IMS owner; its availability does not count as Go business migration or real-network readiness.

## Rust eSIM profile rename

The existing `PATCH /api/reader/esim/profile` contract now dispatches to Rust for selected USB/PCSC readers. Rust validates the ICCID, optional AID, and bounded profile name; uses the per-reader operation lock; performs the nickname APDU; and refreshes the profile snapshot after success. BLE and unsupported readers retain the established Go fallback.

The Go compatibility boundary first obtains its reader operation gate, then stops ordinary VoWiFi only after admission. Multi-standby conflicts therefore reject before VoWiFi is interrupted. The gate is atomically transferred to the identity-free Rust mutation lease. No subscriber identity is sent for the profile rename. The existing route, JSON response contract, operation-conflict behavior, and Swift request fields are unchanged.

This moves one profile operation to Rust but does not remove the cross-process Go lease coordinator, cache refresh, VoWiFi stop, or BLE fallback. It does not cover all profile management or count as end-to-end reader ownership.

## Verification

- `cargo +1.98 fmt --all -- --check`: passed.
- `cargo +1.98 test --offline --workspace`: passed across the complete workspace; `proletariusd` reported 319 passed, 0 failed. The new `proletarius-sip` integration suite reported 4 passed, 0 failed.
- `go test ./cmd/proletarius-macos -count=1`: passed; linker printed an existing duplicate `-lusb-1.0` warning.
- `python3 scripts/tests/test_rust_migration_contracts.py`: 16 tests passed.
- `python3 scripts/check-rust-migration-contracts.py`: passed after the manual review; 187 method/path contracts, 82 Rust-owned handlers, 105 Go-owned handlers, 127 Swift HTTP call sites, and 38 fingerprinted review scopes.
- No Clippy result is claimed; the locked Rust toolchain has no Clippy component installed.

All eSIM/APDU tests used synthetic data. No App was opened, no reader or SIM was accessed, no SMS was sent, and no DMG was generated. 9eSIM `6982` remains excluded.

## Progress

Only the eSIM/LPA package estimate moves from 13% to 14% for the production USB/PCSC profile-rename route plus offline compatibility evidence. The SIP protocol crate is groundwork only and does not increase P3 because no production IMS path calls it yet. On the unchanged 47.5 person-week baseline, the weighted total is `9.235 / 47.5 = 19.4%` complete and `80.6%` remaining (approximately ±3 percentage points). The route census now records 187 method/path contracts, with Rust handler ownership 82 and Go ownership 105; this entry-point count is not the overall migration percentage.
