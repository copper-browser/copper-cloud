-- Company single sign-on (copper-cloud 0.9.0; see docs/sso.md).
--
-- `sso_providers`: one row per identity provider the admin configured (`google` or a generic
-- OIDC issuer). The client secret is envelope-encrypted like the intelligence keys: a random
-- 32-byte data key wrapped by the master-key-derived KEK, the secret AES-256-GCM sealed under
-- it with AAD "sso:<provider id>". Everything else is not secret and stays plaintext.
CREATE TABLE sso_providers (
    id                   text        PRIMARY KEY CHECK (id ~ '^[a-z0-9][a-z0-9_-]{0,31}$'),
    kind                 text        NOT NULL CHECK (kind IN ('google', 'oidc')),
    name                 text        NOT NULL CHECK (length(name) BETWEEN 1 AND 100),
    client_id            text        NOT NULL CHECK (length(client_id) BETWEEN 1 AND 512),
    data_key_wrapped     bytea       NOT NULL,
    client_secret_sealed bytea,
    -- Generic OIDC only: discovery at <issuer>/.well-known/openid-configuration.
    issuer               text        CHECK (issuer IS NULL OR length(issuer) <= 2048),
    -- Allowed email domains (lower-case); a sign-in from any other domain is refused.
    domains              text[]      NOT NULL CHECK (cardinality(domains) BETWEEN 1 AND 50),
    -- Create an account on first sign-in (no password) for an allowed-domain email.
    auto_create          boolean     NOT NULL DEFAULT true,
    enabled              boolean     NOT NULL DEFAULT true,
    created_at           timestamptz NOT NULL DEFAULT now(),
    updated_at           timestamptz NOT NULL DEFAULT now(),
    -- Admin email, or 'cli'.
    updated_by           text,
    CHECK ((kind = 'oidc') = (issuer IS NOT NULL))
);

-- (provider, subject) → account. A user may have several identities; linking happens on the
-- first SSO sign-in (by email match or account creation).
CREATE TABLE user_identities (
    provider_id   text        NOT NULL REFERENCES sso_providers (id) ON DELETE CASCADE,
    subject       text        NOT NULL CHECK (length(subject) BETWEEN 1 AND 255),
    user_id       uuid        NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    -- The email the provider reported (informational).
    email         text        NOT NULL CHECK (length(email) <= 254),
    created_at    timestamptz NOT NULL DEFAULT now(),
    last_login_at timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (provider_id, subject)
);

CREATE INDEX user_identities_user_idx ON user_identities (user_id);

-- SSO-only accounts have no password (NULL); password sign-in answers 401 `sso_only`.
ALTER TABLE users ALTER COLUMN password_hash DROP NOT NULL;

-- `false`: password signup/login is off for Copper users (SSO only). Portal admins are
-- unaffected (separate accounts).
INSERT INTO server_settings (key, value) VALUES ('password_signin', 'true')
ON CONFLICT (key) DO NOTHING;
