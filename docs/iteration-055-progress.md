# Iteration 055 — Rust actor-owned SMS storage reads

Date: 2026-10-05  
Goal: Switch full Rust backend migration; no DMG generated.

## Changes

- Added the authenticated private UDS `POST /api/internal/modem/read-stored-sms`. The Rust modem actor now owns the serialized CPMS query/select/verify, indexed CMGR or full CMGL snapshot, and restore/verify transaction. The operation is bounded, checks the expected SIM generation, and cannot be interleaved by other queued AT commands.
- Routed production Go `Manager` single-message reads and full SMS snapshots through that actor transaction when the Rust modem client is present. Go retains SMS codec adaptation, durable archive acknowledgement, callback delivery, multipart assembly, its compatibility path, and deletion eligibility.
- On uncertain storage selection, Rust attempts to restore the previous three CPMS selections before returning. A PTY regression test confirms the restore sequence completes before an already queued high-priority command.
- Extended the manual API census to enumerate all seven internal modem POST actions and reviewed relevant Rust/Go dispatch fingerprints. The contract inventory now lists 187 method/path entries, including 77 Rust-owned route handlers; this count is an API inventory, not migration completion.

## Validation

- `cargo +1.98 test --manifest-path rust/Cargo.toml --offline -p proletariusd`: 309 passed.
- `go test -race ./internal/modem ./cmd/proletarius-macos`: passed.
- `go vet ./internal/modem ./cmd/proletarius-macos`: passed.
- Migration contract scan and scanner self-tests: passed.
- Device hardware and real SIM/SMS behavior were not exercised. No real messages were read/sent and no records were deleted.

## Progress estimate

P2B hardware/LPA increased from 19% to 20% (+0.08 planned person-weeks for the production-routed transaction and fault test). Overall estimate increased from 17.3% to 17.5%: 8.305 / 47.5 planned person-weeks. The estimate remains ±3 percentage points and does not mean device acceptance or Go removal is complete.

Test logs are kept under `build/optimization/2026-10-05-rust-sms-read-*` (Rust test/fmt, Go race/vet, contract scan/self-test, and diff check).
