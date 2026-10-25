# Switch 3.12 / Build 312

- DMG: [`Switch-3.12-macOS27.dmg`](../../dist/Switch-3.12-macOS27.dmg)
- Local archive: [`Switch-3.12-macOS27.dmg`](../../dist/archive/Switch-3.12-macOS27.dmg)
- Architecture: arm64; minimum macOS 27.0
- SHA-256: `82753d5760aac47a17eb404e31b88be2f43d69507cd701555189d918f24e5801`
- Size: 53,141,658 bytes
- Signing: local ad-hoc; no Developer ID notarization
- Validation: migration gates passed; Release build succeeded; `codesign --verify --deep --strict` passed on the App mounted from the DMG; `hdiutil verify` passed; mounted app version/build were 3.12/312; app, `switchd-rs`, and `switchd` are arm64; archive SHA-256 matches the published DMG.
- Hardware/carrier behavior was not tested as part of packaging.
