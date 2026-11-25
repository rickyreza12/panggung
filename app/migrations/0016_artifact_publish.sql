CREATE TABLE github_artifact_publications (
    owner TEXT NOT NULL COLLATE NOCASE,
    repo TEXT NOT NULL COLLATE NOCASE,
    job_id INTEGER NOT NULL UNIQUE REFERENCES scaffolder_jobs(id),
    actor_id INTEGER NOT NULL REFERENCES identity_users(id),
    state TEXT NOT NULL CHECK(state IN ('uncertain','succeeded')),
    commit_sha TEXT,
    created_at INTEGER NOT NULL DEFAULT (unixepoch()),
    PRIMARY KEY(owner,repo),
    FOREIGN KEY(owner,repo) REFERENCES github_publish_attempts(owner,repo),
    CHECK((state='uncertain' AND commit_sha IS NULL) OR (state='succeeded' AND length(commit_sha)=40))
);
