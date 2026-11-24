CREATE TABLE scorecard_standards (
 name TEXT NOT NULL, version INTEGER NOT NULL CHECK(version > 0),
 title TEXT NOT NULL, checks TEXT NOT NULL CHECK(json_valid(checks) AND json_type(checks)='array'),
 PRIMARY KEY(name,version)
);
INSERT INTO scorecard_standards VALUES ('engineering',1,'Engineering readiness','["description","ownership","docs","metadata"]');
CREATE TABLE scorecard_evaluations (
 id INTEGER PRIMARY KEY, namespace TEXT NOT NULL, name TEXT NOT NULL,
 standard_name TEXT NOT NULL, standard_version INTEGER NOT NULL,
 team TEXT, actor_id INTEGER REFERENCES identity_users(id) ON DELETE SET NULL,
 results TEXT NOT NULL CHECK(json_valid(results) AND json_type(results)='array'),
 created_at INTEGER NOT NULL DEFAULT(unixepoch()),
 FOREIGN KEY(standard_name,standard_version) REFERENCES scorecard_standards(name,version)
);
CREATE INDEX scorecard_evaluations_team ON scorecard_evaluations(team,id);
CREATE INDEX scorecard_evaluations_component ON scorecard_evaluations(namespace,name,id);
