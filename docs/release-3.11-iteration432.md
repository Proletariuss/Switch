# Switch 3.11 / Build 311 — Iteration 432

- DMG: [`Switch-3.11-macOS27-iteration432.dmg`](../../dist/Switch-3.11-macOS27-iteration432.dmg)
- Source: current working tree under `Switch/src`
- Architecture: arm64; minimum macOS 27.0
- App version: 3.11 (311)
- SHA-256: `6ca502756514575f442e3159e8a635129824361df4fb3b43fcfdd352b948c656`
- Signing: local ad-hoc signature; no Developer ID notarization ticket
- Validation: Xcode Release build succeeded; `codesign --verify --deep --strict` passed for the app; `hdiutil verify` passed for the DMG; archive copy SHA-256 matched.
- Scope note: this package contains the in-progress source snapshot at packaging time. Later Iteration 432 work removed stale Rust ownership classification for the retired private download path and added owner plus synthetic HTTP/UDS integration regression tests; these follow-up source changes are not in this DMG. No new DMG was requested for that follow-up. Physical reader/carrier acceptance was not performed.
