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


# dash9-tui MUST never depend on dash9-prom or reach for network/async
# directly — concrete adapters and cross-crate wiring stay in the
# dash9 binary (docs/architecture/rendering.md, Mechanism 6).
tui_manifest="crates/dash9-tui/Cargo.toml"
for forbidden in dash9-prom reqwest tokio; do
  if grep -Eq "^${forbidden}[[:space:]]*=" "$tui_manifest"; then
    echo "forbidden dash9-tui dependency: ${forbidden}" >&2
    exit 1
  fi
done


# dash9-assist has exactly one effector (emitting command-grammar
# text) and no network access beyond its own LLM endpoint — it must
# never depend on dash9-tui or dash9-prom (docs/specs/assist.md
# Section B).
assist_manifest="crates/dash9-assist/Cargo.toml"
if [ -f "$assist_manifest" ]; then
  for forbidden in dash9-tui dash9-prom ratatui crossterm; do
    if grep -Eq "^${forbidden}[[:space:]]*=" "$assist_manifest"; then
      echo "forbidden dash9-assist dependency: ${forbidden}" >&2
      exit 1
    fi
  done
fi

echo "architecture checks passed"
