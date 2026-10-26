#!/bin/bash
# Shared by review and candidate builds; keep process-global tests isolated.
set -euo pipefail
export SWITCH_SOURCE_ROOT="$PWD"
export SWITCH_TEST_TMPDIR="$RUNNER_TEMP/switch-tests/"
umask 077
G8_FIXTURE_ROOT="$(mktemp -d "${RUNNER_TEMP%/}/switch-g8.XXXXXX")"
G8_FIXTURE_ROOT="$(cd "$G8_FIXTURE_ROOT" && pwd -P)"
export G8_FIXTURE_ROOT
trap 'rm -rf -- "$G8_FIXTURE_ROOT"' EXIT
. scripts/workspace-env.sh
scripts/preflight-source.sh
python3 scripts/build-evs.py
python3 scripts/build-module-supervisor.py
(
  cd rust
  cargo +1.98.0 fmt --all -- --check
  # The current daemon has a reviewed warning baseline. Clippy still
  # reports new findings, but warnings are not globally denied until
  # the baseline has been retired deliberately.
  cargo +1.98.0 clippy --locked --workspace --all-targets
  cargo +1.98.0 test --locked --workspace --exclude proletariusd
  # Run the full daemon suite. Tests with process-wide descriptor or
  # retained serial state are exercised below with explicit isolation.
  cargo +1.98.0 test --locked -p proletariusd -- \
    --skip native_serial_module_supervision_tests:: \
    --skip acceptance_capsule:: \
    --skip connection_pools::tests::production_three_pool_driver_reaps_64_admits_queued_65_and_preserves_other_pools
)
python3 - <<'PY'
import subprocess

base = [
    "cargo", "+1.98.0", "test", "--locked", "-p", "proletariusd",
    "--bin", "switchd-rs",
]
def list_tests(pattern):
    listed = subprocess.run(
        base + [pattern, "--", "--list"],
        cwd="rust",
        check=True,
        text=True,
        stdout=subprocess.PIPE,
    )
    return [
        line[:-len(": test")]
        for line in listed.stdout.splitlines()
        if line.endswith(": test")
    ]

serial_prefix = "native_serial_module_supervision_tests::"
serial_tests = [
    test for test in list_tests(serial_prefix)
    if test.startswith(serial_prefix)
]
if len(serial_tests) != 6 or len(set(serial_tests)) != 6:
    raise SystemExit(
        f"expected 6 serial supervision tests, found {len(serial_tests)}"
    )

capsule_prefix = "acceptance_capsule::"
capsule_tests = [
    test for test in list_tests(capsule_prefix)
    if test.startswith(capsule_prefix)
]
if not capsule_tests or len(capsule_tests) != len(set(capsule_tests)):
    raise SystemExit(
        f"expected unique capsule tests, found {len(capsule_tests)}"
    )

exact_tests = [
    "connection_pools::tests::production_three_pool_driver_reaps_64_admits_queued_65_and_preserves_other_pools",
]
for test in exact_tests:
    if list_tests(test) != [test]:
        raise SystemExit(f"expected one exact isolated test: {test}")

isolated = serial_tests + capsule_tests + exact_tests
if len(isolated) != len(set(isolated)):
    raise SystemExit("isolated test selectors overlap")

subprocess.run(
    base + [capsule_prefix, "--", "--test-threads=1"],
    cwd="rust",
    check=True,
)
for test in serial_tests + exact_tests:
    subprocess.run(
        base + [test, "--", "--exact", "--test-threads=1"],
        cwd="rust",
        check=True,
    )
PY
cargo +1.98.0 fmt \
  --manifest-path scripts/acceptance/daemon-capsule-fixture/Cargo.toml \
  --all \
  -- --check
cargo +1.98.0 clippy \
  --manifest-path scripts/acceptance/daemon-capsule-fixture/Cargo.toml \
  --locked \
  --all-targets
cargo +1.98.0 test \
  --manifest-path scripts/acceptance/daemon-capsule-fixture/Cargo.toml \
  --locked \
  -- --test-threads=1
