# Dependency security review

Reviewed on 2026-10-25. Dependabot update PRs and RustSec reports require human
triage; neither service merges fixes or certifies runtime safety.

## Audit behavior and permissions

The workflow pins
[`rustsec/audit-check` at `69366f33c96575abad1ee0dba8212993eecbe998`](https://github.com/rustsec/audit-check/tree/69366f33c96575abad1ee0dba8212993eecbe998).
Its [reporter implementation](https://github.com/rustsec/audit-check/blob/69366f33c96575abad1ee0dba8212993eecbe998/src/reporter.ts)
distinguishes vulnerability advisories from informational notices, unmaintained,
unsound and other warnings. Its internal `critical` counter means a vulnerability
without an informational classification; it is not a CVSS Critical-only filter.

| Event | Token permissions | Finding behavior |
|---|---|---|
| `pull_request` | `contents: read` | Check API publication is denied; the action uses its `GITHUB_HEAD_REF` fallback to print the report in the job log. Vulnerabilities fail the job; informational warnings do not. |
| `workflow_dispatch` | `contents: read`, `checks: write` | Publishes a separate Security audit check when findings exist. Vulnerabilities fail; informational warnings alone succeed. Does not open issues. |
| `schedule` | `contents: read`, `issues: write` | Opens issues for supported advisory findings. Reporting findings alone does not fail the job. Tool, JSON, network or API failures can still fail it. |

Scheduled reporting searches issue/PR **titles** for each advisory ID, including
closed items, before creating an issue. It does not reopen or update an existing
report. Yanked crates without an advisory are logged without an issue. The two
lockfiles run sequentially in the scheduled job to reduce duplicate-report races;
GitHub search indexing can still lag. Review existing issues and the run log even
when the scheduled run succeeds. PR/manual audits use an independent job per
lockfile so one failure does not cancel the other.

All audit jobs run on GitHub-hosted Ubuntu. Checkout credentials are not
persisted. No job has `contents: write`, no audit ignores are configured, and
there is no automatic source rewrite, approval or merge. The action SHA is fixed;
the advisory database and the action's installed `cargo-audit` version are live
inputs and can change between runs.

## Open replacement backlog: unsafe-libyaml

The main lockfile retains `unsafe-libyaml` 0.2.11 through
`serde_yaml_ng` (exposed as the existing `serde_yaml` workspace alias).
[RUSTSEC-2024-0449](https://rustsec.org/advisories/RUSTSEC-2024-0449.html)
classifies `unsafe-libyaml` as unmaintained and lists no patched version. Moving
from deprecated `serde_yaml` to `serde_yaml_ng` does not remove this transitive
dependency or resolve that advisory. An informational audit success does not
close this backlog; a routine version bump cannot supply a nonexistent patch.

Backlog owner: maintainers. Review at the next dependency-security triage and
before renewing the annual automation window on 2027-10-25:

1. Evaluate maintained safe-Rust YAML alternatives, including the advisory's
   `saphyr`/`serde-saphyr` and `noyalib` candidates. They are candidates for review,
   not approved drop-in replacements.
2. Compare existing carrier YAML parsing, serialization, scalar/tag/alias
   semantics, diagnostics and malformed-input handling before selecting one.
3. Migrate in a separate reviewed PR with compatibility fixtures, then verify
   the lockfile graph no longer contains `unsafe-libyaml` and rerun RustSec and
   applicable software gates. Hardware/carrier acceptance remains separate.

Do not add a broad advisory ignore or claim this item fixed merely because an
audit remains green. Record any future time-limited exception, rationale, owner
and review date explicitly.
