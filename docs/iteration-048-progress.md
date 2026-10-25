> 历史记录：以下内容来自 Proletarius，包含当时的功能、路径、发布要求与验证结果，仅供追溯，不是 Switch 的当前指令或验收结论。Switch 已移除传真，当前构建与执行范围以根 README 和桌面工作区改造方案为准。

# Iteration 048 — ESTK BLE transport parity and reader-page hierarchy

Date: 2026-09-28

## Completed scope

- Compared the checked-out NekokoLPA2 source at commit
  `013eadad7037248531f1b1532ebf344390d57087`: its `ESTKme Red` discovery
  selects RED2/10-byte CCID framing; claim, optional continuation, power-on,
  and APDU envelopes match Proletarius's existing CoreBluetooth path.
- Closed a transport gap found in that comparison: discovery accepted a GATT
  writer with only `.write`, but all production packets were sent as
  `.withoutResponse`. The manager now mirrors NekokoLPA2's characteristic
  capability policy, uses response-paced sequential chunks when `.write` is
  selected, sizes chunks using the matching CoreBluetooth limit, and revokes
  the uncertain session on a write error. USB PC/SC and the existing CCID,
  claim, reset, card-lease and reader-generation behavior are unchanged.
- Added a test using the observed `ESTKme Red:205C36914730` style name and
  deterministic checks for write-only, write-without-response and dual-mode
  characteristics, sequential chunk acknowledgement, byte preservation and
  error revocation.
- Reworked the reader card summary into a clearer hierarchy: carrier/profile
  and number are primary; ICCID, IMSI and IMS identities are grouped under one
  collapsed technical-details disclosure. Removed the duplicate eSIM refresh
  control because the existing card refresh already refreshes both card and
  profile snapshots. Consolidated rename/delete into a per-profile menu while
  keeping enable as the primary action. No operation/API was removed.

## Verification

- `sh scripts/test-ble-manager.sh` — 25 production-manager event-sequence
  groups passed, including BLE phase failures/recovery and CCID transactions;
  virtual-time adapters only, no radio/hardware.
- `sh scripts/test-reader-esim-identity.sh` — 8 reader/Profile identity groups
  passed; `sh scripts/test-vowifi-readiness.sh` passed; Go reader CardService
  and NetworkService targeted tests passed; BLE phase deadline/cancellation
  tests passed.
- `cargo check --locked --offline --workspace` passed. The first Xcode build
  attempt encountered a transient Cargo lock check failure; a direct locked
  release build then passed and a clean repeat of the full Xcode Release build
  also passed. Existing Rust dead-code and duplicate libusb-linker warnings
  remain.
- `git diff --check` passed for the changed reader, BLE tests and view files.
- `system_profiler` showed Bluetooth on but no ESTK/USB SIM reader attached;
  only unrelated peripherals were listed. Therefore no physical BLE connect,
  APDU, eSIM switch, VoWiFi registration, SMS or call was performed.
- Version advanced from 2.57 to 2.58 (build 258). Full arm64 Release build
  targeting macOS 27.0 succeeded and packaged the complete fax-enabled app.
  The DMG passed `hdiutil verify`; the `dist` and Desktop `test` copies have
  identical SHA-256:
  `c623bcc1424e806c0bdf5858492f7e9fb720e83646f7c968c4ec34eac582dcd4`.
  The build used local ad-hoc signing because no Developer ID identity was
  configured; this is not notarized. No UPX compression was applied.

## Decision and boundary

Adopt the GATT write-mode correction and simplified reader layout. The local
code/regression/build/package portion of the current objective is complete, but
physical ESTKme-RED 2.0.3 and carrier-service acceptance remain unverified.
Deliver the artifact as locally built and regression-tested; do not claim BLE
hardware functionality from simulator tests.
