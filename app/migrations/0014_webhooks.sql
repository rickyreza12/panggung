CREATE TABLE github_webhook_config (
    singleton INTEGER PRIMARY KEY CHECK (singleton = 1),
    enabled INTEGER NOT NULL CHECK (enabled IN (0,1))
);
INSERT INTO github_webhook_config(singleton,enabled) VALUES(1,0);
CREATE TABLE github_webhook_deliveries (
    delivery TEXT PRIMARY KEY NOT NULL,
    payload_digest BLOB NOT NULL CHECK(length(payload_digest)=32),
    event TEXT NOT NULL,
    received_at INTEGER NOT NULL DEFAULT(unixepoch())
);
CREATE INDEX github_webhook_digest ON github_webhook_deliveries(payload_digest);
