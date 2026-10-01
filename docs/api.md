# HTTP API

Base URL: `https://HOST:PORT` from the link code. All bodies are JSON (UTF-8). Timestamps are
RFC 3339 UTC strings. IDs are UUIDs (v7 for server-generated ones).

## Conventions

### Gate credential (every `/v1` request except `POST /v1/auth/pair`)

```
X-Copper-Instance: <instance key | access key>
```

The header carries **either** the shared instance key (accepted only while the instance's
access mode is `open`) **or** a personal access key (`ck_` + 43 base64url characters, minted
in the admin portal; accepted in both modes while not revoked and not expired). Missing or
not accepted → `401 {"error":"instance_key","message":"unauthorized"}`. This applies to every
`/v1` path, including unknown ones, so an unlinked client learns nothing about the server.

* `open` (default for instances created before access keys existed): anyone with the instance
  link code may connect; signup follows `allow_signup`.
* `directory` (default for fresh installs): only access keys pass; signup needs one.

The access mode is read through a 5-second cache: after an admin switches modes, Coppers see
the change within 5 s. Paths outside `/v1` are not gated: `/healthz`, the admin API
(`/admin/api/*`, see [admin-api.md](admin-api.md)) and the admin portal (every other path).

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
| 401 | `instance_key` / `session` / `credentials` / `account_disabled` / `pairing_code` | see above |
| 403 | `forbidden` | signup disabled; writing another device's `tabs:` doc |
| 403 | `access_key_required` / `access_key_email` / `access_key_exhausted` | signup in `directory` mode without an access key; key bound to another email; key's `max_uses` reached |
| 404 | `not_found` | unknown route / missing resource |
| 409 | `conflict` | email taken; stale `base_version` (body carries the server copy) |
| 413 | `payload_too_large` | body or payload over the configured limit |
| 429 | `rate_limited` | auth rate limit (header `Retry-After: 60`) |
| 500 | `internal` | server bug / database failure (details only in server logs) |

### Rate limit

`/v1/auth/*` (except `GET /v1/auth/me` and `/v1/auth/pairing[/{id}]`, which need a session)
is limited to `limits.auth_per_minute` (default 10) requests per minute per client IP (IPv6:
per /64), GCRA (bursts of 10, then one every 6 s). This includes the ungated
`POST /v1/auth/pair`. Admin login has its own bucket of the same size.
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
  "access_mode": "directory",
  "limits": { "max_blob_bytes": 8000000, "max_history_batch": 2000, "max_history_entry_bytes": 16384 }
}
```

`signup` is whether `POST /v1/auth/signup` will be accepted right now **for this gate
credential**: always `true` with an access key (an email-bound key still needs its email; an
exhausted key still gets `403 access_key_exhausted`); with the instance key, `allow_signup`
(always `true` while the instance has no users). `access_mode` is `"open"` or `"directory"`.

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
- With the **instance key** (open mode): allowed when `allow_signup` is effective (config,
  overridden by `admin enable-signup` / `disable-signup` or the portal) **or** the instance
  has no users yet. Otherwise `403 {"error":"forbidden"}`.
- With an **access key** (either mode): always allowed — the admin minted the key — but if
  the key carries an email, `email` must match it case-insensitively, else
  `403 {"error":"access_key_email"}`. Each signup counts one use of the key; when its
  `max_uses` is reached → `403 {"error":"access_key_exhausted"}` (the key keeps working for
  everything else, so `max_uses: 1` is a one-person invite). A failed signup (e.g. `409`)
  does not consume a use.
- In `directory` mode without an access key → `403 {"error":"access_key_required"}` (in
  practice the gate already answered `401 instance_key`).

`200`:

```json
{
  "token": "pR8…43 chars",
  "user":   { "id": "uuid", "email": "ada@example.com", "display_name": "Ada", "created_at": "2026-10-01T12:00:00Z" },
  "device": { "id": "uuid", "name": "Ada's MacBook", "created_at": "…", "last_seen_at": "…" }
}
```

Errors: `400` (email/password), `403` (`forbidden` / `access_key_*`, see above), `409` (email
exists), `429`.

### `POST /v1/auth/login`

```json
{ "email": "ada@example.com", "password": "…", "device": { "id": "uuid", "name": "Ada's iMac" } }
```

`200` same shape as signup (a new session for that device; the device row is created or its
name updated). Wrong email or password → `401 {"error":"credentials"}` (identical, constant
work). Disabled account → `401 {"error":"account_disabled"}`. Any accepted gate credential
works (access keys are not tied to an account for login).

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

## Pairing codes

A signed-in Copper mints a **single-use pairing code** that links *and* signs in another
Copper in one step (no email/password typing). Codes are `cp_` + 32 base64url characters
(24 random bytes), valid **10 minutes**, stored only as SHA-256.

### `POST /v1/auth/pairing` 🔒

Body optional: `{"device_name": "Ada's Mac mini"}` (used as the new device's name if it sends
none). At most 20 active codes per user (`400` beyond).

```json
{
  "id": "0192…",
  "code": "cp_Q2b…(32 chars)",
  "link": "copper-cloud://cloud.example.com:443/#p=cp_Q2b…&fp=9f86…",
  "device_name": "Ada's Mac mini",
  "created_at": "2026-10-01T12:00:00Z",
  "expires_at": "2026-10-01T12:10:00Z"
}
```

`code` is returned only here. `link` has the same host/port/`fp` as the link code, with
`p=<code>` instead of `k=<key>`; Copper accepts either the bare `code` or the `link`.

### `GET /v1/auth/pairing` 🔒

My active (unused, unexpired) codes, newest first — never the code itself:
`[{"id","device_name","created_by_device","created_at","expires_at"}]`.

### `DELETE /v1/auth/pairing/{id}` 🔒

Revokes one of my unused codes. `200 {"ok":true}`; unknown / used / not mine → `404`.

### `POST /v1/auth/pair` — **no gate header**

The code is the credential, so this is the one `/v1` route reachable without
`X-Copper-Instance` (rate limited like `/v1/auth/*`).

```json
{ "code": "cp_Q2b…", "device": { "id": "uuid", "name": "Ada's Mac mini" } }
```

`200`:

```json
{
  "token": "…43 chars",
  "user":   { "id": "uuid", "email": "ada@example.com", "display_name": "Ada", "created_at": "…" },
  "device": { "id": "uuid", "name": "Ada's Mac mini", "created_at": "…", "last_seen_at": "…" },
  "gate_key": "ck_… | <instance key>"
}
```

- The code is consumed atomically (a second use, even concurrent, gets `401`).
- `gate_key` is what this Copper must send in `X-Copper-Instance` from now on (store it as the
  link's key): in `open` mode the instance key; in `directory` mode a **fresh access key**
  minted for this user (label `"<device name> via pairing"`, bound to the user's email,
  visible and revocable in the portal).
- Unknown, used, expired or revoked code → `401 {"error":"pairing_code"}`; the account is
  disabled → `401 {"error":"account_disabled"}` (the code is not consumed).

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

# Pair a second Copper: mint a code on the first, redeem it (no gate header) on the second.
CODE=$(curl -sk -H "$H" -H "Authorization: Bearer $TOKEN" -X POST https://$HOST/v1/auth/pairing | jq -r .code)
curl -sk https://$HOST/v1/auth/pair -d "{\"code\":\"$CODE\",\"device\":{\"name\":\"second\"}}" | jq '{token, gate_key}'
```

(`-k` because the certificate is self-signed; pin it instead in real clients — see
[security.md](security.md#tls).)
