-- Append-only browsing history. payload = AES-256-GCM(user data key,
-- aad = "<user_id>:history"), the entry JSON as posted by the client.
CREATE TABLE history (
    seq        bigserial   PRIMARY KEY,
    user_id    uuid        NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    device_id  uuid        NOT NULL,
    visited_at timestamptz NOT NULL,
    created_at timestamptz NOT NULL DEFAULT now(),
    payload    bytea       NOT NULL
);

CREATE INDEX history_user_seq_idx ON history (user_id, seq);
