> 历史记录：以下内容来自 Proletarius，包含当时的功能、路径、发布要求与验证结果，仅供追溯，不是 Switch 的当前指令或验收结论。Switch 已移除传真，当前构建与执行范围以根 README 和桌面工作区改造方案为准。

# Iteration 039 — Lease-bound native IKE AKA

## Baseline and threshold

`NativeSessionContext` previously stored a Rust PC/SC `ReaderAkaBinding` even
though the same preparation request had just acquired a Go lease. That created
two different paths for the same reader identity and left ESTK BLE outside the
future IKE authentication path. Reusing the already authenticated, generation-
bound Go reader bridge costs one private Unix-socket request per AKA exchange;
it avoids implementing a second BLE/APDU transport and keeps the reader lock
authoritative, so the benefit exceeds the small per-challenge IPC cost.

## Changes

- The immutable native context now owns its `NativeReaderSessionLease` and its
  IKE authentication callback calls lease-authorized USIM AKA. The old direct
  PC/SC binding was removed from this future IKE path; SIM success, AUTS sync
  failure, and authentication rejection remain distinct in the lease codec.
- The diagnostic `native-session-plan` releases the lease only after it has
  formed the response, and releases it on preparation failure as well. The
  context's tunnel-start method can use the exact same lease and reader
  generation rather than rereading the active reader.
- Go transport comments now document the lease-authorized USB/PC/SC and ESTK
  BLE role. No public VoWiFi start path was enabled; readiness stays fail-closed.
- App/workspace version advanced to 2.18 / build 218.

## Verification and limits

Go `go test ./...` passed. Rust formatting check and the full `proletariusd`
and `proletarius-vowifi` suites passed (156 daemon tests and 129 VoWiFi tests).
No reader was attached for this turn; carrier registration, SMS and call
acceptance were not exercised. The context still has no keyed live actor, the
two-minute lease renewer is not yet consumed by a live session, and protected
IMS/SMS/Call are not wired into its lifetime. Ownership remains 9/15 (60%); no
DMG was built in this iteration.
