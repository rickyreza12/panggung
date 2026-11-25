ALTER TABLE github_artifact_publications ADD COLUMN baseline_sha TEXT;
ALTER TABLE github_artifact_publications ADD COLUMN repository_id INTEGER;
ALTER TABLE github_artifact_publications ADD COLUMN manifest_digest TEXT;
ALTER TABLE github_artifact_publications ADD COLUMN candidate_sha TEXT;
ALTER TABLE github_artifact_publications ADD COLUMN candidate_tree TEXT;
ALTER TABLE github_artifact_publications ADD COLUMN mutation_stage TEXT NOT NULL DEFAULT 'legacy' CHECK(mutation_stage IN ('legacy','reserved','candidate','rejected','confirmed'));
ALTER TABLE github_artifact_publications ADD COLUMN completed_at INTEGER;
CREATE TRIGGER artifact_success_requires_sha_insert BEFORE INSERT ON github_artifact_publications
WHEN NEW.state='succeeded' AND (NEW.commit_sha IS NULL OR length(NEW.commit_sha) != 40)
BEGIN SELECT RAISE(ABORT, 'Publication success requires commit'); END;
CREATE TRIGGER artifact_success_requires_sha_update BEFORE UPDATE ON github_artifact_publications
WHEN NEW.state='succeeded' AND (NEW.commit_sha IS NULL OR length(NEW.commit_sha) != 40)
BEGIN SELECT RAISE(ABORT, 'Publication success requires commit'); END;
