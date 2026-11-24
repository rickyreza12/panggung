# Panggung application

Initial development target: a local-only Rust developer portal. No production authentication or remote provider support yet. Run cargo run from this directory; tests use isolated data. See the repository README and roadmap.

References: https://docs.rs/axum/0.8/axum/ ; https://docs.rs/tokio/1/tokio/ ; https://docs.rs/sqlx/0.8/sqlx/ ; https://www.sqlite.org/lang.html .

## Local interface

- GET /api/components lists up to 500 records; q filters name, description and owner (ASCII case-insensitive); offset selects a page.
- POST /api/components registers a component with name, description and owner.
- GET, PUT and DELETE /api/components/{name} retrieve, update and delete a record. Names are immutable.
- POST /api/catalog/import accepts one JSON Backstage Component descriptor with apiVersion backstage.io/v1alpha1 and default namespace only. It stores name, description and owner; type and lifecycle are required but not yet persisted. Other metadata and relationships are not imported. YAML and remote ingestion are not supported yet.
- PUT and GET /api/objects/{key} create and retrieve immutable binary objects (32 lowercase hexadecimal characters per key, maximum 1 MiB per object). Duplicate keys return conflict. Objects reside in the objects directory under PANGGUNG_DATA_DIR. No deletion or quota management yet. Keep this directory private to the application account.

The descriptor reference is https://backstage.io/docs/features/software-catalog/descriptor-format/ . These endpoints are local development interfaces, not Backstage-compatible web endpoints or authenticated production interfaces.
