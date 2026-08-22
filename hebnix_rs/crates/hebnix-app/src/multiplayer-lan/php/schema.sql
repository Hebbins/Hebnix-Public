CREATE TABLE IF NOT EXISTS rooms (
    pin TEXT PRIMARY KEY,
    host_secret TEXT NOT NULL,
    join_token TEXT NOT NULL,
    host_name TEXT NOT NULL,
    host TEXT NOT NULL,
    port INTEGER NOT NULL,
    map_id TEXT NOT NULL,
    map_name TEXT NOT NULL,
    map_sha256 TEXT NOT NULL,
    map_download_url TEXT NOT NULL,
    protocol_version INTEGER NOT NULL,
    expires_at TEXT NOT NULL
);

CREATE INDEX IF NOT EXISTS rooms_expiry ON rooms (expires_at);
