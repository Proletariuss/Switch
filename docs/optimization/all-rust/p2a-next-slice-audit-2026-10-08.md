# P2A next-slice audit — 2026-10-08

## Decision

This branch does not contain a safe, substantial P2A business-ownership slice
that can be migrated independently. The forwarding and management semantics
that previously had a duplicate Go runtime are already executed by Rust. The
remaining Go-owned management routes are device observations or hardware
commands; changing their route owner alone would create a Rust facade around
Go behavior. No owner declaration, route, code path, or migration percentage
was changed during this audit.

The current census remains 285 method/path contracts: 213 Rust-owned and 72
Go-owned (34 HEAD aliases are derived contracts). The migration execution plan
records P2A at 68.36%; its remaining work is primarily acceptance and recovery
evidence rather than a second Go forwarding implementation.

## Evidence

- `ownership.rs` declares all six public `/api/sms/forwarding/*` actions Rust
  owned, and `main.rs` dispatches them directly to
  `sms_forwarding::Service::handle`. The Rust service starts and shuts down
  with the daemon. `proletarius-forwarding` owns configuration validation,
  SWF1 queue semantics, Telegram/Bark provider behavior, retries, unknown
  outcomes, and chunk checkpoints.
- The Go duplicate executor directory `backend/internal/smsforward` is absent.
  A production-source search found no import or reference to that package.
  `backend/internal/forwardinglockcompat` remains only for cross-process
  `.lock`/`flock` compatibility tests.
- Of the 72 Go-owned contracts, the P2A-adjacent
  `runtime_diagnostics` category has exactly three routes:
  `GET /api/health`, `GET /api/status`, and `POST /api/at`.
- `GET /api/health` in `backend/cmd/proletarius-macos/main.go` derives
  `esim_available` from the Go eSIM manager and returns USB discovery and
  discovery-error observations. Swift's `DeviceSnapshotStore` consumes this
  response. Rust's existing `/api/v2/runtime` is a backend readiness contract,
  not a wire-compatible substitute for this device health response.
- `GET /api/status` branches across the Go module cache, the Rust USB AT
  status actor, radio suppression, phone-number fallback, and reader-only
  discovery. It is a device-state projection spanning the P2B hardware owner;
  moving only the HTTP handler would leave those semantics in Go.
- `POST /api/at` parses and bounds a user-selected AT command before routing it
  to a module-radio or serial executor. It is a hardware-control boundary and
  needs the P2B command allowlist/physical-transaction review, not a P2A route
  flip.
- The initial audit found provider/PAC integration, process-level queue
  recovery/crash cases, producer-trigger coverage, and real provider/device
  acceptance gaps. Iteration 163 covered empty-ledger upgrade and provider-unknown
  crash/restart/no-auto-replay. Iteration 165 now covers the N-wait IMS event and
  Go device-archive event paths through the Rust producer to a synthetic Bark
  endpoint. PAC integration and real provider/device acceptance remain open.
  These gaps do not identify a live Go business implementation to port.

## Verification run

- `scripts/check-rust-migration-contracts.py`: passed; census stayed at 285 /
  Rust 213 / Go 72 / 127 Swift calls / 251 review scopes.
- `scripts/tests/test_rust_migration_contracts.py`: 43 passed.
- `cargo test --locked --offline -p proletarius-forwarding`: 55 passed,
  2 ignored cross-language lock fixtures; the Rust Telegram system-proxy
  re-resolution and provider/queue compatibility tests passed.
- Production Go source search for `internal/smsforward`: no references found.

These are offline repository checks. They do not certify Telegram/Bark delivery,
PAC evaluation against a user's live system configuration, or physical device
behavior.

## Recommended next work

Follow-up after Iteration 165: process-level queue recovery covers a legacy
empty admission ledger and provider-unknown crash/restart without automatic
replay; explicit retry is verified. Process tests also cover both N-wait IMS
inbound and Go device archive triggers through Rust queue admission to a local
Bark endpoint. Next assurance should target a stable isolated CFNetwork/PAC
fixture and real provider/device acceptance.

If the next branch must move a public P2A-adjacent route owner, the smallest
candidate is `GET /api/health`: define a generation-bound, typed Rust snapshot
for the eSIM/USB discovery facts, let Go publish only those source observations,
and have Rust produce the existing response shape. This is a small service
slice rather than a substantial one. The larger `GET /api/status` projection
should be coordinated with P2B because its data collection and control policy
are hardware-owned. Do not claim either route is Rust-owned until response
parity fixtures cover both module-attached and reader-only states, absent
optional data, discovery failures, and child replacement.

No DMG was built, and no device, SIM, or external network was used.

## Iteration 163 follow-up

The Rust forwarding admission store now persists only real state changes and
normalizes a validated legacy empty ledger. A synthetic loopback Bark server
receives the full request while withholding acknowledgment; the Rust daemon is
then killed and restarted against the same runtime directory. The task remains
`paused_result_unknown` without provider replay for a complete worker poll
interval, and an explicit retry succeeds. The Rust/Go process suite now has 19
passing scenarios. This closes the process-level unknown-outcome crash gap; it
does not test Telegram/Bark over the Internet, CFNetwork/PAC, or hardware.


## Iteration 165 follow-up

The 21-scenario Rust/Go process suite now proves that a new N-wait IMS inbound event and a device SMS submitted through Go `archiveSMSChecked` each reach the synthetic local Bark receiver once through the Rust producer/queue. The observed polling window did not duplicate either event. This verifies the two tested compatibility ingress paths; it does not certify physical SIM/USB capture or Internet delivery. Iteration 458 later closes the isolated CFNetwork/PAC evaluator fixture gap below; real provider and hardware acceptance remain open.

## Iteration 458 follow-up — local CFNetwork/PAC evidence

On macOS, `cargo test --locked -p proletarius-system-proxy` passed all 6 tests, including a no-network CFNetwork evaluation of an in-memory synthetic PAC script for proxy selection, DIRECT and candidate fallback. `proletarius-forwarding` passed 56 tests (2 Go-peer flock fixtures are intentionally ignored in the isolated run); `scripts/test-forwarding-lock-interop.sh` separately passed both Rust↔Go file-lock directions, and `scripts/test-forwarding-state-compat.sh` passed the SWF1 compatibility suite. This closes the isolated PAC-evaluator fixture gap noted above. It does not evaluate the machine's current System Configuration/PAC URL or verify Telegram/Bark over the Internet, so real provider/device acceptance remains open. No owner or migration weight changed.
