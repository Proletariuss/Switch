> 历史记录：以下内容来自 Proletarius，包含当时的功能、路径、发布要求与验证结果，仅供追溯，不是 Switch 的当前指令或验收结论。Switch 已移除传真，当前构建与执行范围以根 README 和桌面工作区改造方案为准。

# Iteration 038 — Reader lease renewal

## Scope

The Rust native reader session lease was previously protected by a fixed
two-minute Go watchdog, but the private Rust client had no renewal operation.
That was insufficient for any future session actor that owns a reader across
long-lived IKE/IMS registration.

## Changes

- Added a private authenticated `session-lease/renew` endpoint and Rust client
  method. Renewal extends only the matching live lease; an expired or stale
  token cannot reacquire reader ownership.
- Stored the lease deadline and made the expiry callback re-check it while
  holding the same mutex used by renewal, AKA handoff, and release. A callback
  already queued when renewal arrives now reschedules instead of releasing the
  reader underneath the session.
- Added Go lifecycle/handler tests and Rust wire-contract coverage for renewal.
- Incremented the app version from 2.16 to 2.17 (build 217).

## Verification and limits

The focused Go lease tests pass. Renewal is still a gated building block: the
native Rust VoWiFi actor does not yet consume it, and the production ownership
gates remain closed. This iteration therefore does not claim Rust-owned IMS,
SMS, incoming calls, or an attached-reader live acceptance test. The DMG is a
macOS 27 build of the current integrated app and does not change that gate.
