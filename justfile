set shell := ["bash", "-eu", "-o", "pipefail", "-c"]

check:
    cargo fmt --all --check
    cargo check --workspace --all-targets --all-features
    cargo clippy --workspace --all-targets --all-features -- -D warnings
    ./scripts/check-architecture.sh

test:
    cargo test --workspace --all-targets --all-features

ci: check test

deny:
    cargo deny check

demo:
    cargo run -p dash9 -- demo

up:
    docker compose up -d

down:
    docker compose down
