# Changelog

## Unreleased

- Initial Rust developer portal foundation (implementation in progress).
- Typed local provider selection with explicit unsupported-provider errors.
- Replaceable object storage contract and additional SQLite/data durability checks.
- Additive catalog migration preserving existing records while retaining supported imported Component metadata.
- Limited YAML and JSON Component descriptor import with bounded input and validation.
- Storage provider capability checks before serving traffic.
- Namespace-qualified Component identities and safe migration from legacy name-only primary keys.
- Namespace-scoped catalog operations, filtering and bounded pagination.
- Validated descriptor-derived outbound catalog relationship projections.
- Local Argon2id identity, sessions, one-time offline administrator bootstrap and basic role authorization.
- Authenticated team and membership management with audit events.
- Embedded React, TypeScript and Vite developer portal with functional catalog and team views.
- Frontend build, lint, type checking and tests in continuous integration.
- Administrator-only local account creation with validated roles, atomic audit records and a developer portal form.
