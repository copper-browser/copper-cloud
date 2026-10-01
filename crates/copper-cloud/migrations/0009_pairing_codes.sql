-- Single-use pairing codes (`cp_…`): a signed-in Copper mints one to link and sign in
-- another Copper. Only SHA-256(code) is stored; valid 10 minutes.
CREATE TABLE pairing_codes (
    id                uuid        PRIMARY KEY,
    user_id           uuid        NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    created_by_device uuid,
    -- Optional name for the device being paired (used when the new device sends none).
    device_name       text        CHECK (device_name IS NULL OR length(device_name) <= 200),
    code_sha256       bytea       NOT NULL UNIQUE CHECK (length(code_sha256) = 32),
    created_at        timestamptz NOT NULL DEFAULT now(),
    expires_at        timestamptz NOT NULL,
    used_at           timestamptz,
    used_by_device    uuid
);

CREATE INDEX pairing_codes_user_active_idx ON pairing_codes (user_id, created_at DESC)
    WHERE used_at IS NULL;
CREATE INDEX pairing_codes_expires_idx ON pairing_codes (expires_at);
