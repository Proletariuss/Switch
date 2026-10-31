#!/bin/bash
set -euo pipefail
scripts/preflight-source.sh
PYTHONDONTWRITEBYTECODE=1 python3 scripts/ci/test-publish-release.py
PYTHONDONTWRITEBYTECODE=1 python3 scripts/ci/test-package-dmg.py
PYTHONDONTWRITEBYTECODE=1 python3 scripts/ci/test-compact-release.py
for manifest in rust/Cargo.toml scripts/acceptance/daemon-capsule-fixture/Cargo.toml; do
  cargo +1.98.0 fmt --manifest-path "$manifest" --all -- --check
  # Fetch resolves the entire lockfile, unlike metadata --no-deps.
  cargo +1.98.0 fetch --locked --manifest-path "$manifest"
  cargo +1.98.0 metadata --locked --no-deps --manifest-path "$manifest" --format-version 1 >/dev/null
done
go install github.com/rhysd/actionlint/cmd/actionlint@v1.7.12
"$(go env GOPATH)/bin/actionlint" -color
if [[ "${GITHUB_EVENT_NAME:-}" == pull_request ]]; then
  git diff --check "$BASE_SHA" "$HEAD_SHA"
fi
# Generated Xcode projects are not edited by this portable gate.
git diff --exit-code
