> 历史记录：以下内容来自 Proletarius，包含当时的功能、路径、发布要求与验证结果，仅供追溯，不是 Switch 的当前指令或验收结论。Switch 已移除传真，当前构建与执行范围以根 README 和桌面工作区改造方案为准。

# Iteration 040 — Native reader lease heartbeat

## Baseline and decision

The reader lease expires after two minutes. A prepared Rust context was bound
to its generation and used it for IKE AKA, but the gated tunnel-start path did
not renew the lease. Long setup or a future long-lived session could therefore
lose physical-reader exclusivity while its network side still appeared active.
One renewal every 30 seconds plus one retry only on failure is a small bounded
IPC cost and is cheaper than duplicating the working Go PC/SC/ESTK BLE driver
or allowing the lock to expire.

## Changes

- Added `NativeReaderSessionHeartbeat`: periodic lease renewal, one bounded
  retry, terminal lease-lost notification, and cancellation-safe task cleanup.
- The gated native tunnel startup now keeps the heartbeat alive through proxy
  transport setup and IKE/AKA. It refuses to return a tunnel if renewal fails;
  the move-only tunnel owner retains the heartbeat for subsequent supervision.
- Kept the public VoWiFi start route and ownership gates closed. The diagnostic
  plan still releases its lease at request completion; Go remains the live
  reader VoWiFi owner until the keyed actor also owns protected IMS/SMS/Call.
- Advanced and synchronized release metadata to 2.19 / build 219.

## Verification and limits

- Heartbeat Unix-socket tests cover successful renewal and clean stop, plus two
  rejected renewal requests followed by lease-lost and explicit release.
- Full Rust workspace tests passed (daemon: 158 tests; VoWiFi: 129 tests), as
  did formatting and `git diff --check`.
- macOS 27.0 arm64 Release App compiled with embedded Go and Rust backends;
  app version/build, deployment minimum, backend architectures and deep code
  signature validation passed. Xcode reported existing macOS 27 API-deprecation
  warnings. No 2.19 DMG or carrier live-call/SMS test was performed.
- Production ownership remains 9/15 (60%). The heartbeat is implemented in a
  gated tunnel-start path but no keyed actor calls it yet; lease-lost must next
  drive ordered IMS cleanup and IKE DELETE. SMS/call lifecycle and CTExcel /
  giffgaff real-device gates still block a truthful ownership increase.
