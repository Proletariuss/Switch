# Iteration 051 — SMS inbox and N-wait presentation

Date: 2026-09-30  
Release: Switch 2.64 (build 264)

## Changes

- Renamed the sidebar section and navigation command from “1卡N待” to “N待”.
- Limited the N-wait page to eSIM line status and per-line or all-line start/stop controls. It now sorts ready lines first, uses a responsive card grid and compact status symbols, and links to the SMS page with the selected profile retained.
- Added an N-wait SMS source to the SMS page. It provides per-profile conversation history, message composition, send status, deletion and notification preference. N-wait background storage and system notifications remain active; the N-wait page itself no longer renders a message inbox or send controls.
- Tapping an N-wait SMS notification opens the SMS page and selects the related profile and conversation.
- Replaced the toolbar privacy glyph with a filled, two-eye Tampermonkey-style vector mark. Gray means privacy masking is off; blue means it is on. It uses the same transparent toolbar hit target as the other project actions.
- Increased the app version from 2.63 (263) to 2.64 (264).

## Validation and delivery

- `scripts/build.sh` completed successfully for Release, arm64, macOS 27; strict nested code-signature verification passed.
- No test suite was added or run for this UI iteration.
- `scripts/release.sh` completed; `hdiutil verify` reports a valid checksum. The embedded Switch.app passed strict signature verification and reports version 2.64/build 264.
- DMG: `../dist/Switch-2.64-macOS27.dmg` (43,083,946 bytes; SHA-256 `38def33272820e876a15f088922d8f0febf4665d3d96b6f117484979f8e8d8cc`). The archive copy under `../dist/archive/` has the same checksum.
- Signature: local ad-hoc; no notarization profile was configured.
