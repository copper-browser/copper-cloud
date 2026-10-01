-- Devices are client-generated ids (one per Copper install), scoped per user: the same
-- install may sign into different accounts on one instance.
CREATE TABLE devices (
    user_id      uuid        NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    id           uuid        NOT NULL,
    name         text        NOT NULL DEFAULT '' CHECK (length(name) <= 200),
    created_at   timestamptz NOT NULL DEFAULT now(),
    last_seen_at timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (user_id, id)
);
