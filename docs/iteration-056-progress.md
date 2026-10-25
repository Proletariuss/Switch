# Iteration 056 — Celmux SGP.22 LPA state-machine reuse

Date: 2026-10-05  
Goal: Switch full Rust backend migration; no DMG generated.

## Changes

- Reused Celmux MIT `crates/euicc/lpa/src/lib.rs` and `activation.rs` in the new `rust/crates/proletarius-lpa`, preserving the original copyright and SPDX headers.
- Kept card operations on Switch's existing `proletarius-euicc` API and the ES9+ network boundary behind `Es9Transport`. Replaced Celmux's `ureq::http::Uri` dependency with Switch's existing `http::Uri`; did not copy `HttpEs9Client`, DNS, proxy, TLS, or its certificate-verification policy.
- Replaced Celmux's logger call with the host `DownloadObserver::on_warning` hook so the caller owns diagnostics. The main network failure remains the returned error if best-effort remote cancellation also fails.
- Added in-memory fake card/ES9 tests for successful stage order, user rejection cancellation, network failure preservation, notification retention until success, activation-code validation, and confirmation-code display handling.
- Updated `THIRD_PARTY_NOTICES.md` and the Celmux reuse review with exact source hashes, MIT scope, and the fact that the new crate is not connected to production downloads.

## Validation

- `cargo +1.98 test --offline -p proletarius-lpa -p proletarius-euicc`: five new LPA tests and three existing eUICC profile-codec tests passed.
- `cargo +1.98 test --workspace --offline`: 714 passed, 0 failed, 11 ignored across the workspace. The ignored fixtures require separate Swift/Go compatibility scripts; the Rust workspace test command exited successfully. Log: `build/optimization/2026-10-05-celmux-rust-workspace-tests.log`.
- `cargo +1.98 fmt --all -- --check`, `git diff --check`, and the migration-contract inventory check passed. The inventory remains at 187 contracts (77 declared Rust owners, 110 Go owners, 127 Swift call sites, 38 fingerprinted review scopes); all 16 scanner regression tests passed. LPA added no API route, so the reviewed inventory did not change.
- Clippy could not run because the locked Rust 1.98 toolchain has no Clippy component installed; no component was installed.
- Tests use only local fakes. No network, App, SIM, card, profile download, or installation was performed.

## Progress estimate

Overall remains **17.5%** (8.305 / 47.5 planned person-weeks). The LPA state machine is protocol groundwork only: no production ES9 HTTP transport, Rust download route, persistent operation recovery, reader lease, profile-size confirmation UI, or device acceptance exists yet. It therefore does not count as production LPA migration. The 9eSIM `6982` issue remains excluded.
