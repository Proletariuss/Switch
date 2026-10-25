> 历史记录：以下内容来自 Proletarius，包含当时的功能、路径、发布要求与验证结果，仅供追溯，不是 Switch 的当前指令或验收结论。Switch 已移除传真，当前构建与执行范围以根 README 和桌面工作区改造方案为准。

# Iteration 041 — Ordered native session stop boundary

## Changes

- Kept the native reader VoWiFi startup path fail-closed and private.
- Split actor cancellation from IKE stop ownership. A temporary bridge forwards
  actor stop only while transport/tunnel startup is in progress; the IKE owner
  retains its own stop channel after startup.
- Extended the move-only tunnel owner to retain the reader lease heartbeat and
  IKE stop sender, and expose a lease-loss receiver. The future keyed actor can
  therefore close protected IMS first, then request the IKE DELETE.
- Added a regression test proving actor cancellation reaches only the private
  startup IKE channel.
- Synchronized release metadata to version 2.20 / build 220.

## Verification and limits

- `cargo +1.98.0 fmt --all -- --check` passed.
- `cargo +1.98.0 test -p proletariusd -p proletarius-vowifi` passed:
  daemon 159 tests, VoWiFi 129 tests.
- `git diff --check` passed.
- macOS 27.0 arm64 Release app build, embedded Go/Rust backend signing,
  mounted-app version/build/minimum-OS/architecture checks, deep signature
  validation, and `hdiutil verify` passed. Artifact:
  `dist/Proletarius-2.20-macOS27.dmg` (42,334,427 bytes; SHA-256
  `1f1e9680569f7d24543eb4b3942891efdd551ce102efe9c1c74a1bc5e1f01f6d`).
  Xcode reported existing macOS 27 deprecation warnings. No carrier live call,
  SMS, incoming-call, or reader test was performed.
- No production owner changed. Rust ownership remains 9/15 (60%); Go continues
  to own the live reader VoWiFi session until the keyed actor, protected IMS,
  SMS/call runtime, and real-device gates are complete.
