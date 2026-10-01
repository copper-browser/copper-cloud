# HTTP API

Base URL: `https://HOST:PORT` from the link code. All bodies are JSON (UTF-8). Timestamps are
RFC 3339 UTC strings. IDs are UUIDs (v7 for server-generated ones).

## Conventions

### Instance key (every request except `/healthz`)

```
X-Copper-Instance: <instance_key>
```

Missing or wrong → `401 {"error":"instance_key","message":"unauthorized"}`. This applies to
every path, including unknown ones, so an unlinked client learns nothing about the server.

### Session

Endpoints marked 🔒 need a session token from signup/login:

```
Authorization: Bearer <token>
```

`?token=<token>` is accepted instead of the header (for WebSocket tooling). Tokens are 43
characters (base64url of 32 random bytes). Missing/unknown/expired →
`401 {"error":"session"}`. Sessions slide: each use (recorded at most once a minute) extends
expiry to 90 days after last use.

### Errors

Always `{"error": "<code>", "message": "<text>"}`:

| Status | `error` | When |
|---|---|---|
| 400 | `bad_request` | invalid JSON, field, domain, size of one entry, … |
| 401 | `instance_key` / `session` / `credentials` / `account_disabled` | see above |
| 403 | `forbidden` | signup disabled; writing another device's `tabs:` doc |
| 404 | `not_found` | unknown route / missing resource |
| 409 | `conflict` | email taken; stale `base_version` (body carries the server copy) |
| 413 | `payload_too_large` | body or payload over the configured limit |
| 429 | `rate_limited` | auth rate limit (header `Retry-After: 60`) |
| 500 | `internal` | server bug / database failure (details only in server logs) |

### Rate limit

`/v1/auth/*` (except `GET /v1/auth/me`) is limited to `limits.auth_per_minute` (default 10)
requests per minute per client IP (IPv6: per /64), GCRA (bursts of 10, then one every 6 s).
Behind a proxy set `trust_proxy = true` so the right-most `X-Forwarded-For` hop is used.

### Request size limits

| Body | Limit |
|---|---|
| auth / devices | 16 KiB |
| `PUT /v1/sync/docs/{domain}` | base64 of `limits.max_blob_bytes` (8 MB decoded) + 4 KiB |
| `POST /v1/sync/history` | `max_history_batch` × (`max_history_entry_bytes` + 8) (≤ 64 MiB) |

Bodies must arrive within 60 s. `Content-Type` is not required.

---

## Health

### `GET /healthz`

No key needed. `200 text/plain` `ok`. Liveness only (does not touch the database).

### `GET /v1/info`

Key only (no session). Lets a client validate a link code before showing account UI.

```json
{
  "name": "copper-cloud",
  "version": "0.1.0",
  "signup": true,
  "limits": { "max_blob_bytes": 8000000, "max_history_batch": 2000, "max_history_entry_bytes": 16384 }
}
```

`signup` is whether `POST /v1/auth/signup` will be accepted right now (always `true` while the
instance has no users).

---

## Auth

### `POST /v1/auth/signup`

```json
{
  "email": "ada@example.com",
  "password": "at least 10 characters",
  "display_name": "Ada",
  "device": { "id": "0192…uuid", "name": "Ada's MacBook" }
}
```

- `email`: unique per instance, case-insensitive (stored as typed, trimmed).
- `password`: ≥ 10 characters, ≤ 1024 bytes. Hashed with Argon2id (64 MiB, t=3, p=1).
- `display_name` optional (defaults to the email's local part); `device` optional (`id`
  defaults to a new UUID, `name` to `Copper`). Device ids are client-generated and scoped per
  user; reuse the same id for the same Copper install.
- Allowed when `allow_signup` is effective (config, overridden by `admin enable-signup` /
  `disable-signup`) **or** the instance has no users yet. Otherwise `403`.

`200`:

```json
{
  "token": "pR8…43 chars",
  "user":   { "id": "uuid", "email": "ada@example.com", "display_name": "Ada", "created_at": "2026-10-01T12:00:00Z" },
  "device": { "id": "uuid", "name": "Ada's MacBook", "created_at": "…", "last_seen_at": "…" }
}
```

