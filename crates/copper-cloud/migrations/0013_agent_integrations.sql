-- Agent integrations (copper-cloud 0.9.0; see docs/sso.md "Agent integrations").
--
-- An integration (e.g. a bot platform) holds a `cai_…` key the admin mints; with it the
-- service may sign a headless Copper in to any allowed-domain user's account through
-- `POST /v1/agents/pair`. Only SHA-256(key) is stored.
CREATE TABLE agent_integrations (
    id               uuid        PRIMARY KEY,
    name             text        NOT NULL CHECK (length(name) BETWEEN 1 AND 100),
    key_sha256       bytea       NOT NULL UNIQUE CHECK (length(key_sha256) = 32),
    -- Allowed email domains (lower-case); empty = the enabled SSO providers' domains.
    domains          text[]      NOT NULL DEFAULT '{}' CHECK (cardinality(domains) <= 50),
    -- Create a (password-less) account for an allowed-domain email that has none yet.
    create_users     boolean     NOT NULL DEFAULT true,
    created_by_admin uuid        REFERENCES admins (id) ON DELETE SET NULL,
    created_at       timestamptz NOT NULL DEFAULT now(),
    rotated_at       timestamptz,
    last_used_at     timestamptz,
    revoked_at       timestamptz
);

CREATE INDEX agent_integrations_created_idx ON agent_integrations (created_at DESC);

-- Devices signed in through an integration (agent devices).
ALTER TABLE devices
    ADD COLUMN agent_integration_id uuid REFERENCES agent_integrations (id) ON DELETE SET NULL;

CREATE INDEX devices_agent_integration_idx ON devices (agent_integration_id)
    WHERE agent_integration_id IS NOT NULL;

-- A pairing code minted by `POST /v1/agents/pair` is bound to one device id: redeeming it
-- signs in exactly that device, whatever id the redeemer sends.
ALTER TABLE pairing_codes ADD COLUMN device_id uuid;

-- The device a gate key was minted for (pairing / SSO / agent pairing in directory mode), so
-- re-pairing that device can revoke it.
ALTER TABLE access_keys ADD COLUMN device_id uuid;

CREATE INDEX access_keys_device_idx ON access_keys (created_by_user, device_id)
    WHERE device_id IS NOT NULL;
