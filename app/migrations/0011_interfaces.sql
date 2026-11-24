CREATE TABLE interface_versions (
    namespace TEXT NOT NULL,
    component_name TEXT NOT NULL,
    version INTEGER NOT NULL CHECK(version > 0),
    title TEXT NOT NULL,
    specification TEXT NOT NULL CHECK(json_valid(specification)),
    created_at INTEGER NOT NULL DEFAULT(unixepoch()),
    created_by INTEGER REFERENCES identity_users(id) ON DELETE SET NULL,
    PRIMARY KEY(namespace, component_name, version),
    FOREIGN KEY(namespace, component_name) REFERENCES components(namespace,name) ON DELETE CASCADE
);
