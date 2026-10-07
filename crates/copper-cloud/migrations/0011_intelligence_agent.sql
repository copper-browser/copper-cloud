-- Org-wide agent tool-call round budget (copper-cloud 0.7.0; see docs/admin-api.md).
--
-- `agent_max_turns`: how many rounds of tool calls Copper's agent pane may run for one
-- question, for everyone on the instance. NULL = no org value (each Copper uses its own
-- setting). Served by `GET /v1/intelligence` as `"agent": {"max_turns": N}` independently of
-- the key-sharing toggle; not secret, so plaintext. A row may now hold only this value (no
-- keys), still with a wrapped data key like every other row.
-- Additive only: existing rows and older clients are unaffected.

ALTER TABLE intelligence_settings
    ADD COLUMN agent_max_turns integer CHECK (agent_max_turns BETWEEN 1 AND 500);
