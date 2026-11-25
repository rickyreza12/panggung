-- A deleted catalog entity cannot silently transfer its approval to a replacement.
CREATE TRIGGER webhook_catalog_deleted AFTER DELETE ON components
BEGIN
    DELETE FROM github_webhook_catalog_targets WHERE namespace=OLD.namespace AND name=OLD.name;
END;
-- Ownership changes require explicit reapproval, even if ownership later changes back.
CREATE TRIGGER webhook_catalog_owner_changed AFTER UPDATE OF owner ON components
WHEN NEW.owner<>OLD.owner
BEGIN
    DELETE FROM github_webhook_catalog_targets WHERE namespace=OLD.namespace AND name=OLD.name;
END;
