# Switch implementation scope

Work only within the current authorized workspace
`/Users/StatTrak/Desktop/Switch_MAC/`; the user later directed continuation
in Switch_MAC. The editable application source is `src`, with generated
build/test output in the sibling `build` directory. Older
`/Users/proletarius/Desktop/Switch/` paths describe the historical seed
workspace, not the current edit target. Do not access or modify the original
Proletarius, Boo or 9.29 reference folders outside this workspace.

Follow `../docs/Switch改造方案.md` V3 and its execution update. Remove fax,
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

Include overall project progress in each progress report and final reply.
Show the current P0–P6 stage, the newly verified work and the remaining
acceptance requirements. A percentage needs a frozen, auditable denominator;
label interface coverage separately from overall product completion. Do not
reuse the inconsistent historical person-week ledger as a current percentage.
