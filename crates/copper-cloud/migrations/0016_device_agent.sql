-- Device agent relay (copper-cloud 0.13.0; see docs/api.md "Device agent relay").
--
-- An agent (any MCP client) calls the tools of a person's Copper for iOS through the cloud: the
-- phone registers a link with its tool manifest and holds an SSE mailbox open while Copper is
-- on screen; the agent presents a `cat_…` token the phone minted. Which link is online and the
-- calls in flight live in memory (one daemon per instance), never here.

-- One link per (user, device). `tools` is the manifest the device last registered (a JSON
-- array, ≤ 100 entries, ≤ 256 KiB), served to agents by `tools/list` even while the phone is
-- closed. Revoking (or deleting the device) sets `revoked_at`; registering again clears it and
-- keeps the id. No foreign key to `devices`, so a revoked link outlives its device row.
CREATE TABLE device_links (
    id             uuid        PRIMARY KEY,
    user_id        uuid        NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    device_id      uuid        NOT NULL,
    label          text        NOT NULL CHECK (length(label) BETWEEN 1 AND 200),
    client_version text        CHECK (client_version IS NULL OR length(client_version) <= 64),
    tools          jsonb       NOT NULL DEFAULT '[]'::jsonb CHECK (jsonb_typeof(tools) = 'array'),
    created_at     timestamptz NOT NULL DEFAULT now(),
    last_seen_at   timestamptz,
    revoked_at     timestamptz,
    UNIQUE (user_id, device_id)
);

-- Agent tokens: `cat_` + 32 random bytes base64url, minted by a device for one agent and bound
-- to that user and device. Only SHA-256(token) is stored; the token is shown once. Deleting the
-- device revokes them.
CREATE TABLE agent_tokens (
    id           uuid        PRIMARY KEY,
    user_id      uuid        NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    device_id    uuid        NOT NULL,
    label        text        NOT NULL CHECK (length(label) BETWEEN 1 AND 100),
    token_sha256 bytea       NOT NULL UNIQUE CHECK (length(token_sha256) = 32),
    created_at   timestamptz NOT NULL DEFAULT now(),
    last_used_at timestamptz,
    revoked_at   timestamptz
);

CREATE INDEX agent_tokens_user_idx ON agent_tokens (user_id, created_at DESC);
CREATE INDEX agent_tokens_device_idx ON agent_tokens (user_id, device_id) WHERE revoked_at IS NULL;
