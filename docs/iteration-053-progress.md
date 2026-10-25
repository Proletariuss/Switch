# Iteration 053 — Compact N-wait workspace and source cleanup

Date: 2026-10-01  
Release candidate: Switch 2.67 (build 267)

## Changes

- Replaced the vertically scrolling N-wait page with a compact, responsive profile grid. Cards are smaller; column and row counts adapt to the available window size, and page controls expose additional profiles without vertical scrolling.
- Moved the compact SMS action immediately before Refresh in the N-wait controls.
- Added a concise safety banner advising users to preserve eSIM activation/recovery material because loss of a profile or physical-card chip damage can make a number unrecoverable.
- Refined the page's Möbius mark as a small vector ribbon with a soft gradient treatment.
- Made line status distinguish offline, IMS lease renewal, and automatic recovery. Existing service behavior renews IMS registration before the network-granted lease ends and rebuilds failed sessions with bounded retry delay; reauthentication selects the corresponding eSIM profile as needed. Carrier subscription-plan expiry is not provided by current card/modem data, so the UI does not claim to detect an account's plan expiry.
- Recorded the user's Switch 2.66 hardware result and explicit defer decision in `iteration-052-progress.md`; the 6982 issue remains deferred, with no additional investigation or code changes in this iteration.
- Removed the Switch-local reference copies of `Proletarius`, `Boo`, and `9.29`, the temporary `NekokoLPA2` clone under `src/build/tmp`, and the stray `src/build` folder (its only report exactly matches the retained workspace report). The original folders outside Switch remain unchanged. Recoverable copies are in `/Users/proletarius/.Trash/Switch-reference-cleanup-20261001.jbelBA`.
- Bumped the source version to 2.67/build 267.

## Validation and delivery

- `scripts/build.sh` completed for macOS 27 arm64 Release; the app passed `codesign --verify --deep --strict` and reports version 2.67/build 267.
- App artifact: `../dist/Switch.app`. Existing 2.66 DMGs remain untouched; no new DMG was generated.
- No DMG is generated for this UI iteration.
