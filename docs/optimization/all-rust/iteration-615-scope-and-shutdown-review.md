# Iteration 615 — 9eSIM scope guard and LPA shutdown drain review

This iteration records a scope/acceptance audit only. It does not change product runtime code, add a scored requirement, award functional acceptance, freeze the denominator, or produce a DMG.

## 9eSIM scope

The current user instruction excludes only the partner-specific encrypted profile-download path that returns `6982`. B0/B1 password and BF71 authentication, BLE connection, EID/Profile reading and switching, and supported normal authenticated downloads remain in scope. Authentication bypass, reverse engineering of the excluded path, or claims that `6982` is fixed remain prohibited.

The acceptance metadata check passed with 294 API entries, 131 Swift-call entries, 56 runtime entries, 115 requirement leaves, and 21 scenario classes. The overall denominator remains unfrozen and the checker awarded no functional acceptance. A regression test now asserts both the exclusion and the retained 9eSIM members. All 48 acceptance-ledger tests pass.

## LPA shutdown drain

Four targeted Rust tests passed:

- `shutdown_closes_esim_admission_and_waits_for_owned_operation`
- `retained_reader_owner_survives_cancelled_http_waiter_and_shutdown_drains_it`
- `shutdown_module_finalizer_waits_for_accepted_esim_owner_drain`
- `reader_download_gate_stays_held_until_owner_cleanup_finishes`

The source review confirms the reader download owner keeps the shared operation gate through durable result completion, reader-lease release, and final reader refresh. Shutdown first closes admission, then waits for that gate before module-radio final receipts. The tested guarantee is this owner cleanup drain; it is not a claim of physical LPA download acceptance. This supersedes the stale Iteration 605 summary that described the reader-owner drain as still pending a shutdown join.

Machine-readable evidence: [Rust shutdown tests](../../../../build/reports/iteration-615-lpa-shutdown-tests.json), SHA-256 `5e8d986d42fe275f43548aff54e7b6c2a9e542e176c89ebe5b5c664568e1fec6`; [acceptance ledger check](../../../../build/reports/iteration-615-ledger-check-final.json).

## P6 boundary retained

The Iteration 612 synthetic 3.42→3.46→3.42→3.46 roundtrip remains partial ([full report](../../../../build/reports/iteration-612-p6-roundtrip-342-to-346.json)). Both 3.42 copied-bundle runs exit 1 with `call_mode_shutdown_unconfirmed`: the hardware-denied sandbox blocks IOKit and Mach lookup, so the legacy 3.42 module shutdown cannot find a supported USB ADB interface and cannot issue its native shutdown receipt. The 3.46 candidate exits cleanly and quarantines legacy module recovery state. This is a test-environment limit for the old binary, not proof that rollback is safe; do not mark P6 complete or weaken the sandbox. A module-backed rollback/shutdown validation or an explicitly accepted support boundary is still required.

The last complete product-code gate remains Iteration 612: Rust 3927 passed / 0 failed / 9 ignored, Python 100 passed, 297 protocol replays, and 6466 oracle cases. No product runtime code changed in this iteration, so no DMG was built.

An interrupted default-path Cargo compile created a duplicate partial `src/rust/target` cache. After confirming no process held it and preserving the established `build/rust-target`, the duplicate cache was removed (392,063,701 file bytes); no source, dependencies, release artifacts, or test evidence were removed.
