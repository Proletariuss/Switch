# Iteration 515: ESTKme-RED + SIM World hardware checkpoint

## Scope and isolation

- On 2026-10-18, the selected PC/SC reader was `ESTKme-RED`. A separate Rust `switchd-rs` instance used an isolated data directory under `build/runtime/iteration-515-estk3hk`; the installed Switch application and its data were not modified.
- The eUICC inventory returned ten profiles in one group. SIM World was active. No 3HK profile was present, so this checkpoint is **not** 3HK acceptance. No profile switch, download, SMS send, or call was attempted.
- Hardware identifiers, IMSI, AKA material, EID, ICCID, internal addresses, and SIP contents are omitted here.

## Real-device observations

1. Rust connected to the PC/SC reader, selected the SIM World subscription, and read the EID and ten-profile catalog.
2. The first VoWiFi attempt reached encrypted IKE_AUTH but failed because the ePDG sent RFC 4187 EAP-AKA Identity (subtype 5), which the runtime did not handle. A bounded identity response and exact AKA-Identity checkcode transcript were added. With this change, the real card advanced through EAP-AKA.
3. The next attempt reached the authenticated final IKE_AUTH response. The ePDG supplied a zero IPv4 DNS placeholder alongside a valid assigned address and P-CSCF entries. The parser now ignores an unspecified address **only** for optional IPv4 DNS/P-CSCF attributes, while still rejecting it as an assigned internal address. The real card then reached `tunnel_state=connected` and `ims_state=registering`.
4. IMS plain TCP connect timed out. A temporary local packet-count trace found eight outbound inner packets successfully sealed and submitted to UDP 4500 as ESP, with no inbound ESP or inner packets during those attempts. The trace was removed from source after diagnosis. This does not prove whether the cause is the network/proxy path, ePDG policy, packet formatting, or a carrier endpoint. IMS registration, SMS receipt, and calls remain **unverified**.

## Code and verification

- `proletarius-vowifi` now handles bounded AKA-Identity requests and the zero optional server placeholder; focused parser tests and a real ePDG handshake exercised both changes.
- Startup diagnostics expose static IKE/EAP/configuration failure classes and bounded IMS I/O stage/kind/numeric OS error without serializing subscriber identity, keys, SIP payloads, or network endpoints.
- Rust migration contract inventory still reports 294/294 Rust HTTP owners and zero Go owners. This is an interface ownership check, not proof of carrier interoperability.
- `cargo +1.98.0 fmt --all -- --check` passed. With `scripts/workspace-env.sh` loaded, `cargo +1.98.0 test --locked --workspace` passed: 3,023 tests passed, zero failed, one ignored (`build/reports/iteration-515-rust-workspace-tests-env.log`). The first ad-hoc invocation without that environment failed because tests require `SWITCH_BUILD_DIR` and `SWITCH_DATA_DIR`; it is not a source regression.
- `cargo +1.98.0 build --locked -p proletariusd` passed and refreshed the development Rust backend binary under `build/rust-target/debug/`. No DMG was produced and the installed application remains the prior release.
- `check-rust-migration-contracts.py --require-rust-only` passed after manual fingerprint review, and `check-rust-backend-exit.py --require-go-free` passed. These structural gates do not certify carrier interoperability.

## Remaining acceptance

Investigate why no ESP reply reached the selected UDP 4500 channel after successful IKE/CHILD_SA establishment. Repeat registration after a justified network-path or implementation correction, then verify incoming SMS and calls only when IMS is actually registered. Do not classify tunnel installation alone as VoWiFi service readiness.
