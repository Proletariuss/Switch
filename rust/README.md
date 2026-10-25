# Switch Rust backend

The production macOS app starts `switchd-rs`. The source tree contains no Go
backend, and the app bundle contains no Go executable. Rust owns the public
HTTP API, PC/SC and BLE reader services, eSIM operations, module services,
VoWiFi/IMS, SMS routing, and call control. The Swift app remains the UI and
launch supervisor. Historical internal error type names such as
`GoBackendUnavailable` do not start a Go process.

All `../build` and `../dist` paths below are relative to the source root (the
parent of this `rust` directory).

Build from the source root:

```sh
scripts/build.sh
```

This builds and signs `../dist/Switch.app`. It does not make a DMG. Generated
Cargo and Xcode state is kept in sibling `../build/`, not in `rust/target` or
the original Proletarius checkout. Do not set a different `CARGO_TARGET_DIR`
for project checks unless the isolated output will also be removed afterward.

Run the repository gates from the source root:

```sh
scripts/test-migration-gates.sh
python3 scripts/check-rust-backend-exit.py --require-go-free
python3 scripts/check-rust-migration-contracts.py --require-rust-only
```

The gates include Rust workspace tests, Swift BLE regressions, frozen protocol
fixtures, route ownership, and a negative check against reintroducing a Go
build, bundled child, launcher, or request forwarding. A passing gate does not
prove a physical card, operator registration, calls, SMS, or upgrade/rollback
behavior. The current evidence and remaining P6 boundaries are recorded in
[`../docs/optimization/all-rust/README.md`](../docs/optimization/all-rust/README.md).

For VoWiFi tests, preserve the configured front proxy and its DNS path. The
verified SIM World tests used Surge's automatic enhanced-mode route and the
known ePDG; an explicitly configured SOCKS5 domain proxy is another supported
transport. Do not bypass the configured proxy by resolving ePDG DNS locally or
switching the test to direct transport.
When neither a proxy nor Surge is configured, the code also has a separate
direct transport branch; it was not used for this SIM World verification.

After all Cargo/Xcode processes have exited and the signed app plus reports
have been retained, `../build/rust-target` and `../build/DerivedData` are
regenerable caches. Keep `../build/cargo-home` for offline dependencies, and
preserve release baselines and reports needed for rollback evidence.

Clean completed build stages promptly with `scripts/clean-generated.sh --list`,
then `--remove` followed by the exact listed paths. The allowlist also covers
Swift/Python caches and compiler caches inside recognised clean-build audit
capsules. It preserves capsule sources, offline dependencies, signed products,
logs and audit records. Never clear `../build`, `../build/runtime` or
`../build/tmp` wholesale; these contain more than disposable compiler output.
