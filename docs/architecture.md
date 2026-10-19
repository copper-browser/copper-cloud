# Architecture

```
┌──────────── Copper (macOS) ────────────┐   TLS (pinned)   ┌──────────── copper-cloud ────────────────────────┐
│ Settings › Cloud  (link, account)      │ ───────────────▶ │ axum on :443 (rustls: self-signed | ACME | off)  │
│ Sync engine: PUT docs, POST history,   │  X-Copper-       │  observe (span + metrics) → instance gate →      │
│   SSE /v1/sync/events → pull           │  Instance +      │  auth rate limit → routes                        │
│ Canvas host: WebSocket relay           │  Bearer token    │   /v1/auth /v1/devices /v1/sync /v1/intelligence │
└────────────────────────────────────────┘                  │   /v1/canvases /v1/invites /v1/mentions … ──┐    │
                                                            │     (canvas link: proxy, ws pipe, events)   │    │
                                                            │ Postgres (local or RDS)                     │    │
                                                            │ /metrics on 127.0.0.1:9464, JSON logs       │    │
                                                            └─────────────────────────────────────────────┼────┘
                                                              app key + X-Canvas-User, SSE events         │
                                                                                                          ▼
                                                            ┌──────────── Canvas Cloud (separate server) ──────┐
                                                            │ canvases REST + Yjs rooms, chat, mentions,       │
                                                            │ invites, share links; its own Postgres           │
                                                            └──────────────────────────────────────────────────┘
```

