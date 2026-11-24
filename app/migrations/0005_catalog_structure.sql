CREATE TABLE catalog_domains (
    namespace TEXT NOT NULL,
    name TEXT NOT NULL,
    PRIMARY KEY (namespace, name)
);

CREATE TABLE catalog_systems (
    namespace TEXT NOT NULL,
    name TEXT NOT NULL,
    domain_namespace TEXT,
    domain_name TEXT,
    PRIMARY KEY (namespace, name),
    CHECK ((domain_namespace IS NULL) = (domain_name IS NULL)),
    FOREIGN KEY (domain_namespace, domain_name) REFERENCES catalog_domains(namespace, name)
);
