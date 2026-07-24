# Contributing

Thank you for improving dash9. Discuss substantial behaviour in an issue and update or add a specification before implementation — `SPEC.md` for Phase 1 capabilities, a new file under `docs/specs/` for later phases. Record durable architecture decisions in an ADR under `docs/adr/`. Use conventional commits (`type(scope): summary`), such as `feat(prom): add label metadata queries`.

Fork the repository, create a focused branch, run `just ci`, and submit a pull request using the template. New dependencies need a documented role, license review, maintenance assessment, and minimal feature set — `dash9-core` in particular must never gain a UI, network, or async-runtime dependency (see `AGENTS.md`). Defect fixes require a regression test. The command grammar and dashboard TOML schema are public, versioned formats: extending them means adding a new verb, subverb, or field, never changing an existing one's meaning (`SPEC.md` Section B.1).

Keep tests deterministic and offline. Prefer a fixture or a throwaway local TCP listener the test itself owns over a live network dependency — see `crates/dash9-prom/src/lib.rs`'s `live_tests` module and `crates/dash9/tests/cli.rs` for the established pattern.

By participating, you agree to the [Code of Conduct](CODE_OF_CONDUCT.md). Security reports belong in the private process described by [SECURITY.md](SECURITY.md), not public issues.
