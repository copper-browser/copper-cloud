-- Accounts. Emails are unique per instance, case-insensitively (lower(email) index; no
-- citext extension so managed Postgres without superuser works).
CREATE TABLE users (
    id               uuid        PRIMARY KEY,
    email            text        NOT NULL CHECK (length(email) BETWEEN 3 AND 254),
    display_name     text        NOT NULL DEFAULT '' CHECK (length(display_name) <= 200),
    password_hash    text        NOT NULL,
    -- Per-user AES-256 data key, wrapped (AES-GCM) by the KEK derived from master_key.
    data_key_wrapped bytea       NOT NULL,
    created_at       timestamptz NOT NULL DEFAULT now(),
    updated_at       timestamptz NOT NULL DEFAULT now(),
    disabled         boolean     NOT NULL DEFAULT false
);

CREATE UNIQUE INDEX users_email_lower_key ON users (lower(email));
