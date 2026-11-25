ALTER TABLE github_artifact_publications ADD COLUMN outcome TEXT NOT NULL DEFAULT 'unresolved' CHECK(outcome IN ('unresolved','rejected','conflict','confirmed'));
UPDATE github_artifact_publications SET outcome='confirmed' WHERE state='succeeded';
UPDATE github_artifact_publications SET outcome='rejected' WHERE mutation_stage='rejected' AND state='uncertain';
