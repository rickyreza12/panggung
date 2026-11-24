# Panggung Application Rules

Inherit ../AGENTS.md. This directory contains the real Rust platform.

- Use rust-best-practices and clean-code when available; follow equivalent rules otherwise.
- Use Rust, Axum, Tokio and SQLite with SQLx. Start with a modular monolith.
- Follow official library documentation and Backstage specifications for documented interoperability only.
- Do not claim Backstage plugin compatibility. Add providers and extension contracts only when tested.
- Keep default deployment local-only until authentication and authorization are verified.
- Validate all external input. Use parameterized queries and additive migrations.
- Preserve user data, secrets and applied migration history.
- Use frontend design skills for visible changes when available; preserve accessible controls and honest loading/error states.
- Keep compiled frontend assets embedded when possible; external infrastructure optional.
- Run formatting, Clippy, tests, build and smoke checks. Report exact failures and unverified capabilities.