-- SQLx applies this migration in a transaction. Copy all existing values verbatim.
CREATE TABLE components_with_namespaces (
    name TEXT NOT NULL,
    description TEXT NOT NULL,
    owner TEXT NOT NULL,
    namespace TEXT NOT NULL DEFAULT 'default',
    component_type TEXT,
    lifecycle TEXT,
    descriptor TEXT CHECK (descriptor IS NULL OR json_valid(descriptor)),
    PRIMARY KEY (namespace, name)
);

INSERT INTO components_with_namespaces (name, description, owner, namespace, component_type, lifecycle, descriptor)
SELECT name, description, owner, namespace, component_type, lifecycle, descriptor FROM components;

DROP TABLE components;
ALTER TABLE components_with_namespaces RENAME TO components;
