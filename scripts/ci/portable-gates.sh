#!/bin/bash
set -euo pipefail
scripts/preflight-source.sh
for manifest in rust/Cargo.toml scripts/acceptance/daemon-capsule-fixture/Cargo.toml; do
  cargo +1.98.0 fmt --manifest-path "$manifest" --all -- --check
  # Fetch resolves the entire lockfile, unlike metadata --no-deps.
  cargo +1.98.0 fetch --locked --manifest-path "$manifest"
  cargo +1.98.0 metadata --locked --no-deps --manifest-path "$manifest" --format-version 1 >/dev/null
done
go install github.com/rhysd/actionlint/cmd/actionlint@v1.7.7
"$(go env GOPATH)/bin/actionlint" -color
# Generated Xcode projects are not edited by this portable gate.
git diff --exit-code
