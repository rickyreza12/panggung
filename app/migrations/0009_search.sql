CREATE VIRTUAL TABLE component_search USING fts5(
    name, namespace, description, owner, component_type, lifecycle,
    tokenize = 'unicode61'
);

INSERT INTO component_search(rowid,name,namespace,description,owner,component_type,lifecycle)
SELECT rowid,name,namespace,description,owner,component_type,lifecycle FROM components;

CREATE TRIGGER component_search_insert AFTER INSERT ON components BEGIN
    INSERT INTO component_search(rowid,name,namespace,description,owner,component_type,lifecycle)
    VALUES (new.rowid,new.name,new.namespace,new.description,new.owner,new.component_type,new.lifecycle);
END;

CREATE TRIGGER component_search_update AFTER UPDATE ON components BEGIN
    DELETE FROM component_search WHERE rowid = old.rowid;
    INSERT INTO component_search(rowid,name,namespace,description,owner,component_type,lifecycle)
    VALUES (new.rowid,new.name,new.namespace,new.description,new.owner,new.component_type,new.lifecycle);
END;

CREATE TRIGGER component_search_delete AFTER DELETE ON components BEGIN
    DELETE FROM component_search WHERE rowid = old.rowid;
END;
