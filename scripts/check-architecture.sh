#!/usr/bin/env bash
set -euo pipefail

# dash9-core MUST have no tokio, no UI, no network dependencies (SPEC.md, PR 1).
core_manifest="crates/dash9-core/Cargo.toml"
for forbidden in tokio reqwest ratatui crossterm; do
  if grep -Eq "^${forbidden}[[:space:]]*=" "$core_manifest"; then
    echo "forbidden dash9-core dependency: ${forbidden}" >&2
    exit 1
  fi
done

echo "architecture checks passed"
