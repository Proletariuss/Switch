# GitHub automation policy

Switch uses GitHub-hosted runners for automated review. The current annual
iteration window runs from 2026-10-25 through 2027-10-25. No workflow depends on
the maintainer's Mac, a self-hosted runner, attached hardware, or locally saved
credentials.

## Free-plan budget

GitHub Free currently includes 2,000 Actions minutes per month, 500 MB of
Actions artifact storage, and 10 GB of cache storage per repository. macOS
runners consume the allowance substantially faster than Linux runners, so the
workflows use proportional gates:

- every pull request receives a small Ubuntu source-contract check;
- production-code changes receive the macOS Rust software gate;
- Swift, resources, Xcode project, and packaging changes additionally build
  `Switch.app` on the same macOS runner;
- the macOS software gate also runs once per month to detect runner or toolchain
  drift;
- release builds remain manual or tag-driven;
- pull-request checks do not upload an app or generate a DMG;
- release artifacts produced by Actions are retained for seven days.

Current limits and prices may change. Review GitHub's official billing page
before changing runner frequency:
<https://docs.github.com/en/billing/concepts/product-billing/github-actions>.

## Automatic iteration

Dependabot checks the main Rust workspace, the independent daemon-capsule
acceptance fixture, and GitHub Actions weekly. It opens no more than two main
Cargo update pull requests, one fixture update pull request, and one Actions
update pull request at a time. Updates wait seven days before selection to reduce
newly released version risk. Every update remains a reviewable pull request and
is never auto-merged.

The weekly RustSec workflow checks both `rust/Cargo.lock` and
`scripts/acceptance/daemon-capsule-fixture/Cargo.lock` against the advisory
database. Pull requests that modify either Cargo workspace receive the same
audit. Findings fail the check; scheduled findings may create a GitHub issue.
No repository or forwarding secret is supplied to the audit.

Clippy runs on code-affecting pull requests and reports its findings. The
existing daemon warning baseline is intentionally not converted into a global
`-D warnings` gate in one step. New warning classes should be retired in small,
reviewable pull requests until the baseline can be made blocking.

Without a paid Copilot plan, GitHub does not perform semantic AI rewrites. The
free automation therefore limits automatic source changes to GitHub-native
Dependabot pull requests. It does not grant a workflow permission to rewrite
`main`, approve a pull request, or merge a change.

At the end of the annual window, review Actions usage, unresolved findings and
the remaining acceptance boundary before renewing the schedule. The end date is
an operational review point, not an assertion that the product will be complete.

## Workflow map

| Workflow | Trigger | Runner | Writes source? |
|---|---|---|---|
| `Review Switch changes` | PR, manual, monthly | Ubuntu and conditional macOS | No |
| `Audit Rust dependencies` | Cargo PR, manual, weekly | Ubuntu | No; scheduled findings may open an issue |
| `Build Switch` | version tag or manual | macOS | No; uploads a seven-day App artifact |
| Dependabot | weekly | GitHub service | Opens dependency PRs only |

## Merge rules

The current private Free-plan repository cannot enforce branch protection or
required checks. Maintainers must enforce the following process manually:

1. Make changes on a branch and open a pull request.
2. Wait for the latest commit's applicable checks to finish successfully.
3. Review the exact diff, especially dependency, certificate, AKA, RF/USB,
   storage, and network-state changes.
4. Never treat software-only checks as proof of physical device, carrier
   registration, SMS delivery, call audio, or long-running behavior.
5. Merge manually. Do not enable automatic merge.

Action dependencies are pinned to reviewed full commit SHAs, checkout does not
persist credentials, and ordinary jobs receive only read access to repository
contents. The RustSec job receives only the additional checks and issues access
needed to publish its result.
