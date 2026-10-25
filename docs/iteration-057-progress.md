# Iteration 057 — Switch-owned Rust ES9+ HTTPS adapter

Date: 2026-10-05  
Goal: Switch full Rust backend migration; no DMG generated.

## Changes

- Added `HttpEs9Client` to `rust/crates/proletarius-lpa/src/es9_http.rs` and exported it from `proletarius-lpa`.
- Followed the current Go ES9+ transport contract: GSMA CI production and test root bundles, Admin Protocol `gsma/rsp/v2.5.0`, `gsma-rsp-lpad` user agent, direct HTTPS, redirects disabled, and per-call deadlines. TLS verification remains enabled; Celmux's `disable_verification(true)` behavior was not adopted.
- Kept the request/response path bounded: 1 MiB ES9 control JSON, 24 MiB BPP response JSON, 16 MiB decoded BPP, and 65,535-byte TLV fields. Added transaction ID, status, Base64, HTTPS authority and notification endpoint checks. Transport errors omit reqwest's raw diagnostic text to avoid echoing request or URL details.
- Reused the workspace's pinned reqwest 0.13.5 and enabled its blocking client for the LPA crate. The synchronous LPA/PCSC work must run on a dedicated blocking worker; no daemon/API call site uses the adapter yet.
- Kept Celmux's MIT attribution on the adapted implementation and documented the exact protocol/TLS/production boundary in `THIRD_PARTY_NOTICES.md` and the Celmux review.

## Validation

- `cargo +1.98 test --offline -p proletarius-lpa -p proletarius-euicc`: 12 LPA tests and 3 eUICC profile-codec tests passed.
- `cargo +1.98 test --workspace --offline`: 721 passed, 0 failed, 11 ignored. Ignored items are existing fixtures that require separate Swift/Go compatibility scripts. Log: `build/optimization/2026-10-05-celmux-es9-workspace-tests.log`.
- A local loopback HTTP fixture exercised request headers and JSON, HTTP 503 handling and body-size rejection; tests used no external server. Root bundle parsing and reqwest client construction were also tested.
- `cargo +1.98 fmt --all -- --check` and `git diff --check` passed. Clippy could not run because the locked Rust 1.98 toolchain has no Clippy component installed; no component was installed.
- No external network, SM-DP+, app, reader or physical card was used. No profile was downloaded or installed.

## Progress estimate

Overall remains **17.5%** (8.305 / 47.5 planned person-weeks, ±3 percentage points). The ES9 adapter is tested but no production route, download operation state, pre-install size confirmation, recovery ledger or reader session lease calls it. Go still owns production eSIM routes, so this work adds no production migration credit. The 9eSIM `6982` issue remains excluded.
