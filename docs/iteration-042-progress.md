> 历史记录：以下内容来自 Proletarius，包含当时的功能、路径、发布要求与验证结果，仅供追溯，不是 Switch 的当前指令或验收结论。Switch 已移除传真，当前构建与执行范围以根 README 和桌面工作区改造方案为准。

# Iteration 042 — Cancellation-safe IMS-before-IKE shutdown

## Baseline and decision

The 2.20 owner had a private IKE stop sender, but there was no callable bridge
that guaranteed `RegisteredImsSession::shutdown()` completed before IKE was
allowed to send its protected DELETE. Awaiting cleanup directly in a keyed
actor would also let actor cancellation drop the cleanup future. A short-lived
Tokio task on shutdown is lower cost than risking an orphaned IMS security
lease or violating the IKE/IMS teardown order.

## Changes

- Added a gated `NativeTunnelOwner::shutdown_registered_ims` helper. It runs
  the move-only IMS shutdown in an independent task, retains its cleanup
  report, then signals only this tunnel owner's IKE stop channel.
- The signal is sent after shutdown returns even when IMS reports a cleanup
  error, so the network owner can still make bounded progress toward teardown.
- Added a failure-injection test: pause IMS cleanup, drop the actor's join
  handle, prove IKE remains active, then return a cleanup error and prove IKE
  stop is signalled afterward.
- Advanced release metadata to 2.21 / build 221.

## Verification and limits

- Rust daemon: 160 tests passed; formatting, `git diff --check`, and optimized
  `proletariusd` build passed.
- No live reader/IMS/SMS/call path or public route consumes this gated helper.
  No reader, carrier, proxy, or operator network was exercised.
- Production ownership remains 9/15 (60%). The next material integration is
  the keyed actor joining the native network task with IMS REGISTER and then
  SMS/call lifecycle before any production route can be enabled.
