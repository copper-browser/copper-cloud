-- Cloud-wide intelligence keys: one Jev (TypeSafe) key and one LLM router (LiteLLM) key that
-- the admin sets once and every signed-in Copper on the instance fetches from
-- `GET /v1/intelligence`, so nobody pastes keys by hand.
--
-- Singleton row (id = 1). Keys are envelope-encrypted like user data: `data_key_wrapped` is a
-- random 32-byte key wrapped by the master-key-derived KEK; each key is AES-256-GCM sealed
-- under it with AAD "intelligence:jev" / "intelligence:router". Endpoints, model and URL are
-- not secret and stay plaintext. A block is either fully set or fully NULL.
CREATE TABLE intelligence_settings (
    id                smallint    PRIMARY KEY DEFAULT 1 CHECK (id = 1),
    data_key_wrapped  bytea       NOT NULL,
    jev_key_sealed    bytea,
    jev_endpoint      text,
    jev_model         text,
    router_key_sealed bytea,
    router_url        text,
    -- Admin toggle: when false, `/v1/intelligence` answers as if nothing were set.
    enabled           boolean     NOT NULL DEFAULT true,
    updated_at        timestamptz NOT NULL DEFAULT now(),
    -- Admin email, or 'cli' for `copper-cloud intelligence …`.
    updated_by        text,
    CHECK ((jev_key_sealed IS NULL) = (jev_endpoint IS NULL)
       AND (jev_key_sealed IS NULL) = (jev_model IS NULL)),
    CHECK ((router_key_sealed IS NULL) = (router_url IS NULL))
);