Errors: `400` (email/password), `403` (signup disabled), `409` (email exists), `429`.

### `POST /v1/auth/login`

```json
{ "email": "ada@example.com", "password": "…", "device": { "id": "uuid", "name": "Ada's iMac" } }
```

`200` same shape as signup (a new session for that device; the device row is created or its
name updated). Wrong email or password → `401 {"error":"credentials"}` (identical, constant
work). Disabled account → `401 {"error":"account_disabled"}`.

### `POST /v1/auth/logout` 🔒

Body optional: `{"all": true}` revokes every session of the user, otherwise only the current
one. `200 {"ok": true, "revoked": 1}`.

### `GET /v1/auth/me` 🔒

```json
{
  "user":   { "id": "uuid", "email": "…", "display_name": "…", "created_at": "…" },
  "device": { "id": "uuid", "name": "…", "created_at": "…", "last_seen_at": "…" },
  "sync_cursor": { "history_seq": 1234 }
}
```

`history_seq` is the newest history sequence number for this user (0 if none) — a fresh
device can start pulling from 0, or from here to skip backfill.

### `POST /v1/auth/password` 🔒

```json
{ "old": "current password", "new": "new password ≥ 10 chars" }
```

`200 {"ok": true, "revoked_sessions": 2}` — every *other* session is revoked. Wrong `old` →
`401 {"error":"credentials"}`. Synced data is unaffected (data keys are wrapped by the server
master key, not the password).

---

## Devices 🔒

### `GET /v1/devices`

```json
[ { "id": "uuid", "name": "Ada's MacBook", "created_at": "…", "last_seen_at": "…", "current": true } ]
```

Most recently seen first.

### `PATCH /v1/devices/{id}`

`{"name": "New name"}` → `200` device object. `404` if not yours.

### `DELETE /v1/devices/{id}`

Revokes the device's sessions and deletes its `tabs:<id>` doc. `200 {"ok": true}`.

---

## Sync docs 🔒

Whole-document, last-writer-wins domains with optimistic concurrency:

| Domain | Contents (opaque to the server) | Writers |
|---|---|---|
| `spaces` | spaces + pinned/saved tabs | any device of the user |
| `settings` | safe settings subset | any device |
| `bookmarks` | bookmarks tree | any device |
| `tabs:<device_id>` | that device's open tabs | **only** that device (others read) |

Any other domain → `400`. `tabs:` ids are normalized to lower-case hyphenated form; `:` may be
percent-encoded (`tabs%3A…`). Payloads are bytes (normally UTF-8 JSON), base64-encoded in
transit, encrypted at rest with the user's data key.

### `GET /v1/sync/docs`

Metadata only:

```json
[ { "domain": "spaces", "version": 7, "updated_at": "…", "device_id": "uuid", "bytes": 5120 } ]
```

`device_id` = last writer; `bytes` = plaintext size. Sorted by domain.

### `GET /v1/sync/docs/{domain}`

```json
{ "domain": "spaces", "version": 7, "updated_at": "…", "device_id": "uuid", "payload": "eyJzcGFjZXMiOltdfQ==" }
```

`payload` is standard base64. `404` if the doc was never written.

### `PUT /v1/sync/docs/{domain}`

```json
{ "base_version": 7, "payload": "<base64>" }
```

- `base_version` = the version this write is based on; `0` means "create".
- Success: `200 {"version": 8, "updated_at": "…"}` and an SSE `doc` event to all of the
  user's streams.
- Stale base (someone else wrote first, or `0` for an existing doc, or non-zero for a missing
  one): `409` with the server copy so the client can merge and retry with its `version`:

```json
{ "error": "conflict", "message": "conflict", "domain": "spaces", "version": 8,
  "updated_at": "…", "device_id": "uuid", "payload": "<base64>" }
```

  (missing doc: `"version": 0, "payload": null`).
- `403` writing another device's `tabs:` domain; `413` decoded payload > `max_blob_bytes`;
  `400` payload not base64.

