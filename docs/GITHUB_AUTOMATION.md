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

The operating ceiling is planned against the private GitHub Free allowance,
not against unlimited retries. At the currently listed baseline rates, a
30-minute standard macOS gate costs about the same as 310 baseline Linux
minutes. One monthly drift run plus at most four full code/dependency PR gates
therefore keeps roughly 450 of the 2,000 included minute-equivalents available
for one manual release attempt or unexpected rerun. Superseded runs on the same
PR are cancelled. If account-wide usage reaches about three quarters of its
monthly allowance, defer non-security maintenance PRs until the next billing
window; security fixes may use the reserve after explicit review. This is an
operational target, not a GitHub-enforced spending cap, and other private
repositories share the same account allowance.

## Automatic iteration

Dependabot checks the main Rust workspace, the independent daemon-capsule
acceptance fixture, and GitHub Actions weekly. It opens no more than two main
Cargo **version-update** pull requests, one fixture version-update pull request,
and one Actions version-update pull request at a time. The seven-day cooldown
also applies only to version updates: eligible releases are selected on the next
weekly run after their cooldown. Security-update PRs bypass that cooldown and
use GitHub's separate, non-configurable limit of ten open security-update PRs;
the configured limits do not cap the combined queue at four. Security updates
also require the corresponding repository feature to be enabled. Every update
remains a reviewable pull request and is never auto-merged. See the
[Dependabot option reference](https://docs.github.com/en/code-security/reference/supply-chain-security/dependabot-options-reference#open-pull-requests-limit).

The weekly RustSec workflow checks both `rust/Cargo.lock` and
`scripts/acceptance/daemon-capsule-fixture/Cargo.lock` against the advisory
database. Pull requests that modify either Cargo workspace receive the same
audit. PR/manual runs fail on vulnerability advisories; informational warnings
(including unmaintained dependencies) are reported without failing the audit.
Scheduled runs create issues for newly reported advisory IDs, including supported
informational warnings. Successful issue reporting can leave a scheduled run
green despite findings. A green run is therefore not evidence of zero findings.
Only the scoped `GITHUB_TOKEN` is supplied; no personal, repository-custom, or
forwarding credentials are supplied. See [dependency security](DEPENDENCY_SECURITY.md)
for the pinned action's reporting details and current replacement backlog.

Clippy runs on code-affecting pull requests and reports its findings. The
existing daemon warning baseline is intentionally not converted into a global
`-D warnings` gate in one step. New warning classes should be retired in small,
reviewable pull requests until the baseline can be made blocking.

Without a paid Copilot plan, GitHub does not perform semantic AI rewrites. The
free automation therefore limits automatic source changes to GitHub-native
Dependabot pull requests. It does not grant a workflow permission to rewrite
`main`, approve a pull request, or merge a change.

2027-10-25 is the annual review and stop point. By that date, maintainers must
review Actions usage, unresolved findings and the remaining acceptance boundary,
then explicitly renew the policy or disable scheduled workflow triggers and
Dependabot version updates. These YAML schedules do not expire automatically;
this document alone does not stop GitHub from running them. The date does not
assert that the product will be complete.

`macos-26` with Xcode 26.6 can compile the current project while its deployment
target is set to macOS 27. That target value does not install a macOS 27 SDK or
provide a macOS 27 runtime. Hosted build success covers compilation with the
runner's actual SDK; macOS 27 SDK/API compatibility and runtime acceptance still
require the corresponding toolchain and OS, plus separate device/carrier tests.

## Workflow map

| Workflow | Trigger | Runner | Writes source? |
|---|---|---|---|
| `Review Switch changes` | PR, manual, monthly | Ubuntu and conditional macOS | No |
| `Audit Rust dependencies` | Cargo/audit-workflow PR, manual, weekly | Ubuntu | No; scheduled findings may open an issue |
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
contents. RustSec PR jobs also remain read-only and report findings in the job
log through the pinned action's Check-API-denied fallback. Manual audit jobs add
only `checks: write`; scheduled audit jobs add only `issues: write`. No workflow
receives `contents: write` or runs PR code via `pull_request_target`.
