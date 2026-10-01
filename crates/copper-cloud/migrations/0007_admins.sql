-- Admin accounts for the web portal / admin API (`/admin/api/*`). Separate from `users`:
-- admins manage the instance, they do not sync. Emails are stored lower-cased.
CREATE TABLE admins (
    id            uuid        PRIMARY KEY,
    email         text        NOT NULL CHECK (length(email) BETWEEN 3 AND 254),
    password_hash text        NOT NULL,
    created_at    timestamptz NOT NULL DEFAULT now(),
    last_login_at timestamptz
);

CREATE UNIQUE INDEX admins_email_lower_key ON admins (lower(email));

-- Cookie sessions (`cc_admin`). Only SHA-256(token) is stored; fixed 7-day lifetime.
CREATE TABLE admin_sessions (
    id           uuid        PRIMARY KEY,
    admin_id     uuid        NOT NULL REFERENCES admins (id) ON DELETE CASCADE,
    token_sha256 bytea       NOT NULL UNIQUE CHECK (length(token_sha256) = 32),
    created_at   timestamptz NOT NULL DEFAULT now(),
    expires_at   timestamptz NOT NULL,
    last_seen_at timestamptz NOT NULL DEFAULT now()
);

CREATE INDEX admin_sessions_admin_idx ON admin_sessions (admin_id);
CREATE INDEX admin_sessions_expires_idx ON admin_sessions (expires_at);

-- Every admin mutation. `target` is the affected id (or a name such as 'settings');
-- `detail` carries non-secret context (changed fields, labels, emails).
CREATE TABLE admin_audit (
    id       bigserial   PRIMARY KEY,
    admin_id uuid        REFERENCES admins (id) ON DELETE SET NULL,
    action   text        NOT NULL CHECK (length(action) BETWEEN 1 AND 64),
    target   text        NOT NULL DEFAULT '' CHECK (length(target) <= 256),
    detail   jsonb,
    at       timestamptz NOT NULL DEFAULT now()
);

CREATE INDEX admin_audit_at_idx ON admin_audit (at DESC);
