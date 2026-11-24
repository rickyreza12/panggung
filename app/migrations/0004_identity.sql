CREATE TABLE identity_users (
    id INTEGER PRIMARY KEY,
    username TEXT NOT NULL UNIQUE,
    password_hash TEXT NOT NULL,
    role TEXT NOT NULL CHECK (role IN ('admin', 'member', 'viewer')),
    created_at INTEGER NOT NULL DEFAULT (unixepoch())
);

CREATE TABLE identity_sessions (
    token_hash BLOB PRIMARY KEY NOT NULL CHECK (length(token_hash) = 32),
    user_id INTEGER NOT NULL REFERENCES identity_users(id) ON DELETE CASCADE,
    created_at INTEGER NOT NULL DEFAULT (unixepoch()),
    expires_at INTEGER NOT NULL CHECK (expires_at > created_at),
    revoked_at INTEGER
);
CREATE INDEX identity_sessions_user ON identity_sessions(user_id);

CREATE TABLE identity_teams (
    id INTEGER PRIMARY KEY,
    name TEXT NOT NULL UNIQUE
);
CREATE TABLE identity_team_memberships (
    team_id INTEGER NOT NULL REFERENCES identity_teams(id) ON DELETE CASCADE,
    user_id INTEGER NOT NULL REFERENCES identity_users(id) ON DELETE CASCADE,
    PRIMARY KEY (team_id, user_id)
);

CREATE TABLE identity_audit_events (
    id INTEGER PRIMARY KEY,
    actor_id INTEGER REFERENCES identity_users(id) ON DELETE SET NULL,
    subject_id INTEGER REFERENCES identity_users(id) ON DELETE SET NULL,
    action TEXT NOT NULL,
    created_at INTEGER NOT NULL DEFAULT (unixepoch())
);
