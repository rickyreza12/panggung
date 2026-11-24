CREATE TABLE scaffolder_jobs (
    id INTEGER PRIMARY KEY,
    actor_id INTEGER NOT NULL REFERENCES identity_users(id),
    name TEXT NOT NULL,
    version TEXT NOT NULL,
    artifact TEXT NOT NULL CHECK(json_valid(artifact)),
    created_at INTEGER NOT NULL DEFAULT (unixepoch()),
    FOREIGN KEY (name, version) REFERENCES software_template_versions(name, version)
);
CREATE INDEX scaffolder_jobs_actor ON scaffolder_jobs(actor_id, id);
