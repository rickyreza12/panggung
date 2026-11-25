CREATE TABLE github_webhook_jobs (
    id INTEGER PRIMARY KEY,
    delivery TEXT NOT NULL UNIQUE REFERENCES github_webhook_deliveries(delivery),
    github_id INTEGER NOT NULL REFERENCES github_repositories(github_id),
    owner TEXT NOT NULL,
    repo TEXT NOT NULL,
    state TEXT NOT NULL DEFAULT 'queued' CHECK(state IN ('queued','running','succeeded','failed','conflict','unsupported')),
    attempts INTEGER NOT NULL DEFAULT 0 CHECK(attempts BETWEEN 0 AND 5),
    available_at INTEGER NOT NULL DEFAULT(unixepoch()),
    lease_until INTEGER,
    finished_at INTEGER,
    outcome TEXT
);
CREATE INDEX github_webhook_jobs_pending ON github_webhook_jobs(state,available_at,id);
CREATE TABLE github_webhook_sources (
    github_id INTEGER PRIMARY KEY REFERENCES github_repositories(github_id),
    owner TEXT NOT NULL,
    repo TEXT NOT NULL,
    approved_by INTEGER NOT NULL REFERENCES identity_users(id)
);
