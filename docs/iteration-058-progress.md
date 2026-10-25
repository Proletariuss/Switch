# Iteration 058 — Rust profile metadata and eSIM download-size estimate

Date: 2026-10-05  
Goal: Switch full Rust backend migration; no DMG generated.

## Changes

- Added bounded BER-TLV parsing for the GSMA profile metadata root tags `E3` and `BF25`, extracting the optional estimated size (`9F21`), service-provider name (`91`) and operator PLMN (`B7/80`). It also matches Go's rejection cases for signed state/icon/class fields, profile policy bit strings, nested BER-TLV and notification event ranges. Malformed or oversized data is rejected before the confirmation callback.
- Ported the existing Go fallback size-selection rules into the Switch Rust LPA crate. A positive SM-DP+ size wins; otherwise matching uses normalized RSP, provider and PLMN scores, then the existing NekokoLPA2-derived reference table and EUM size map. The table is included from its existing Switch path rather than duplicated. The explicitly excluded 9eSIM `6982` path was not changed.
- Added `download_with_eid` and a default observer method carrying parsed metadata plus the estimated byte count, while preserving existing observers through the previous callback. On malformed metadata, Rust cancels with `MetadataMismatch`; when the user declines confirmation, it uses the same `Postponed` cancellation reason as the current Go implementation.
- Fields not consumed by the download confirmation are validated when they can fail Go decoding, while the observer DTO stays limited to size estimation and presentation fields.
- This is still crate-level behavior. The app's Go eSIM route does not call the Rust LPA state machine or display this callback, and operation recovery, unique reader lease, post-install verification and device use remain outstanding.

## Validation

- `cargo +1.98 test --offline -p proletarius-lpa`: 24 passed.
- `cargo +1.98 test --workspace --offline --quiet`: 733 passed, 0 failed, 11 ignored; ignored compatibility fixtures require separate Swift/Go scripts. Log: `build/optimization/2026-10-05-celmux-profile-metadata-tests.log`.
- The tests cover the exact Go AuthenticateClient metadata fixture, both metadata root tags, signed size parsing, malformed/trailing BER-TLV rejection, invalid policy/notification values, RSP/provider/PLMN/EUM reference selection, a declared size taking precedence, the 39,008-byte reference size reaching the download confirmation observer, rejection cancellation, and no card prepare/install after malformed metadata.
- `cargo +1.98 fmt --all -- --check` and `git diff --check` passed. The full suite emitted the two pre-existing `native_vowifi_session.rs` dead-code warnings. No real network, SM-DP+, reader, SIM, profile download or install was used.

## Progress estimate

Overall remains **17.5%** (8.305 / 47.5 planned person-weeks, ±3 percentage points). Metadata parsing and size estimation are tested inside an isolated LPA crate but still have no production route, UI confirmation, operation ledger or reader lease, so they add no production migration credit. The 9eSIM `6982` issue remains excluded.
