# Iteration 447：将 3 HK 用户模板的 PANI 策略接入 Rust IMS profile

日期：2026-10-13

## 改动

- Rust native IMS profile resolver 只在解析结果为 `apple_hutchison_hk` 且归属 PLMN 为 454/03 时，读取应用自有 `3_hk.json` 中的 `include_pani_authenticated`。
- The user-template value is applied before an explicit `carrier_overrides.yaml` setting, so an operator override remains authoritative. 454/04 and other carrier selections retain their existing defaults.
- The 3 HK JSON now records `include_pani=false` and `include_pani_authenticated=false` explicitly, matching the inspected Clarivate template. Go's typed template already resolves both to false; its optional false JSON serialization remains unchanged.
- The carrier refresh audit records that Clarivate's carrier YAML is older than Switch's pinned e560437 registry, and documents the Rust template parity boundary.

## 验证与范围

- Rust `proletarius-vowifi`: **181 passed / 0 failed**, plus 2 carrier integration tests passed.
- Native session owner tests: **52 passed / 0 failed**; `cargo check -p proletariusd` passed with 17 dormant-code warnings.
- Go carrier package tests passed.
- `rustfmt --edition 2024 --check` for the changed Rust source, `git diff --check`, and migration contract `--diff` passed; census remains 354 contracts (Rust 293 / Go 61), 127 Swift call sites, 378 review scopes.
- The new regression verifies 454/03 reads false, 454/04 keeps its prior true default, and an explicit 454/03 override restores true.
- This remains gated native IMS configuration work. It does not enable production registration or prove live SIMworld VoWiFi. No SIM/APDU, network, call, or SMS action was performed.
- Overall migration remains **37.99% (18.045/47.5)** because the profile input is not yet connected to production IMS ownership or hardware acceptance. No DMG generated.

## 下一步

Continue closing the native IMS template and runtime ownership gap: consume the full carrier-resolved REGISTER policy from one immutable session profile, retain Go-equivalent header/retry/error behavior, and keep startup gates closed until endpoint authorization, event routing, cleanup, and offline parity are complete.
