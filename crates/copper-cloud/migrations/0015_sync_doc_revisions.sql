-- Recoverable sync docs (copper-cloud 0.12.0). The server cannot read a doc, so its guard is
-- keeping what a write replaced: every accepted overwrite of a sync_docs row first copies the
-- old row here, sealed bytes as they were (same AAD "<user_id>:<domain>"). The last 20 per
-- (user, domain) are kept; a restore writes one back as a new version.
--
-- client_build: the optional X-Copper-Build header of the write (NULL for older clients).

ALTER TABLE sync_docs
    ADD COLUMN client_build text CHECK (client_build IS NULL OR length(client_build) <= 64);

CREATE TABLE sync_doc_revisions (
    user_id       uuid        NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    domain        text        NOT NULL CHECK (length(domain) BETWEEN 1 AND 64),
    version       bigint      NOT NULL CHECK (version > 0),
    device_id     uuid,
    client_build  text        CHECK (client_build IS NULL OR length(client_build) <= 64),
    -- When this version was written; replaced_at = when a newer write archived it.
    updated_at    timestamptz NOT NULL,
    replaced_at   timestamptz NOT NULL DEFAULT now(),
    payload       bytea       NOT NULL,
    payload_bytes integer     NOT NULL CHECK (payload_bytes >= 0),
    PRIMARY KEY (user_id, domain, version)
);
