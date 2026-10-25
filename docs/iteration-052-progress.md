# Iteration 052 — Additive NekokoLPA2 / 9eSIM compatibility

Date: 2026-10-01  
Release: Switch 2.66 (build 266)

## Changes

- Extended the existing eSIM download flow with NekokoLPA2-informed transport compatibility. External PC/SC readers use 240-byte STORE DATA chunks, while the existing modem AT/QMI chunk sizing and standard download route remain in place.
- For identified 9eSIM cards, the default eSTK applet probe now checks SE1 before SE0. Explicit user-selected applets continue to take precedence.
- Separated the eUICC `AuthenticateServer` APDU from SM-DP+ `AuthenticateClient` HTTPS authentication so the reported download stage identifies which side returned an error. A card-side `6982` is now reported as eUICC authentication.
- Added parsing for profile metadata tag `9F21` (`EstimatedProfileSizeIndication`) and a licensed reference-size fallback. The app displays the estimate and waits for confirmation before `PrepareDownload` or writing the profile to the card; cancel sends `CancelSession` and exits without starting the write.
- Preserved the standard flow when the server supplies no estimate or a card does not match the 9eSIM compatibility path. The fallback reference dataset and its MIT attribution are recorded in `THIRD_PARTY_NOTICES.md`.

## Validation and delivery

- `go test ./...` passed in `backend`; the focused `euicc-go` LPA and v2 package tests passed.
- The complete macOS 27 arm64 Release build succeeded. Strict nested app signature verification passed.
- `hdiutil verify` passed for the final DMG. The `dist/` DMG and archive copy have matching SHA-256 `294305b3d0de569f2e907039cbabe1507f5179ca76e0dca9f7430cbeca4f60b5` (43,167,420 bytes).
- Final artifact: `../dist/Switch-2.66-macOS27.dmg`. Existing `../dist/Switch-2.64-macOS27.dmg` was preserved.
- Signature is local ad-hoc; no notarization profile was configured.
- When this report was first delivered, the earlier physical-card attempts had returned `6982` at eUICC `AuthenticateServer`, and no post-change hardware retest had yet occurred.

## User-reported hardware result and disposition

- On 2026-10-01, the user retested Switch 2.66 with the physical reader and reported that profile download still fails with `6982`; NekokoLPA2 downloads the same profile successfully.
- The user explicitly asked to record and defer this issue. No further eSIM download troubleshooting, code changes, or test-code attempts are authorized in this iteration. Reopen only when the user asks to resume.
