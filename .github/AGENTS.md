# GitHub governance scope

Keep pull_request jobs safe for forks and Dependabot: read-only GITHUB_TOKEN,
SHA-pinned actions and checkout with persist-credentials: false. Never execute
PR source in a privileged pull_request_target workflow. Do not put credentials,
user data, local runtime logs or production configuration into commits/artifacts.

Quality uses the current shared software-gates workflow. Draft blocks the final
PR result explicitly; ready_for_review runs all selected gates and
converted_to_draft revokes the ready result. RustSec and policy fixtures run on
every PR targeting main, without top-level path filters. Documentation-only
changes keep portable checks; workflow/build/runtime changes retain their
appropriate native and App gates.

Preserve these exact PR check names:
- Review result
- RustSec PR (rust)
- RustSec PR (scripts/acceptance/daemon-capsule-fixture)
- Review policy regression fixtures

Non-PR runs use distinct names. Review policy fixtures execute the actual
production evaluator and classifier on GitHub-hosted runners. Never turn a
failure, cancellation or unexpectedly skipped required gate into success.

Release identity and the privileged publish job must require refs/heads/main.
The publisher independently verifies the workflow run's main branch and exact
source identity before any write. Keep immutable tags/assets, complete native
gates, original reports and upload/readback checks. Retired reviewed preparation
must not become reachable again without explicit authorization.

Public PR templates, policy documents and release notes are German. CODEOWNERS
identifies the maintainer; it does not imply an independent approval or automatic
merge. No Codex login or automatic review setup is part of this repository policy.
