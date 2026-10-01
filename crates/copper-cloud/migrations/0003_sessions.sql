-- Opaque bearer sessions. Only SHA-256(token) is stored. Sliding expiry: every use (touch
-- throttled to once per minute) pushes expires_at to last_seen_at + 90 days.
CREATE TABLE sessions (
    id           uuid        PRIMARY KEY,
    user_id      uuid        NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    device_id    uuid        NOT NULL,
    token_sha256 bytea       NOT NULL UNIQUE CHECK (length(token_sha256) = 32),
    created_at   timestamptz NOT NULL DEFAULT now(),
    last_seen_at timestamptz NOT NULL DEFAULT now(),
    expires_at   timestamptz NOT NULL,
    FOREIGN KEY (user_id, device_id) REFERENCES devices (user_id, id) ON DELETE CASCADE
);

CREATE INDEX sessions_user_idx ON sessions (user_id, device_id);
CREATE INDEX sessions_expires_idx ON sessions (expires_at);