Canvases are served by a linked Canvas Cloud ([copper-browser/canvas-cloud](https://github.com/copper-browser/canvas-cloud));
copper-cloud proxies the canvas API to it as the signed-in user — see [canvas-link.md](canvas-link.md).

## Crates

| Crate | Role |
|---|---|
| `crates/copper-cloud` (bin + lib) | CLI (clap), `serve` bootstrap, graceful shutdown, `doctor`, `admin`, admin API (`/admin/api`), embedded admin portal (rust-embed of `portal/out`), migrations dir |
| `crates/core` (`copper-cloud-core`) | config, db pool + embedded migrations, crypto, access mode + access keys + gate, auth extractor + routes, pairing codes, sync docs/history/SSE, cloud-wide intelligence keys, events, TLS, link codes, observability, app composition |
| `portal/` | Next.js static export (the admin portal), built with bun into `portal/out` before cargo in CI/release |
| `crates/canvas-link` (`copper-cloud-canvas-link`) | the canvas link: settings (sealed app key), REST proxy + WebSocket pipe to Canvas Cloud, event bridge, user provisioning (batch, JIT, reconcile, outbox), `/join/{token}` landing page, `GET /v1/info` `canvas` field, admin/CLI/doctor helpers (see [canvas-link.md](canvas-link.md)) |
| `crates/canvas-migrate` (`copper-cloud-canvas-migrate`) | `copper-cloud canvas migrate`: reads the legacy canvas tables, streams them to Canvas Cloud's import, verifies by digest (see [canvas-migration.md](canvas-migration.md)) |

The canvas engine (`crates/canvas`, `yrs`) was removed in 0.9.0; it lives on in Canvas Cloud.

Stack: tokio, axum 0.8, axum-server (rustls, `ring` provider), rustls-acme, rcgen, sqlx 0.8
(Postgres, runtime queries — no DB needed at compile time), argon2, aes-gcm, hkdf, sha2,
governor, tracing (+ JSON), metrics + Prometheus exporter. `unsafe` is forbidden workspace-wide.

## Request path

`copper_cloud::build_app` = `copper_cloud_core::app_with(state, link.router(), root)` wrapped
in the link's `info_layer`, where `link = CanvasLink::new(state)`; `link.start()` spawns the
event bridge, the user reconcile loop and the outbox worker:

```
Router
├── GET /healthz                      (not gated)
├── /v1  = core::router() ⊕ link.router()        (fallback → 404 JSON)
│         link.router(): /canvases, /canvases/{*rest}, /people, /invites, /invites/{*rest},
│         /canvas-links/{*rest}, /mentions, /mentions/{*rest} (any method) → Canvas Cloud
│         POST /v1/auth/pair is the only /v1 route without the gate header
├── /admin/api = admin_api::router()   (cookie sessions + CSRF header; fallback → 404 JSON)
├── GET /join/{token}                  (share-link landing page; not gated)
└── fallback → portal::serve           (embedded portal/out; /v1x, /metrics… → 404 JSON)
layers (outermost first):
  info_layer          adds "canvas": {"linked": bool} to successful GET /v1/info responses
  observe::track      span {method, path, route, ip, user_id, status, latency_ms,
                      error, error_message (400s)}; http_* metrics
  instance_gate       /v1 only: X-Copper-Instance = instance key (open mode, constant-time)
                      or access key (SHA-256 lookup) → GateIdentity extension; else 401
                      instance_key. access_mode cached 5 s.
  auth_rate_limit     /v1/auth/* except GET /me + pairing management; /admin/api/login has
                      its own bucket. GCRA keyed by client IP (IPv6 /64)
```

Handlers authenticate with the `AuthUser` extractor (the canvas proxy uses it too): one SQL
round trip that verifies `sha256(token)`, checks expiry and `users.disabled`, and — at most
once a minute per session — slides `sessions.expires_at` and touches `devices.last_seen_at`
(data-modifying CTEs in the same statement). The result is cached in request extensions.
Sync handlers use a variant that also returns the unwrapped per-user data key.

The span's `user_id` is recorded by the extractor, so every log line from a handler carries
it. The query string is never logged (it can carry `?token=`).

## Data model (core tables)

| Table | Key | Notes |
|---|---|---|
| `users` | `id` | `lower(email)` unique, `password_hash` (Argon2id PHC), `data_key_wrapped` (60 B), `disabled` |
| `devices` | `(user_id, id)` | client-generated ids, `name`, `last_seen_at` |
| `sessions` | `id`; `token_sha256` unique | FK → devices (cascade), sliding `expires_at` |
| `sync_docs` | `(user_id, domain)` | `version` (LWW), `device_id` (last writer), `payload` sealed, `payload_bytes` |
| `history` | `seq` bigserial; index `(user_id, seq)` | `device_id`, `visited_at`, `payload` sealed |
| `server_settings` | `key` | runtime settings from the admin CLI / portal (`allow_signup`, `access_mode`) |
| `access_keys` | `id`; `key_sha256` unique | per-person gate credentials: `label`, `email?`, `expires_at?`, `revoked_at?`, `uses`/`max_uses?`, `last_used_at` |
| `pairing_codes` | `id`; `code_sha256` unique | single-use, 10-min codes: `user_id`, `device_name?`, `used_at`, `used_by_device` |
| `intelligence_settings` | `id` = 1 (singleton) | cloud-wide AI keys: `data_key_wrapped`, sealed `jev_key_sealed` / `router_key_sealed`, `jev_endpoint`, `jev_model`, `router_url`, `enabled`, `agent_max_turns` (org agent round budget, 1–500 or NULL), `updated_at/by` |
| `admins` | `id`; `lower(email)` unique | portal admin accounts (Argon2id), `last_login_at` |
| `admin_sessions` | `id`; `token_sha256` unique | `cc_admin` cookie sessions, fixed 7-day `expires_at` |
| `admin_audit` | `id` bigserial | every admin mutation: `admin_id`, `action`, `target`, `detail` jsonb, `at` |
| `canvas_link` | `id` = 1 (singleton) | the linked Canvas Cloud: `url`, sealed `app_key_sealed` + `data_key_wrapped`, `tls_fingerprint` (pin or NULL), `enabled`, `linked_at`, `updated_at/by` |
| `canvas_link_outbox` | `id` bigserial | queued provisioning: `kind` (`upsert_user` / `delete_user` / `disconnect_user`), `subject` (Copper user id), `attempts`, `next_attempt_at`, `last_error` |
| `canvas_migrations` | `canvas_id` | migration ledger: `status` (`imported` / `migrated` / `failed`), `digest`, `updates`, `migrated_at`, `detail` |

The three canvas-link tables come from `0030_canvas_link.sql`. The legacy canvas tables
(`0100`–`0103`: `canvases`, `canvas_members`, `canvas_invites`, `canvas_updates`,
`canvas_snapshots`, `canvas_share_links`, `canvas_mentions`) are still created — sqlx needs the
files on every existing deployment — but 0.9.0 only reads them, in `canvas migrate`. All migrations live in
`crates/copper-cloud/migrations` and are embedded once as `copper_cloud_core::db::MIGRATOR`;
`serve` applies pending migrations at startup (advisory-locked), `migrate` does it explicitly.
Every user-owned row cascades from `users`, so `admin delete-user` removes everything here; the
user's Canvas Cloud account and canvases are removed through the link (admin API: outbox at
once; shell CLI deletes: `copper-cloud canvas sync-users --prune`, never automatically).

## Sync semantics

- **Docs** are whole-document LWW with optimistic concurrency: a write names the version it
  is based on; the server applies it only if that is still current (`UPDATE … WHERE version =
  $base` / `INSERT … ON CONFLICT DO NOTHING` — single statement, no lost updates), otherwise
  returns `409` with the current server copy for the client to merge.
- `tabs:<device_id>` docs are writable only by that device's sessions.
- **History** is append-only for syncing; `seq` gives a total order per instance and is the pull
  cursor. Batches are inserted with one `INSERT … SELECT FROM UNNEST(…)`. The owner can delete
  rows (`DELETE /v1/sync/history`, 0.8.0) with one `DELETE` that selects by metadata only — a
  list of `seq`s the client picked from its own pull, or a `visited_at` window and/or device —
  and never opens a payload (the route does not even unwrap the data key). `seq` is never
  reused, so cursors survive deletes.
- **Events**: an in-process per-user `tokio::sync::broadcast` fan-out (`Events`). Doc writes
  publish `doc`, history appends publish `history`, history deletes publish `history_deleted`,
  the canvas link's event bridge publishes `canvas` (from Canvas Cloud's app event stream). SSE streams turn them into `event:` frames with a 15 s
  keepalive comment; a lagging stream gets `resync`. Single-process by design (one daemon per
  instance).

