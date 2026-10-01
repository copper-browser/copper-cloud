-- Per-person gate credentials (`X-Copper-Instance: ck_…`). Only SHA-256(key) is stored.
-- A key passes the gate while not revoked and not expired; `uses` counts accounts created
-- (signups) with the key and `max_uses` caps that number.
CREATE TABLE access_keys (
    id               uuid        PRIMARY KEY,
    key_sha256       bytea       NOT NULL UNIQUE CHECK (length(key_sha256) = 32),
    label            text        NOT NULL CHECK (length(label) BETWEEN 1 AND 200),
    -- When set, signup with this key must use this email (case-insensitive).
    email            text        CHECK (email IS NULL OR length(email) BETWEEN 3 AND 254),
    created_by_admin uuid        REFERENCES admins (id) ON DELETE SET NULL,
    -- Set for keys minted by pairing (directory mode).
    created_by_user  uuid        REFERENCES users (id) ON DELETE SET NULL,
    created_at       timestamptz NOT NULL DEFAULT now(),
    expires_at       timestamptz,
    revoked_at       timestamptz,
    last_used_at     timestamptz,
    uses             integer     NOT NULL DEFAULT 0 CHECK (uses >= 0),
    max_uses         integer     CHECK (max_uses IS NULL OR max_uses > 0)
);

CREATE INDEX access_keys_created_idx ON access_keys (created_at DESC);
CREATE INDEX access_keys_email_lower_idx ON access_keys (lower(email)) WHERE email IS NOT NULL;

-- `open`: the shared instance key (and access keys) pass the gate; `directory`: only access
-- keys. Existing instances keep today's behaviour; install.sh / Terraform switch new
-- instances to `directory` when they create the first admin.
INSERT INTO server_settings (key, value) VALUES ('access_mode', '"open"')
ON CONFLICT (key) DO NOTHING;
