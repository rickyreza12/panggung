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
- Phase 02: local implementation criteria verified. SQLite migration and persistence, foreign keys, typed provider selection and rejection, explicit object storage capabilities, local immutable file storage, and replaceable catalog repository/object interfaces have passing tests. External providers are intentionally unsupported; no compatibility with them is claimed.
- Phase 03: partial. Registration, listing, detail, update, deletion, search and bounded pagination verified. Additive migrations retain legacy data, add metadata and upgrade to namespace/name identity; JSON and YAML Component imports preserve supported descriptor metadata. Namespace-qualified operations isolate same-named components. Outbound references now resolve against registered components, systems, default-namespace teams and users. Missing or unsupported targets are explicitly unresolved. Domain/system registration and domain foreign keys are implemented. Full Backstage schema compatibility and granular ownership permissions remain outstanding.
- Phase 04: partial. Local users with Argon2id password hashes, hashed and expiring sessions, one-time offline administrator bootstrap, server-side administrator/member/viewer roles, team and membership operations, and audit events are implemented and covered by tests. Granular ownership permissions, external identity, fully managed users, production hardening and security assessment remain outstanding.
- Phase 05: partial. React, TypeScript and Vite source, responsive embedded browser assets, working catalog/forms, login/logout and actual team management are implemented. Source linting, type checking, six unit tests and frontend production build passed. Full accessibility audit, cross-browser end-to-end verification and all planned portal sections remain outstanding.
- Latest combined local verification before the next release commit: 29 Rust tests, strict Clippy, Rust build, frontend type checking, lint, six tests and production build passed on Windows. Isolated loopback runtime smoke confirmed embedded frontend assets, startup/restart and unauthenticated route protections. The administrator increment passed remote workflow 45366264930. The catalog structure and template registry increment passed 34 Rust tests and 11 frontend tests, strict Clippy, formatting, frontend type checking/lint/build and embedded asset synchronization locally. Runtime and browser verification and remote checks for the latest increment remain outstanding.

- Phase 06: partial. Authenticated immutable template registry and administrator creation, five seeded stack definitions, bounded parameter schemas, pagination and integrated frontend. Scaffolding, ownership-specific permissions and publishing remain outstanding.

- Phases 07-11: partial. Immutable scaffolder archives for five stacks, versioned documentation, embedded search, interface versions, scorecards, guarded GitHub publishing, signed webhook intake and approved canonical refresh implemented. Local baseline b2fa365: 62 Rust tests, 14 frontend tests, strict Clippy, debug build, mapping/browser acceptance and asset integrity passed. Remote workflow 45552716282 failed at a newer Clippy lint; no remote success claimed.
- Phase 13: partial. Actual OpenTelemetry providers and emitted request/operation metrics and spans tested, authenticated opt-in scraping and Grafana provisioning implemented. Exporter operational deployment, full lifecycle and performance acceptance remain outstanding.
- Standalone synthetic acceptance: controlled debug fixture executable verified member registration, administrator mapping, canonical retrieval, signed delivery, persisted entity/relationship updates, restart, replay and expired-lease recovery (isolated fixture). No external repositories mutated. Further conflict, saturation and security acceptance remains outstanding.

Production readiness requires verified authentication, authorization, backup and restore, secure deployment, dependency review and documented operational limits. The current loopback-only preview is not a production release.