## Encryption at rest

`master_key` → HKDF-SHA256 → KEK. Each user gets a random 32-byte data key wrapped by the KEK.
Blobs are AES-256-GCM `nonce(12) ‖ ciphertext ‖ tag` with AAD `"<user_id>:<domain>"`
(history: `"<user_id>:history"`), so rows cannot be swapped between users or domains. See
[security.md](security.md).

## TLS

- `self-signed` (default): rcgen ECDSA P-256 certificate (SANs: public host — IP SAN when it
  is an IP — plus `localhost`/`127.0.0.1`, 10-year validity), created by `tls-init` or on first
  start. Clients pin the leaf's SHA-256 from the link code.
- `acme`: rustls-acme (Let's Encrypt, TLS-ALPN-01 on :443), cache in
  `/var/lib/copper-cloud/acme`; link code has no `fp`.
- `off`: plain HTTP for use behind a TLS-terminating proxy only.

HTTP/2 and HTTP/1.1 are negotiated via ALPN.

## Lifecycle

`serve`: load config → tracing → metrics recorder → connect Postgres (retries up to 60 s) →
migrate → state → spawn metrics listener + hourly expired-session purge → serve. SIGTERM /
SIGINT: trigger the process-wide shutdown signal (ends SSE streams; proxied canvas WebSockets
are closed with 1001 on both sides), stop accepting, drain in-flight requests for up to 10 s, close the pool.

## Performance notes

- One DB round trip per authenticated request for auth (+ the handler's query).
- Prepared statements are cached per connection by sqlx; pool size `db_max_connections`.
- Doc bodies are parsed borrowed (`Cow<str>` payload, no copy); history pages splice the
  stored entry JSON into the response without re-parsing.
- Argon2 runs on the blocking pool behind a semaphore (≤ 8 concurrent, 64 MiB each).
- Body sizes are bounded per route; bodies have a 60 s read deadline.
