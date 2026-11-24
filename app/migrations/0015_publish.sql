CREATE TABLE github_publish_attempts (
    owner TEXT NOT NULL COLLATE NOCASE,
    repo TEXT NOT NULL COLLATE NOCASE,
    state TEXT NOT NULL CHECK (state IN ('uncertain','succeeded')),
    github_id INTEGER CHECK (github_id > 0),
    actor_id INTEGER REFERENCES identity_users(id) ON DELETE SET NULL,
    created_at INTEGER NOT NULL DEFAULT (unixepoch()),
    completed_at INTEGER,
    PRIMARY KEY (owner, repo),
    CHECK ((state = 'succeeded' AND github_id IS NOT NULL AND completed_at IS NOT NULL)
        OR (state = 'uncertain' AND github_id IS NULL AND completed_at IS NULL))
);
