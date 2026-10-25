# Iteration 065 — Rust eSIM profile deletion route

Date: 2026-10-05  
Goal: continue the approved full-Rust backend migration; preserve existing API and Swift behavior; no DMG and no physical-card operation.

## Change

`DELETE /api/reader/esim/profile` now dispatches selected USB/PCSC readers to the Rust eSIM worker. The dispatcher retains the Go compatibility path for BLE or unsupported reader transports. Rust validates the request and target ICCID, preserves the existing conflict response, obtains the mutation lease through the existing Go coordinator, serializes work per reader, and checks cancellation/lease health while operating.

The Rust worker opens the selected eUICC, confirms the requested profile exists, captures the notification cursor and available storage, issues the ES10c delete operation with the existing ICCID/ISD-P AID fallback, and verifies that the profile is gone. A matching deletion notification is sent to ES9+ before removing it from the card; failed delivery leaves the notification on the eUICC for retry. The response retains the existing DTO and includes the available-storage delta. Existing Go VoWiFi stop, cross-process reader lease transfer, post-operation card cache refresh, BLE fallback, and operation conflict coordinator remain dependencies.

The route body remains capped at 8 KiB and participates in the daemon's aggregate request-body budget. API path, method, JSON contract, and Swift call sites are unchanged. 9eSIM `6982` handling is explicitly out of scope.

## Verification

- `cargo +1.98 test --manifest-path rust/Cargo.toml --offline --workspace`: 759 passed, 0 failed, 11 ignored; `proletariusd` reported 325 passed.
- `go test ./cmd/proletarius-macos -count=1`: passed; linker emitted the existing duplicate `-lusb-1.0` warning.
- Workspace `cargo fmt --check`, the route/consumer contract scanner, its 16 self-tests, and `git diff --check`: passed.
- Contract census: 187 method/path contracts, Rust handler owner 83, Go owner 104, 127 Swift HTTP call sites, and 48 fingerprinted review scopes. This inventory is an entry-point audit, not end-to-end Rust completion.
- Rust emitted two existing unused-field warnings in `native_vowifi_session.rs`; Clippy was not run because it is absent from the locked toolchain.

Tests used synthetic eUICC/APDU and notification fixtures. No app was launched, no reader or physical SIM was touched, no SMS or real network request was made, and no profile was downloaded, installed, or deleted. No DMG was generated.

## Progress

The eSIM/LPA work-package estimate moves from 14% to 16% for selected USB/PCSC profile deletion and its offline failure-boundary coverage (+0.11 planned person-week). P2B remains 22%; no credit is assigned for remaining Go lease/VoWiFi/cache dependencies, BLE, profile activation/switching, notification management, crash recovery beyond the tested deletion boundary, or hardware/carrier acceptance. On the unchanged 47.5 person-week baseline, the weighted total is **9.345 / 47.5 = 19.7% complete**, with **80.3% remaining** (approximately ±3 percentage points).

Next: migrate profile activation/switching and notification-management routes while preserving the single-reader ownership boundary, then remove Go's lease/coordinator dependencies and expand crash/restart and hardware acceptance evidence.