Payload base64 may be standard or url-safe, padded or not.

---

## History 🔒

Append-only. Each entry is a client-defined JSON object (e.g. `url`, `title`, `visited_at`,
`transition`); the server stores it encrypted and returns it verbatim.

### `POST /v1/sync/history`

```json
{ "entries": [ { "url": "https://example.com", "title": "Example", "visited_at": "2026-10-01T12:00:00Z" } ] }
```

- ≤ `max_history_batch` (2000) entries, each a JSON object ≤ `max_history_entry_bytes`
  (16 KiB) → else `400`.
- `visited_at`: RFC 3339 string, or Unix seconds (numbers > 1e11 are read as milliseconds);
  missing → now. Indexed for ordering/retention only.
- `200 {"seq": 1240, "inserted": 3}` — `seq` is the highest sequence number assigned. An SSE
  `history` event follows. Empty `entries` → `{"seq": <current max>, "inserted": 0}`.

### `GET /v1/sync/history?since=<seq>&limit=<n>&exclude_device=me`

- `since` (default 0): return entries with `seq > since`, ascending.
- `limit` default 500, clamped to `1..=max_history_batch`.
- `exclude_device`: `me` (the calling device) or a device UUID — skip entries it authored.
  Clients normally pass `me`.

```json
{
  "entries": [
    { "seq": 1238, "device_id": "uuid", "visited_at": "2026-10-01T12:00:00Z",
      "payload": { "url": "https://example.com", "title": "Example", "visited_at": "2026-10-01T12:00:00Z" } }
  ],
  "next": 1240,
  "more": false
}
```

Loop with `since = next` while `more` is `true`; store `next` as the cursor.

---

## Events (SSE) 🔒

### `GET /v1/sync/events`

`text/event-stream`, one stream per device; events for the authenticated user only.

```
event: ready
data: {"device_id":"…"}

event: doc
data: {"type":"doc","domain":"spaces","version":8,"device_id":"…"}

event: history
data: {"type":"history","seq":1240}

event: canvas
data: {"type":"canvas","canvas_id":"…","kind":"created"}

event: resync
data: {"skipped":12}

: keepalive
```

- `doc.device_id` is the writer — ignore your own writes.
- `resync`: this stream fell behind and dropped events; do a full pull
  (`GET /v1/sync/docs` + history from your cursor).
- A `: keepalive` comment every 15 s. The stream ends on server shutdown, when the session is
  revoked (checked every 5 min), or on network loss — reconnect with backoff and re-pull.
- Events carry no payloads; fetch the doc/history after an event.

---

## Canvases 🔒

Mounted under the same `/v1` gate and session auth; documented in
[canvas.md](canvas.md):

`GET/POST /v1/canvases`, `GET/PATCH/DELETE /v1/canvases/{id}`, `GET /v1/canvases/{id}/members`,
`DELETE /v1/canvases/{id}/members/{user_id}`, `GET/POST /v1/canvases/{id}/invites`,
`GET /v1/invites`, `POST /v1/invites/{id}/accept|decline`, `GET /v1/canvases/{id}/ws`
(WebSocket, Yjs sync protocol), `GET /v1/canvases/{id}/state`, `GET /v1/canvases/{id}/read`,
`POST /v1/canvases/{id}/ops`.

---

## Example (curl)

```sh
H="X-Copper-Instance: $KEY"
curl -sk -H "$H" https://$HOST/v1/info
TOKEN=$(curl -sk -H "$H" https://$HOST/v1/auth/signup \
  -d '{"email":"ada@example.com","password":"correct horse battery","device":{"name":"curl"}}' | jq -r .token)
curl -sk -H "$H" -H "Authorization: Bearer $TOKEN" -X PUT https://$HOST/v1/sync/docs/settings \
  -d "{\"base_version\":0,\"payload\":\"$(printf '{"theme":"dark"}' | base64)\"}"
curl -skN -H "$H" -H "Authorization: Bearer $TOKEN" https://$HOST/v1/sync/events
```

(`-k` because the certificate is self-signed; pin it instead in real clients — see
[security.md](security.md#tls).)
