> 历史记录：以下内容来自 Proletarius，包含当时的功能、路径、发布要求与验证结果，仅供追溯，不是 Switch 的当前指令或验收结论。Switch 已移除传真，当前构建与执行范围以根 README 和桌面工作区改造方案为准。

# Iteration 047 — Reader card-service lease integration

Date: 2026-09-28

## Change

- Routed reader eSIM overview, explicit overview refresh, health snapshot, and
  profile-switch APDU through the reader card-service adapter.
- The adapter serializes card access with the reader transaction mutex, fences
  each operation against the selected reader generation, and supports
  cancellation while waiting for the card lease.
- Kept profile-switch lifecycle orchestration in the existing HTTP operation:
  stop VoWiFi, report progress, rebuild the selected reader session, send/verify
  pending operator notifications, and wait for the resulting card identity.
- Added fault-injection coverage for a reader replacement during the switch
  APDU and for cancellable lock waiting / overview refresh failure. This keeps
  successful APDU acceptance distinguishable from stale-reader completion.

## Verification

- `go test ./...` — passed after the overview-service routing and fault-injection
  assertions were complete.
- `cargo test --workspace` — passed (all Rust workspace tests; two existing
  unused-field warnings in `proletariusd` remain).
- BLE manager event sequences: 24 groups passed with virtual time; BLE phase
  cancellation stress (100 cycles), manual reader selection (13 cases),
  eSIM identity (8 groups), and VoWiFi readiness scripts passed.
- Host inspection found no USB CCID/eSIM reader attached and no ESTK reader in
  the connected Bluetooth device inventory. No physical profile switch,
  VoWiFi registration, SMS, call, or sleep/reconnect test was possible.

## Decision and remaining risk

Adopted as a small, reversible consistency improvement: all UI-facing reader
eSIM overview reads and profile APDUs now share one generation-fenced card
lease, while established post-switch behavior remains unchanged. No proxy,
NAS, carrier, or power settings were touched. This iteration does not prove
multi-reader concurrent IMS isolation, long-duration stability, or hardware
BLE transport correctness. Re-run the reader-to-IMS physical smoke test when
ESTKme-RED 2.0.3 or a USB CCID reader is available; do not treat these local
tests as a substitute.
