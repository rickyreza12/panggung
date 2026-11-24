# Panggung application

Initial development target: a local-only Rust developer portal. No production authentication or remote provider support yet. Run cargo run from this directory; tests use isolated data. See the repository README and roadmap.

Configuration: `PANGGUNG_PORT` defaults to `3000`, `PANGGUNG_DATA_DIR` defaults to `data`, `PANGGUNG_DATABASE_PROVIDER` defaults to `sqlite`, and `PANGGUNG_STORAGE_PROVIDER` defaults to `local`. Unsupported provider values fail startup; remote providers are not implemented. The application listens only on loopback.

References: https://docs.rs/axum/0.8/axum/ ; https://docs.rs/tokio/1/tokio/ ; https://docs.rs/sqlx/0.8/sqlx/ ; https://www.sqlite.org/lang.html .

## Local interface

- GET /api/components lists records across namespaces; optional namespace filters an exact namespace, q filters name, description and owner (ASCII case-insensitive), offset selects a page, and limit accepts 1–500 (default 500). Results are ordered by name then namespace.
- POST /api/components registers a component with name, description and owner in the default namespace.
- GET, PUT and DELETE /api/components/{name} operate only in the default namespace. GET, PUT and DELETE /api/components/{namespace}/{name} address a specific namespace. Names and namespaces are immutable; updates accept name, description and owner as before.
- POST /api/catalog/import accepts one JSON Backstage Component descriptor with apiVersion backstage.io/v1alpha1. POST /api/catalog/import/yaml accepts a single UTF-8 YAML descriptor. Both validate the same subset and reject duplicate namespace/name identities, unsupported kinds and versions, invalid namespaces, unresolved external substitutions and malformed known metadata fields. The descriptor is limited to 64 KiB of serialized JSON (and YAML input is limited to 64 KiB).
- Component responses retain name, description and owner and additionally include namespace, component_type, lifecycle and descriptor. Imported descriptors retain JSON-compatible metadata (including annotations, labels, tags, links), specification fields and extensions; supported description and owner updates also update the stored descriptor. Legacy rows and manual registrations retain their original data and have null type, lifecycle and descriptor. The additive migration does not invent missing metadata.
- Component identity is the namespace/name pair (kind is fixed to Component). An omitted namespace defaults to `default`; explicit namespaces use 1–63 lowercase ASCII letters, digits, hyphens, underscores or periods, starting and ending with a letter or digit. Existing lowercase letter, digit and hyphen name rules remain. Migration 0003 copies existing records and their descriptor text unchanged into the composite-key table in a transaction. Links must use HTTP or HTTPS. Relationship references are stored as data; they are not resolved into a graph. Full Backstage schema validation, multi-document YAML, remote ingestion and external substitutions remain outside this subset. No descriptor link is fetched during import.
- PUT and GET /api/objects/{key} create and retrieve immutable binary objects (32 lowercase hexadecimal characters per key, maximum 1 MiB per object). Duplicate keys return conflict. Objects reside in the objects directory under PANGGUNG_DATA_DIR. No deletion or quota management yet. Keep this directory private to the application account.

The descriptor reference is https://backstage.io/docs/features/software-catalog/descriptor-format/ . These endpoints are local development interfaces, not Backstage-compatible web endpoints or authenticated production interfaces.
