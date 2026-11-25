ALTER TABLE components ADD COLUMN sync_revision INTEGER NOT NULL DEFAULT 0;
ALTER TABLE github_repositories ADD COLUMN sync_revision INTEGER NOT NULL DEFAULT 0;
CREATE TRIGGER components_sync_revision AFTER UPDATE ON components
WHEN NEW.sync_revision=OLD.sync_revision
BEGIN
    UPDATE components SET sync_revision=OLD.sync_revision+1 WHERE namespace=NEW.namespace AND name=NEW.name;
END;
CREATE TRIGGER github_sync_revision AFTER UPDATE ON github_repositories
WHEN NEW.sync_revision=OLD.sync_revision
BEGIN
    UPDATE github_repositories SET sync_revision=OLD.sync_revision+1 WHERE github_id=NEW.github_id;
END;
