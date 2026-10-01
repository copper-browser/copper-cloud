-- Runtime server settings changed by the admin CLI (e.g. allow_signup), overriding the
-- config file default without editing it.
CREATE TABLE server_settings (
    key        text        PRIMARY KEY,
    value      jsonb       NOT NULL,
    updated_at timestamptz NOT NULL DEFAULT now()
);
