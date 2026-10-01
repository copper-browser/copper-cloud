-- Last-writer-wins whole-document sync domains: spaces, settings, bookmarks, tabs:<device_id>.
-- payload = AES-256-GCM(user data key, aad = "<user_id>:<domain>"), nonce(12) || ct.
CREATE TABLE sync_docs (
    user_id       uuid        NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    domain        text        NOT NULL CHECK (length(domain) BETWEEN 1 AND 64),
    device_id     uuid,
    version       bigint      NOT NULL CHECK (version > 0),
    updated_at    timestamptz NOT NULL DEFAULT now(),
    payload       bytea       NOT NULL,
    payload_bytes integer     NOT NULL CHECK (payload_bytes >= 0),
    PRIMARY KEY (user_id, domain)
);
