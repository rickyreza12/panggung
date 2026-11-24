# Panggung roadmap

All phases below are planned unless the verification ledger explicitly records completion. A release label is a target, not a published release.

| Phase | Target | Scope |
|---|---|---|
| 00 | v0.1.0 | Repository initialization |
| 01 | v0.1.0 | Rust application foundation |
| 02 | v0.1.0 | Infrastructure providers: SQLite and local storage |
| 03 | v0.1.0 | Software Catalog and documented Backstage import subset |
| 04 | v0.1.0 | Teams, ownership, dependencies, baseline access control |
| 05 | v0.1.0 | Developer portal frontend and embedded assets |
| 06 | v0.2.0 | Software Templates |
| 07 | v0.2.0 | Golden Path Scaffolder |
| 08 | v0.2.0 | Technical documentation |
| 09 | v0.2.0 | Search and interface explorer |
| 10 | v0.2.0 | Engineering Scorecards |
| 11 | v0.3.0 | GitHub integration |
| 12 | v0.3.0 | GitHub Actions and GitLab |
| 13 | v0.3.0 | Observability |
| 14 | v0.3.0 | Kubernetes and Harness |
| 15 | v0.3.0 | Developer command-line tooling |
| 16 | v0.4.0 | Versioned plugin contracts |
| 17 | v0.4.0 | Sandboxed and remote plugin runtime |
| 18 | v0.4.0 | Plugin management |
| 19 | v0.4.0 | Plugin developer ecosystem |
| 20 | v0.5.0 | Remote libSQL and PostgreSQL |
| 21 | v0.5.0 | External object storage |
| 22 | v0.5.0 | External search |
| 23 | v0.5.0 | Distributed cache, queue and workers |
| 24 | v0.5.0 | Enterprise identity and security |
| 25 | v0.5.0 | Horizontal scaling |
| 26 | v0.6.0 | Model Context Protocol server |
| 27 | v0.6.0 | AI Engineering Assistant |
| 28 | v0.6.0 | Repository and architecture analysis |
| 29 | v0.6.0 | Developer productivity |
| 30 | v1.0.0 | Migration and backup tooling |
| 31 | v1.0.0 | Performance engineering |
| 32 | v1.0.0 | Security engineering |
| 33 | v1.0.0 | Production distribution |
| 34 | v1.0.0 | Stable public release |

## Architectural decisions

- Start with one Rust process, SQLite, local filesystem, and embedded browser assets.
- Introduce narrow provider interfaces at real boundaries. External providers require implementation, migration and compatibility tests.
- No automatic Backstage plugin compatibility. Consult https://backstage.io/docs/features/software-catalog/descriptor-format/ before implementing descriptor import.
- Add remote plugins only after authentication, permission and resource-limit contracts exist.
- Never silently fall back to stale authorization or local data on remote provider failure.
- Measure actual footprint and capacity; targets are not benchmarks.

## Verification ledger

No implementation phase is complete until actual checks have passed. Use phase-specific reports and distinguish partial, blocked and not-run checks.

- Phase 00: repository, license, contribution and security documentation established; public main branch verified.
- Phase 01: local Rust server, typed configuration, graceful shutdown handler and health/readiness endpoints. Formatting, strict Clippy, debug build, two automated tests and live loopback smoke checks passed on Windows. Shutdown handler exists but graceful shutdown behavior was not independently tested.
- Phase 02: partial. SQLite migrations and persistence across database reopen verified; immutable local object storage upload, download, duplicate rejection and invalid key rejection verified. Replaceable provider contracts remain outstanding.
- Phase 03: partial. Registration, listing, detail, update, deletion, search, pagination and limited Backstage JSON Component import verified. Import currently projects only name, description and owner; broader metadata preservation, YAML support and frontend management remain outstanding.
- Latest local verification: cargo fmt --check, strict Clippy, four automated tests, debug build and live loopback import, update, search, object upload/download and deletion passed on Windows.

Production readiness requires verified authentication, authorization, backup and restore, secure deployment, dependency review and documented operational limits. The current loopback-only preview is not a production release.
