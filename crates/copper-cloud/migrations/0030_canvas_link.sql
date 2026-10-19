-- Canvas link (copper-cloud ≥ 0.9): canvases live on a linked Canvas Cloud server; copper-cloud
-- proxies the canvas API there as the signed-in user (crate `copper-cloud-canvas-link`).
-- The legacy canvas tables (0100–0103) stay untouched: `copper-cloud canvas migrate` reads them,
-- and an older copper-cloud can be reinstalled against them (rollback).

-- The link (singleton row, id = 1). The Canvas Cloud app key is envelope-encrypted like the
-- intelligence keys: `data_key_wrapped` is a random 32-byte key wrapped by the master-key KEK;
-- `app_key_sealed` is AES-256-GCM under it with AAD 'canvas_link:app_key'.
--   url             base URL, no trailing '/' (https://…, or http:// for a loopback host)
--   tls_fingerprint lower-case hex SHA-256 of the pinned leaf certificate (self-signed
--                   Canvas Cloud); NULL = verify against the web PKI
--   enabled         false = keep the settings but serve 503 canvas_unavailable
--   linked_at       first link to this URL (kept across key rotations)
--   updated_by      admin email, or 'cli'
CREATE TABLE canvas_link (
    id               int         PRIMARY KEY DEFAULT 1 CHECK (id = 1),
    url              text        NOT NULL CHECK (length(url) BETWEEN 8 AND 2048),
    app_key_sealed   bytea       NOT NULL,
    data_key_wrapped bytea       NOT NULL,
    tls_fingerprint  text        CHECK (tls_fingerprint ~ '^[0-9a-f]{64}$'),
    enabled          boolean     NOT NULL DEFAULT true,
    linked_at        timestamptz NOT NULL DEFAULT now(),
    updated_at       timestamptz NOT NULL DEFAULT now(),
    updated_by       text
);

-- User provisioning work that has to reach Canvas Cloud (pushed right away; kept and retried
-- with backoff while Canvas Cloud is unreachable). Dropped when the link is removed.
--   kind            'upsert_user' | 'delete_user' | 'disconnect_user'
--   subject         the Copper user id (= Canvas Cloud subject and user id)
--   next_attempt_at (added column) when the item is due again
CREATE TABLE canvas_link_outbox (
    id              bigserial   PRIMARY KEY,
    kind            text        NOT NULL CHECK (kind IN ('upsert_user', 'delete_user', 'disconnect_user')),
    subject         uuid        NOT NULL,
    payload         jsonb,
    created_at      timestamptz NOT NULL DEFAULT now(),
    attempts        int         NOT NULL DEFAULT 0,
    last_error      text,
    next_attempt_at timestamptz NOT NULL DEFAULT now()
);

CREATE INDEX canvas_link_outbox_due_idx ON canvas_link_outbox (next_attempt_at, id);

-- Migration ledger of `copper-cloud canvas migrate` (idempotent re-runs, rollback evidence).
--   status  'migrated' | 'failed'
--   digest  canonical document digest reported by Canvas Cloud (CONTRACT §6.3)
--   updates number of legacy updates (incl. the snapshot) sent
--   detail  counts / error context
-- IF NOT EXISTS: the migrate crate's tests may have created it ahead of this migration.
CREATE TABLE IF NOT EXISTS canvas_migrations (
    canvas_id   uuid        PRIMARY KEY,
    status      text        NOT NULL,
    digest      text,
    updates     int,
    migrated_at timestamptz DEFAULT now(),
    detail      jsonb
);
