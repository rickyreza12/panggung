CREATE TABLE techdocs_versions (
    namespace TEXT NOT NULL,
    component_name TEXT NOT NULL,
    version INTEGER NOT NULL CHECK (version > 0),
    object_key TEXT NOT NULL UNIQUE CHECK (length(object_key) = 32),
    content_digest BLOB NOT NULL CHECK (length(content_digest) = 32),
    created_at INTEGER NOT NULL DEFAULT (unixepoch()),
    created_by INTEGER REFERENCES identity_users(id) ON DELETE SET NULL,
    PRIMARY KEY (namespace, component_name, version),
    FOREIGN KEY (namespace, component_name) REFERENCES components(namespace, name) ON DELETE CASCADE
);
