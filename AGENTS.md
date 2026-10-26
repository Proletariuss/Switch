# Switch implementation scope

The current source of truth is the private GitHub repository
`Proletariuss/Switch`. The user authorized cloud-only maintenance:
source changes are made through GitHub, and all formatting, tests, repair jobs,
compilation and artifact generation run on GitHub-hosted runners. The local Mac
is only a read-only source and GitHub API control plane; do not run the project,
repair scripts, tests or builds locally, or schedule local maintenance tasks.

The former `Switch_MAC/src` layout is historical context, not an instruction
to resume local work. Preserve the original Proletarius, Boo and 9.29 reference
folders; do not access or modify them.

Follow the approved cloud scope in `docs/CLOUD_FOUR_BATCHES.md` and the
repository's retained migration specifications. Remove fax,
preserve all other existing behavior, and add independent multi-profile
VoWiFi SMS with a Möbius sidebar icon. New sessions are SMS-only; existing
voice functionality remains. Do not run real card mutations or send SMS
without a clearly selected test card and destination from the user.

Do not generate any DMG unless the user explicitly requests it. Build the
application and test binaries only. `SWITCH_DATA_DIR` should point inside
`../build/runtime` during development. Do not run broad pkill commands or
read/migrate the original application's user data.

## Cache hygiene

After each completed build/test batch, promptly remove disposable compiler
caches when no remaining validation needs them. Prefer the exact candidates
from `scripts/clean-generated.rb --list`, with explicit `--remove` paths.
Keep builds paused during cleanup and check that no process holds files in
the selected paths. Retain sources, offline dependencies, EVS resources,
test evidence, rollback baselines and current release artifacts. Never treat
an entire `build`, `tmp`, `runtime`, `optimization` or worktree directory as
disposable just because of its name. Record the paths and reclaimed size.
When a validation process already sets `CFFIXED_USER_HOME`, pass the existing
installed `RUSTUP_HOME` explicitly to Xcode and its child build scripts. Xcode
can change `HOME`; do not download a duplicate toolchain into the test home.

## Progress reporting

Report progress against the frozen four-batch criteria in
`docs/CLOUD_FOUR_BATCHES.md`. A percentage needs an auditable denominator;
keep source/interface coverage separate from software and product acceptance.
Do not reuse the historical P0–P6/person-week ledger as a current percentage.
Device, carrier, macOS 27 runtime and long-run acceptance remain unverified
until supported by new evidence. Only successful final-source cloud jobs
can close the corresponding software acceptance item.
