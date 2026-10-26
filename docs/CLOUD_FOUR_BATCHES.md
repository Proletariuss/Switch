# GitHub-only four-batch execution

Approved 2026-10-26. Baseline: dedaf50a94e1475883db80f2381a1b42ee15846b.
Tree: eab6f5d08ac2bd5224b5ab37b0fdde42ac8182f4.

Source changes use GitHub APIs. Tests, fixes applied by jobs, compilation and
artifact generation run on GitHub-hosted runners. The maintainer's Mac is only
a read-only source/API control plane; no local repair, build or test commands.

## Frozen completion criteria

1. Shared cloud gates: both locked graphs checked on Linux; complete Rust
   and offline Swift gates; workflow upload smoke; exact-SHA App artifact.
2. Runtime: original module binding survives reboot admission; installing
   N-standby retry cannot be consumed; ePDG aliases preserve receipt semantics.
3. SMS: archive-only recovery never resends; bounded durable authority,
   v1 migration, safe terminal retirement and durable Swift issuance.
4. Three review passes, final-source software gates, App identity and blank
   forwarding defaults, immutable checksums, GitHub links and acceptance scope.

Each batch needs final-source evidence. v3.51 tag/release remain immutable.
This delivery is an ad-hoc CI App snapshot, not product-final or a new DMG.

## Dependency decision

PR #3 changes a linked main manifest without its lockfile; #5 requires an
API migration; sha1 0.11 removes the existing IKE compress feature. Retain the
compiled hash family for this stability delivery and hold incompatible version
updates for coordinated migration. Review by 2026-11-25. RustSec and security
alerts stay enabled without advisory ignores; new alerts require fresh triage.

## Acceptance boundary

Artifacts bind commit, tree/source digest, run/attempt, actual toolchain and
archive SHA. Historical local receipts cannot become a cloud PASS.
Offline tests do not prove physical card/carrier/provider, real Keychain,
notification/audio, macOS 27 runtime or continuous 24/72-hour operation.
No Developer ID/notary credentials are configured. Current runners are macos-26.

## Status

All planned code changes are integrated in PR #6; final-source software and
artifact acceptance remain pending. Three static review rounds completed:
initial implementation review; cross-component review; independent review of
the fixes and integrated boundaries. Findings corrected include exact v2
payload/nonce matching, shared Store ownership, startup SMS isolation,
nonblocking journal locking and directory identity, full v1+v2 pagination,
report selection across attempts, and valid retry fixture timestamps.

The isolated Swift candidate 6b06ee8be8f1cffcb2b4c3c012f0d9adad815bff passed
29 groups in GitHub run 40275765127. The first integrated Rust run
40273933310 passed 3684 daemon tests but failed two fixtures and did not reach
all isolated groups; those two fixtures are corrected in the integration.
Neither preliminary run is final acceptance. The final run must test and
build one exact source commit and successfully verify the uploaded App.

Final evidence and links will be recorded in PR #6 to avoid changing the
already tested source commit just to add its own run URL. This plan and its
static review do not award functional, hardware or product acceptance.
