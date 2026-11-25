CREATE TABLE github_webhook_catalog_targets (
    github_id INTEGER PRIMARY KEY REFERENCES github_webhook_sources(github_id) ON DELETE CASCADE,
    namespace TEXT NOT NULL,
    name TEXT NOT NULL,
    owner TEXT NOT NULL,
    approved_by INTEGER NOT NULL REFERENCES identity_users(id),
    UNIQUE(namespace,name)
);
