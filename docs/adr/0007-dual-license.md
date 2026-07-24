# ADR 0007: Apache-2.0 OR MIT dual license

- Status: Accepted
- Date: 2026-07-19

## Context
Rust projects commonly offer a permissive dual license compatible with broad commercial and open-source use.

## Decision
License contributions and distributions under Apache License 2.0 or MIT, at the recipient's option. Package metadata uses `Apache-2.0 OR MIT` (`workspace.package.license` in the root `Cargo.toml`, inherited by every crate). Contributions are made under the same terms unless explicitly stated.

## Consequences
Users choose either license. Maintainers must keep both license texts (`LICENSE-APACHE`, `LICENSE-MIT`) current and review dependency license compatibility. A contributor agreement is not required today.
