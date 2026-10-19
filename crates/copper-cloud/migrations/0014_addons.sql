-- Copper addons (copper-cloud 0.10.0; see docs/addons.md).
--
-- An addon is a small web bundle written for exactly one *slot* of the Copper window (v1:
-- `agent-sidebar`, the pane toggled by Cmd+E / Ctrl+E). Admins upload it (a `.copperaddon`
-- zip), pick who gets it and set its settings; every signed-in Copper asks
-- `GET /v1/addons` which addons apply to its person.
--
-- * `addons`: one row per manifest id. Off by default (`enabled = false`, audience `people`
--   with nobody in it), so an upload reaches no one until an admin turns it on. `settings`
--   holds the admin's values (manifest defaults fill the rest when served). `revision` is
--   bumped on every change.
-- * `addon_versions`: immutable uploads. `bundle` is the exact served JSON document
--   (`copper-addon-bundle/1`) and `sha256` its lowercase hex digest — the version's identity.
--   `uploaded_by` is NULL for the CLI.
-- * `addon_audience`: who gets an addon whose audience is `people`, by email (stored
--   lower-cased, matched against `users.email` case-insensitively), so a person can be added
--   before they sign up.
--
-- The slot list is closed: new slots arrive with a migration that widens the CHECK.
-- Additive only: existing rows and older clients are unaffected.

CREATE TABLE addons (
    id                 text        PRIMARY KEY
                       CHECK (id ~ '^[a-z0-9][a-z0-9.-]{2,63}$'
                              AND position('..' IN id) = 0
                              AND position('.' IN id) > 0),
    slot               text        NOT NULL CHECK (slot IN ('agent-sidebar')),
    name               text        NOT NULL CHECK (length(name) BETWEEN 1 AND 60),
    enabled            boolean     NOT NULL DEFAULT false,
    audience           text        NOT NULL DEFAULT 'people'
                       CHECK (audience IN ('everyone', 'people')),
    settings           jsonb       NOT NULL DEFAULT '{}'::jsonb
                       CHECK (jsonb_typeof(settings) = 'object'),
    priority           integer     NOT NULL DEFAULT 0
                       CHECK (priority BETWEEN -1000 AND 1000),
    -- FK added below, once addon_versions exists.
    current_version_id uuid,
    revision           bigint      NOT NULL DEFAULT 1 CHECK (revision >= 1),
    created_at         timestamptz NOT NULL DEFAULT now(),
    updated_at         timestamptz NOT NULL DEFAULT now(),
    updated_by         text        CHECK (updated_by IS NULL OR length(updated_by) <= 256)
);

CREATE INDEX addons_slot_idx ON addons (slot) WHERE enabled;

CREATE TABLE addon_versions (
    id          uuid        PRIMARY KEY,
    addon_id    text        NOT NULL REFERENCES addons (id) ON DELETE CASCADE,
    version     text        NOT NULL CHECK (length(version) BETWEEN 5 AND 64),
    manifest    jsonb       NOT NULL CHECK (jsonb_typeof(manifest) = 'object'),
    bundle      bytea       NOT NULL,
    sha256      text        NOT NULL CHECK (sha256 ~ '^[0-9a-f]{64}$'),
    size_bytes  integer     NOT NULL CHECK (size_bytes = octet_length(bundle)),
    uploaded_by uuid        REFERENCES admins (id) ON DELETE SET NULL,
    created_at  timestamptz NOT NULL DEFAULT now(),
    UNIQUE (addon_id, version),
    CHECK (sha256 = encode(sha256(bundle), 'hex'))
);

CREATE INDEX addon_versions_addon_idx ON addon_versions (addon_id, created_at DESC);

ALTER TABLE addons
    ADD CONSTRAINT addons_current_version_fkey
    FOREIGN KEY (current_version_id) REFERENCES addon_versions (id);

CREATE TABLE addon_audience (
    addon_id text        NOT NULL REFERENCES addons (id) ON DELETE CASCADE,
    email    text        NOT NULL
             CHECK (length(email) BETWEEN 3 AND 254 AND email = lower(email)),
    added_at timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (addon_id, email)
);

CREATE INDEX addon_audience_email_idx ON addon_audience (email);
