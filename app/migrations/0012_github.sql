CREATE TABLE github_repositories (
    github_id INTEGER PRIMARY KEY CHECK (github_id > 0),
    owner TEXT NOT NULL COLLATE NOCASE,
    name TEXT NOT NULL COLLATE NOCASE,
    description TEXT,
    default_branch TEXT NOT NULL,
    archived INTEGER NOT NULL CHECK (archived IN (0,1)),
    descriptor_status TEXT NOT NULL CHECK (descriptor_status IN ('found','missing','unsupported')),
    synced_at INTEGER NOT NULL DEFAULT (unixepoch()),
    synced_by INTEGER REFERENCES identity_users(id) ON DELETE SET NULL,
    UNIQUE(owner,name)
);
