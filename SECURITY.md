# Security policy

dash9 is pre-release; only the latest `main` branch receives security fixes. Do not open a public issue for a suspected vulnerability. Use GitHub's private vulnerability reporting for this repository. Include impact, affected revision, reproduction steps using non-sensitive fixtures (a dashboard TOML and datasource responses, never real credentials or production metric data), and a suggested mitigation if available. Maintainers should acknowledge a report within seven days.

Never include real credentials, API keys, or private datasource content in a report or a fixture. dash9 performs no telemetry; the only network access it ever makes is to datasources and, when the optional `assist` feature is enabled, the configured LLM endpoint — both explicitly configured by the user, never auto-discovered. See `docs/specs/assist.md` for the assistant's specific network and effector boundaries.
