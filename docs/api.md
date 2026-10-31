# HTTP API

Base URL: `https://HOST:PORT` from the link code. All bodies are JSON (UTF-8). Timestamps are
RFC 3339 UTC strings. IDs are UUIDs (v7 for server-generated ones).

## Conventions

### Gate credential (every `/v1` request except `POST /v1/auth/pair`, the SSO routes, `POST /v1/agents/pair` and `/v1/agent/*`)

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
| 400 | `bad_request` | invalid JSON, field, domain, too many history entries, … |
| 400 | `bad_redirect` | SSO sign-in with a non-loopback `redirect_uri` |
| 401 | `instance_key` / `session` / `credentials` / `account_disabled` / `pairing_code` | see above |
| 401 | `sso_only` | password login for an account that only signs in with SSO |
| 401 | `sso_failed` / `agent_key` | SSO code exchange or ID-token check failed; bad agent-integration key |
| 401 | `agent_token` | `/v1/agent/*`: missing, malformed, unknown or revoked `cat_` token ([device agent relay](#device-agent-relay)) |
| 403 | `forbidden` | signup disabled; writing another device's `tabs:` doc; opening or feeding another device's link frames |
| 403 | `password_signin_disabled` | password signup/login turned off by the admin (use SSO) |
| 403 | `sso_domain` / `sso_no_account` / `agent_domain` / `account_disabled` | email domain not allowed; no account and auto-create off; disabled account (SSO, agent pairing) |
| 404 | `sso_disabled` / `no_account` | SSO provider not set up / disabled; agent pairing for a missing account with `create_users` off |
| 403 | `access_key_required` / `access_key_email` / `access_key_exhausted` | signup in `directory` mode without an access key; key bound to another email; key's `max_uses` reached |
| 404 | `not_found` | unknown route / missing resource |
| 404 | `link_not_found` | unknown, revoked, or deleted-canvas share link |
| 409 | `conflict` | email taken; stale `base_version` (body carries the server copy) |
| 413 | `payload_too_large` | body or payload over the configured limit |
| 405 | `method_not_allowed` | `GET` / `DELETE /v1/agent/mcp` (no event stream, no session) |
| 429 | `rate_limited` | auth rate limit, a per-user limit (mentions, history delete) or the per-token agent limit (header `Retry-After: 60`) |
| 500 | `internal` | server bug / database failure (details only in server logs) |
| 502 | `canvas_upstream` | canvas routes only: the linked Canvas Cloud is unreachable, timed out, or rejected the app key ([canvas-link.md](canvas-link.md#status-codes)) |
| 503 | `canvas_unavailable` | canvas routes only: no Canvas Cloud is linked |

Canvas routes otherwise return Canvas Cloud's status and body verbatim (except `401`, which
is never forwarded).

### Rate limit

`/v1/auth/*` (except `GET /v1/auth/me` and `/v1/auth/pairing[/{id}]`, which need a session,
and the public `GET /v1/auth/sso`) is limited to `limits.auth_per_minute` (default 10) requests
per minute per client IP (IPv6: per /64), GCRA (bursts of 10, then one every 6 s). This
includes the ungated `POST /v1/auth/pair` and `POST /v1/auth/sso/{provider_id}`.
`POST /v1/agents/pair` is not limited (its 256-bit key is the credential). Admin login has its own bucket of the same size.
Behind a proxy set `trust_proxy = true` so the right-most `X-Forwarded-For` hop is used.
`POST /v1/canvases/{id}/mentions` has its own per-user bucket (60 per minute, bursts of 60), and
`DELETE /v1/sync/history` another (20 per minute, bursts of 20; requests rejected with `400` or
`413` do not count). `/v1/agent/*` is limited per agent token: 600 requests per minute (bursts of
600, then one every 100 ms).

### Request size limits

| Body | Limit |
|---|---|
| auth / devices | 16 KiB |
| `PUT /v1/sync/docs/{domain}` | base64 of `limits.max_blob_bytes` (8 MB decoded) + 4 KiB |
| `POST /v1/sync/history` | `max_history_batch` × (`max_history_entry_bytes` + 8) (≤ 64 MiB) |
| `DELETE /v1/sync/history` | 256 KiB (room for 5000 `seqs`) |
| `POST /v1/me/links` | 1 MiB (the `tools` manifest itself: 256 KiB) |
| `POST /v1/me/links/{id}/frames` | 32 MiB (replies may carry screenshots) |
| `POST /v1/agent/mcp` | 4 MiB |

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
  "version": "0.10.0",
  "signup": true,
  "access_mode": "directory",
  "features": ["history_delete", "sso", "agent_pairing", "canvas_link"],
  "limits": { "max_blob_bytes": 8000000, "max_history_batch": 2000, "max_history_entry_bytes": 16384 },
  "canvas": { "linked": true }
}
```

`signup` is whether `POST /v1/auth/signup` will be accepted right now **for this gate
credential**: always `true` with an access key (an email-bound key still needs its email; an
exhausted key still gets `403 access_key_exhausted`); with the instance key, `allow_signup`
(always `true` while the instance has no users). `access_mode` is `"open"` or `"directory"`.

`features` (0.8.0+) lists optional capabilities so a client can show or hide controls; it
only ever grows, and an absent field (older servers) means none of them:

| Feature | Since | Means |
|---|---|---|
| `history_delete` | 0.8.0 | [`DELETE /v1/sync/history`](#delete-v1synchistory) and the `history_deleted` event |
| `sso` | 0.9.0 | [`GET /v1/auth/sso`](#get-v1authsso--no-gate-header) and [`POST /v1/auth/sso/{provider_id}`](#post-v1authssoprovider_id--no-gate-header) |
| `agent_pairing` | 0.9.0 | [`POST /v1/agents/pair`](#post-v1agentspair--no-gate-header) and device-bound pairing codes |
| `addons` | 0.10.0 | [`GET /v1/addons`](#addons) and the `addons` event |
| `canvas_link` | 0.11.0 | canvases are served by a linked Canvas Cloud through this server ([canvas-link.md](canvas-link.md)); the canvas API itself is unchanged |
| `doc_revisions` | 0.12.0 | [doc revisions](#doc-revisions-features-doc_revisions) and the optional `X-Copper-Build` header |
| `device_agent` | 0.13.0 | the [device agent relay](#device-agent-relay): `/v1/me/links` (+ frames), `/v1/me/agent-tokens`, and the ungated `GET /v1/agent/status` + `POST /v1/agent/mcp` |

`canvas` (0.11.0+): `linked` is `true` while a Canvas Cloud is linked and enabled. When it is
`false`, every canvas route answers `503 canvas_unavailable`. It says nothing about
reachability (that shows up as `502 canvas_upstream`).

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
- `403 {"error":"password_signin_disabled"}` while the admin has turned password sign-in off
  (single sign-on only).
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
work). Disabled account → `401 {"error":"account_disabled"}`. An account created by single
sign-on has no password → `401 {"error":"sso_only"}` (the message says to use SSO). Password
sign-in turned off by the admin → `403 {"error":"password_signin_disabled"}`. Any accepted gate credential
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
`401 {"error":"credentials"}` (also for an SSO-only account, which has no password). Synced
data is unaffected (data keys are wrapped by the server
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
- A code minted by [`POST /v1/agents/pair`](#post-v1agentspair--no-gate-header) is **bound to
  a device**: redeeming it signs in that device id and name, whatever the request says (the
  response's `device.id` is the one to keep).

---

## Single sign-on

Company sign-in with Google or an OpenID Connect provider (0.9.0+, feature `sso`; older servers
answer `404` → hide SSO). Native-app flow: authorization code + PKCE S256 with a loopback
redirect (RFC 8252); the server holds the client secret and does the code exchange. Setup and
the trust model: [sso.md](sso.md).

### `GET /v1/auth/sso` — **no gate header**

No credentials; not rate limited. Enabled, usable providers only:

```json
{
  "enabled": true,
  "password_signin": true,
  "providers": [{
    "id": "google", "kind": "google", "name": "Google",
    "client_id": "123-abc.apps.googleusercontent.com",
    "authorization_endpoint": "https://accounts.google.com/o/oauth2/v2/auth",
    "scopes": ["openid", "email", "profile"],
    "domains": ["example.com"],
    "hd": "example.com"
  }]
}
```

Off: `{"enabled": false, "password_signin": true, "providers": []}`. `kind` is `google` or
`oidc` (the `authorization_endpoint` comes from the issuer's discovery document; an OIDC
provider whose discovery fails is left out). `hd` is set only for Google with exactly one
domain (pass it as the `hd` authorization parameter, with `prompt=select_account`).
`password_signin: false` means password signup/login is off: show only SSO.

### `POST /v1/auth/sso/{provider_id}` — **no gate header**

Rate limited like `/v1/auth/*`.

```json
{
  "code": "4/0Ab…",
  "code_verifier": "43-128 chars (RFC 7636)",
  "redirect_uri": "http://127.0.0.1:53682/",
  "device": { "id": "uuid", "name": "Ada's MacBook" }
}
```

- `redirect_uri` must be the loopback URI the authorization request used:
  `http://127.0.0.1:<port>/…`, `http://[::1]:<port>/…` or `http://localhost:<port>/…` (no query
  or fragment). Anything else → `400 bad_redirect`.
- The server exchanges `code` + `code_verifier` (+ the client secret) at the provider's token
  endpoint, then checks the ID token: `iss`, `aud`/`azp` = client id, `exp`,
  `email_verified`, email domain ∈ `domains`; Google: `hd` ∈ `domains` too.
- Account: the linked identity (provider, `sub`), else the account with that email (the
  identity is linked to it), else a new password-less account when the provider's
  `auto_create` is on (display name = `name` claim or the email's local part).
- Then exactly what pairing does: device upsert, new session, gate key (`directory` mode: a
  fresh access key labelled `"<device name> via sso"`; `open` mode: the instance key).

`200`: the [`POST /v1/auth/pair`](#post-v1authpair--no-gate-header) response plus `created`
(a new account was made):

```json
{
  "token": "…", "user": { "id", "email", "display_name", "created_at" },
  "device": { "id", "name", "created_at", "last_seen_at" },
  "gate_key": "ck_… | <instance key>", "created": true
}
```

| Status | `error` | When |
|---|---|---|
| 400 | `bad_redirect` | not a loopback redirect URI |
| 400 | `bad_request` | missing `code`, `code_verifier` not 43-128 unreserved characters |
| 401 | `sso_failed` | the provider rejected the code (`invalid_grant`, `invalid_client` in the message), unreachable provider, bad/expired ID token, unverified email |
| 403 | `sso_domain` | email (or Google `hd`) not in the allowed domains; the message names them ("use your @example.com account") |
| 403 | `sso_no_account` | no account with that email and `auto_create` is off |
| 403 | `account_disabled` | the account is disabled |
| 404 | `sso_disabled` | unknown or disabled provider (or Google without a client secret) |

---

## Agent pairing

### `POST /v1/agents/pair` — **no gate header**

For a trusted service holding an agent-integration key (minted by an admin, see
[sso.md](sso.md#agent-integrations)) to sign a headless Copper in to a user's account.

```
Authorization: Bearer cai_<43 chars>
```

```json
{
  "email": "ada@example.com",
  "device": { "id": "<stable uuid>", "name": "Bot · Vera" },
  "display_name": "Ada Lovelace"
}
```

- The integration must not be revoked; `email`'s domain must be in its `domains` (empty = the
  enabled SSO providers' domains; neither → nobody).
- User by email (case-insensitive); missing → created without a password when the integration
  has `create_users` (display name = `display_name` or the email's local part), else
  `404 no_account`.
- `device.id` is required. The device is upserted and flagged as an agent device of the
  integration. Re-pairing the same device id deletes its previous sessions, revokes the gate
  keys minted for it and drops its unused codes.
- Mints a single-use 10-minute pairing code **bound to that device id and name**. Redeem it
  with [`POST /v1/auth/pair`](#post-v1authpair--no-gate-header).

`200`:

```json
{
  "code": "cp_…",
  "link": "copper-cloud://cloud.example.com:443/#p=cp_…&fp=9f86…",
  "expires_at": "2026-10-01T12:10:00Z",
  "user": { "id": "uuid", "email": "ada@example.com", "display_name": "Ada Lovelace" },
  "device": { "id": "uuid", "name": "Bot · Vera" },
  "created_user": false
}
```

Errors: `401 agent_key` (missing, malformed, unknown, rotated or revoked key), `403
agent_domain` (the message names the allowed domains), `404 no_account`, `403
account_disabled`, `400 bad_request` (email, missing `device.id`). Every pairing is audited
(`agent.pair`).

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

Revokes the device's sessions, deletes its `tabs:<id>` doc, and turns off its
[device agent](#device-agent-relay) link (its stream closes with `revoked`) and agent tokens.
`200 {"ok": true}`.

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

Optional request header `X-Copper-Build: <client build>` (printable ASCII, ≤ 64 chars; anything
else is ignored) is stored with the write and shown in its revision (below). Older clients just
leave it out.

### Doc revisions (`features`: `doc_revisions`)

The server cannot read a doc, so it keeps what each write replaced: every accepted overwrite
saves the old version (still encrypted) and the newest 20 per domain are kept. Use them to
recover from a bad write by any device.

- `GET /v1/sync/docs/{domain}/revisions` → newest first, metadata only:

```json
[ { "version": 6, "device_id": "uuid", "client_build": "1.0.20261028.3", "updated_at": "…",
    "replaced_at": "…", "bytes": 5120 } ]
```

  `updated_at` = when that version was written, `replaced_at` = when a newer write replaced
  it, `client_build` = its `X-Copper-Build` (or `null`).
- `GET /v1/sync/docs/{domain}/revisions/{version}` → the revision in the
  `GET /v1/sync/docs/{domain}` shape (with `payload`); `404` if not kept.
- `POST /v1/sync/docs/{domain}/revisions/{version}/restore` → writes it back as a new version
  (the current one becomes a revision too): `200 {"version": 9, "updated_at": "…",
  "restored_from": 6}` and an SSE `doc` event with `device_id: null`, so every device, the
  caller's included, pulls it. `404` unknown revision; `403` another device's `tabs:` domain.

Removing a device deletes its `tabs:` doc and that doc's revisions. Operators:
`copper-cloud admin docs revisions --user EMAIL --domain spaces` and
`copper-cloud admin docs restore --user EMAIL --domain spaces --version N` (no event; Coppers
pick it up on their next pull).

---

## History 🔒

Append-only for syncing; the owner can delete rows (`DELETE`, 0.8.0). Each entry is a
client-defined JSON object (e.g. `url`, `title`, `visited_at`, `transition`); the server stores
it encrypted and returns it verbatim.

### `POST /v1/sync/history`

```json
{ "entries": [ { "url": "https://example.com", "title": "Example", "visited_at": "2026-10-01T12:00:00Z" } ] }
```

- Each entry: a JSON object of at most `max_history_entry_bytes` (16 KiB, as serialized in
  the request).
- `visited_at`: RFC 3339 string, or Unix seconds (numbers > 1e11 are read as milliseconds);
  missing → now. Indexed for ordering/retention only.
- `200 {"seq": 1240, "inserted": 3, "rejected": []}` — `seq` is the highest sequence number
  assigned. An SSE `history` event follows when anything was inserted. Empty `entries` (or
  nothing valid) → `seq` is the current max and `inserted` is `0`.
- **A bad entry never fails the batch** (since 0.5.0). It is skipped, the valid entries are
  stored, and the response lists what was skipped, by position in `entries`:

  ```json
  { "seq": 1240, "inserted": 2,
    "rejected": [ { "index": 1, "reason": "too_large", "message": "entry is 20480 bytes; the limit is 16384" } ] }
  ```

  | `reason` | Entry problem |
  |---|---|
  | `too_large` | over `max_history_entry_bytes` |
  | `not_object` | not a JSON object |
  | `invalid` | an object serde cannot read (e.g. a duplicate `visited_at`) |
  | `bad_visited_at` | `visited_at` neither RFC 3339 nor a Unix timestamp |

  Skipped entries are not stored; a client should treat the batch as sent and move on (a
  client that resends the same batch after a failure therefore never wedges on one entry).
  Invalid UTF-8 inside an entry is replaced with U+FFFD rather than failing the request.
- Whole-request problems stay errors, with a message saying which: `400` for a body that is
  not JSON, not an object with an `entries` array, or has more than `max_history_batch`
  (2000) entries; `413` for a body over the cap above.

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

Loop with `since = next` while `more` is `true`; store `next` as the cursor. Deleted rows are
simply absent: sequence numbers are never reused or renumbered, so a stored cursor stays valid
after any delete (the next page just has fewer rows, and `next` never moves backwards).

### `DELETE /v1/sync/history`

Deletes the caller's own synced history. **The server never looks inside history to decide what
to delete**: it selects rows only by metadata it stores in the clear (`seq`, `visited_at`, the
pushing device), never by url, host, title or any other payload content. Picking individual
entries — "forget this site", "remove these visits" — is the user's choice and is done on the
client: pull the history (every `GET` row carries its `seq`), filter locally, and send the
chosen seqs.

Exactly one of three forms:

| Form | Request | Deletes |
|---|---|---|
| By seq | body `{"seqs": [<seq>, …]}` (≤ 5000), no query parameters | exactly those rows, where they are the caller's; other users' or unknown seqs are silently ignored |
| By metadata | no body; any of `since`, `until`, `device` in the query | rows matching **every** parameter given |
| Everything | no body, no parameters | all of the caller's history, from every device |

| Query parameter | Value | Selects |
|---|---|---|
| `since` | RFC 3339 | visits with `visited_at >= since` |
| `until` | RFC 3339 | visits with `visited_at < until` |
| `device` | device UUID | rows pushed by that device |

```http
DELETE /v1/sync/history
Content-Type: application/json

{ "seqs": [1201, 1202, 1240] }
```

```http
DELETE /v1/sync/history?since=2026-10-01T00:00:00Z&until=2026-10-02T00:00:00Z
```

- Other users' rows are never touched, whatever the form.
- `seqs` may be in any order and contain duplicates; `{"seqs": []}` deletes nothing (it never
  means "everything"). An empty or whitespace-only body counts as no body.
- Encode `+` in an offset as `%2B` (`2026-10-01T14:00:00%2B02:00`) or use `Z`; an unencoded
  `+` (decoded as a space) is accepted too.
- One SQL statement; a delete that has started finishes even if the client disconnects, and
  repeating it is harmless.

`200`:

```json
{ "deleted": 12 }
```

Then every stream of the user (the caller's too) gets a `history_deleted` event, even when
`deleted` is `0` (see [Events](#events-sse-)).

Errors (all `{"error": "bad_request", "message": …}` unless noted):

- `400` — more than 5000 `seqs`; `seqs` **and** `since`/`until`/`device` together (ambiguous);
  a body that is not exactly `{"seqs": [<integer>, …]}` (`{}`, `null`, `{"seqs": null}`, an
  unknown field, a non-integer seq); an unparsable `since`/`until`/`device`; `since` not
  earlier than `until`; a parameter given twice; or **any other query parameter** (`host`,
  `url`, …), so a typo never widens a delete to everything (`token` — the `?token=` session form
  — is allowed).
- `413 payload_too_large` — body over 256 KiB.
- `401` without a session; `429 rate_limited` beyond 20 deletes per minute per user.

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

event: history_deleted
data: {"type":"history_deleted","deleted":12,"since":"2026-10-01T00:00:00Z","until":null,"device":null,"device_id":"…"}

event: history_deleted
data: {"type":"history_deleted","deleted":3,"since":null,"until":null,"device":null,"seqs":3,"device_id":"…"}

event: canvas
data: {"type":"canvas","canvas_id":"…","kind":"created"}

event: addons
data: {"type":"addons"}

event: resync
data: {"skipped":12}

: keepalive
```

- `doc.device_id` is the writer — ignore your own writes.
- `history_deleted` (0.8.0) follows every successful `DELETE /v1/sync/history`. Metadata only —
  never urls, hosts or other history content. `deleted` = rows removed on the server;
  `device_id` = the device that asked. A delete by metadata echoes its filter so other devices
  can apply it to their local history: `since` / `until` (RFC 3339 UTC, `null` = unbounded) and
  `device` (`null` = every device); `seqs` is omitted. A delete by seq has `since` / `until` /
  `device` `null` and `seqs` = how many distinct seqs were asked for. Deletes made with the
  admin CLI send no event.
- `addons` goes to **every** connected user whenever an admin changes any addon (upload,
  on/off, audience, settings, priority, version, delete): refetch
  [`GET /v1/addons`](#addons) with `If-None-Match` (cheap when nothing changed for you).
  Changes made with the server CLI send no event.
- `resync`: this stream fell behind and dropped events (`{"skipped":n}`), or a linked Canvas
  Cloud's events may have been missed (`{"type":"resync","reason":"canvas"}`, 0.11.0, sent to
  every connected user, then followed by `event: canvas` `{"type":"canvas","kind":"resync"}`
  without a `canvas_id`); do a full pull (`GET /v1/sync/docs` + history from your cursor) and
  refetch canvases.
- A `: keepalive` comment every 15 s. The stream ends on server shutdown, when the session is
  revoked (checked every 5 min), or on network loss — reconnect with backoff and re-pull.
- Events carry no payloads; fetch the doc/history after an event. `canvas` kinds are listed in
  [canvas-link.md](canvas-link.md#event-bridge) (0.6.0 adds `mention` and `mention_read`: refetch
  `GET /v1/mentions`). Since 0.11.0 they are bridged from the linked Canvas Cloud; frames are unchanged.

---

## Intelligence keys 🔒

### `GET /v1/intelligence`

The instance's cloud-wide AI keys, set once by the admin (portal › AI keys,
`PUT /admin/api/intelligence`, or `copper-cloud intelligence set`), so no Copper user pastes a
LiteLLM or Jev key. Same gate and session as every other `/v1` user endpoint (gate credential
+ `Authorization: Bearer`); `Cache-Control: no-store`.

```json
// 200 — both keys set
{
  "jev": {
    "key": "<Jev (TypeSafe) API key>",
    "endpoint": "https://api.typesafe.ai/v1/systemone",
    "model": "jev-latest"
  },
  "router": {
    "key": "<LiteLLM virtual key>",
    "url": "https://llm.example.com"
  },
  "updated_at": "2026-10-05T20:45:00Z",
  "agent": { "max_turns": 100 }
}
// 200 — no keys set, or the admin turned sharing off (the agent budget is still served)
{ "jev": null, "router": null, "updated_at": null, "agent": { "max_turns": 100 } }
// 200 — nothing set at all
{ "jev": null, "router": null, "updated_at": null, "agent": null }
```

- Exactly these four fields. Each block is an object or `null` independently (an instance
  may share only a router key). `updated_at` (RFC 3339) changes whenever the admin changes
  anything; clients can use it to notice rotation. It is `null` exactly when both blocks are.
- `router.url` is the gateway base URL without a trailing `/` (OpenAI-compatible:
  `POST {url}/v1/chat/completions` with `Authorization: Bearer {key}`).
- `agent` (copper-cloud 0.7.0+) is the org-wide budget of tool-call rounds per question for
  Copper's agent pane: `{"max_turns": N}` with N an integer in 1–500, or `null` when the org
  sets none. It is **independent of key sharing** — present even when `jev`/`router` are
  `null`. `null` (or the field absent, from older servers) ⇒ use the local setting; a value
  wins over the local setting while present.
- Re-fetch at launch and periodically; when a block turns `null`, stop using the key you had.
- Reads are recorded in the admin audit log (`intelligence.read`, at most once per user per
  hour). Keys never appear in server logs.

---

## Addons 🔒

Web bundles that replace a slot of the Copper window (concepts, manifest, bundle format:
[addons.md](addons.md)). Same gate and session as every other `/v1` user endpoint. A server
without addons answers `404` here: treat that as "no addons".

### `GET /v1/addons`

What the caller gets: per slot at most one addon (enabled, audience `everyone` or listing the
caller's email, highest `priority`, then most recently changed).

```json
// 200, ETag: "r_3f9a0c1d2e4b5a69", Cache-Control: no-store
{
  "revision": "r_3f9a0c1d2e4b5a69",
  "addons": [
    {
      "id": "com.example.notes-sidebar",
      "slot": "agent-sidebar",
      "name": "Notes sidebar",
      "version": "1.0.0",
      "sha256": "<64 hex: SHA-256 of the served bundle>",
      "size": 123456,
      "entry": "index.html",
      "bridge": 1,
      "platforms": ["mac", "linux", "windows"],
      "permissions": { "hosts": ["api.example.com"], "capabilities": ["identity", "storage"] },
      "settings": { "apiUrl": "https://api.example.com", "pageSize": 20 },
      "bundle": "/v1/addons/com.example.notes-sidebar/bundle/<sha256>",
      "updated_at": "2026-10-19T18:04:05.123Z"
    }
  ]
}
```

- `revision` = `r_` + the first 16 hex of SHA-256 over the resolved list's ids, slots,
  bundle hashes and settings; it changes exactly when what this person should run changes.
  Send it back as `If-None-Match` (quoted ETag or the bare revision) → `304` with no body.
- `platforms` is always listed (all three when the manifest names none); filter on the client.
- `settings` = manifest defaults overlaid by the admin's values; settings with neither are
  absent. Values may be credentials: keep them like secrets.
- `size` is the served bundle's byte length (verify before and after downloading).

### `GET /v1/addons/{id}/bundle/{sha256}`

The served bundle (`application/json`, `copper-addon-bundle/1`; [format](addons.md#bundles)),
only while that addon version is what `GET /v1/addons` resolves for the caller — otherwise
`404`. `Cache-Control: private, max-age=31536000, immutable`, `ETag: "<sha256>"`. Check that
SHA-256 of the body equals `{sha256}` before using it.

---

## Device agent relay

(`features`: `device_agent`, 0.13.0.) An agent — phi, Claude Code, any MCP client or plain
`curl` — calls the tools of a person's Copper for iOS through this server. The phone dials out
and holds an event stream while Copper is open; the server hands it each call and returns its
reply. Nothing listens on the phone.

```text
agent ── POST /v1/agent/mcp (cat_ token) ──▶ copper-cloud ── SSE "request" ──▶ Copper on iPhone
agent ◀── JSON-RPC result (JSON or SSE) ──── copper-cloud ◀── POST frames "reply" ── iPhone
```

Which links are online and the calls in flight live in memory (one daemon per instance); links
and tokens are stored (`device_links`, `agent_tokens`).

### Device side 🔒

Gate credential + session, like every user route. A link belongs to the calling session's
device; one link per (user, device).

#### `POST /v1/me/links`

Register (or update) this device's link and its tool manifest.

```json
{ "label": "Ada's iPhone", "client_version": "ios-1.0.0",
  "tools": [ { "name": "jev_run", "description": "…", "inputSchema": { "type": "object" } } ] }
```

`200` — the link object (every link route returns it):

```json
{ "id": "uuid", "device_id": "uuid", "device_name": "Ada's iPhone", "label": "Ada's iPhone",
  "online": false, "revoked": false, "created_at": "2026-10-31T20:00:00Z", "last_seen_at": null,
  "tools": 22 }
```

- Idempotent: the same device always gets the same link id, and a revoked link comes back
  un-revoked. Omitted fields keep their stored value (a new link: `label` = the device name, no
  tools). `label` ≤ 100 characters and `client_version` ≤ 64 (longer values are cut).
- `tools` is stored as sent and served to agents by `tools/list`, so an agent can list tools
  while the phone is closed. `400` unless it is an array of at most 100 objects, each with a
  unique, non-empty `name` (≤ 128 characters), and at most 256 KiB as JSON.
- In the link object, `tools` is how many tools the manifest has; `online` is whether its stream
  is open and the device was heard from in the last 45 s; `last_seen_at` is when it last opened
  or closed its stream or uploaded frames (refreshed at most every 30 s while online).

#### `GET /v1/me/links` · `GET /v1/me/links/{id}` · `DELETE /v1/me/links/{id}`

- `GET /v1/me/links`: the user's live (not revoked) links, oldest first.
- `GET /v1/me/links/{id}`: one link, revoked included.
- `DELETE /v1/me/links/{id}`: revoke it (any of the user's devices may). Its stream gets
  `revoked` and closes, its calls in flight fail, and every agent call answers "turned off" until
  the device registers again. Idempotent; `200 {"ok": true}`.

All three answer `404` for another user's link. Deleting the device
([`DELETE /v1/devices/{id}`](#delete-v1devicesid)) revokes its link too.

#### `GET /v1/me/links/{id}/frames` — SSE mailbox

Only the link's own device may open it (`403` from the user's other devices, `404` for another
user's link). `Content-Type: text/event-stream`, `Cache-Control: no-store`, `X-Accel-Buffering:
no`. A new stream supersedes the previous one (which gets `superseded` and closes). Each `data:`
is one JSON object:

| event | data |
|---|---|
| `hello` | `{"link": {…link object…}, "protocol": 1}` — always first |
| `request` | `{"id": "req_…", "method": "tools/call", "params": {"name": "jev_run", "arguments": {…}}, "caller": {"token_id": "uuid", "label": "phi"}, "deadline_ms": 200000}` |
| `cancel` | `{"id": "req_…"}` — the agent went away, timed out or was revoked; stop that call |
| `ping` | `{}` every 15 s |
| `superseded` | `{}`, then the stream closes; do not reconnect from this stream |
| `revoked` | `{}`, then the stream closes; the user turned the link off, or the account was disabled or deleted |

Opening a revoked link's stream answers `hello` (`"revoked": true`), then `revoked`, then closes.
The stream also ends on server shutdown and when the session is revoked (checked every 5 min).

**Re-delivery (0.13.1).** A call stays pending when the stream its request went down ends
without a reply. Every stream the device opens (a reconnect, or one that supersedes a stream
still open) gets, right after `hello`, every request still pending on its link, oldest first,
with the same `id`, `method`, `params` and `caller`, and `deadline_ms` = the time the call has
left. The device must treat a request id it has already seen as the same call: answer a finished
one with its stored reply, ignore one it is still running. A pending call with no stream fails
after 15 s ([below](#post-v1agentmcp--mcp-streamable-http-stateless)).

#### `POST /v1/me/links/{id}/frames`

From the link's own device (`403` / `404` as above): `{"frames": [ … ]}`, body up to 32 MiB.
`200 {"ok": true, "accepted": n}` — `n` = well-formed frames of a known type (malformed frames and
unknown types are skipped; a reply for an unknown or finished request is accepted and ignored).
`400` if the body is not `{"frames": [...]}`.

| frame | shape |
|---|---|
| reply | `{"type": "reply", "id": "req_…", "result": {"content": [...], "isError": false}}` or `{"type": "reply", "id": "req_…", "error": {"code": "…", "message": "…"}}` |
| progress | `{"type": "progress", "id": "req_…", "seq": 3, "message": "3 · CLICK [12] button \"Search\""}` — `seq` must grow (a stale or repeated one is ignored; a frame without one counts as the next); messages over 16 KiB are cut |
| heartbeat | `{"type": "heartbeat"}` every 15 s while the stream is open |
| log | `{"type": "log", "level": "info", "message": "…"}` — logged at debug, never stored |

A link is **online** while its stream is open and the device was heard from — stream opened,
heartbeat or any other frames POST — within the last 45 s. The phone closes its stream when
Copper leaves the screen.

#### Agent tokens: `POST /v1/me/agent-tokens` · `GET /v1/me/agent-tokens` · `DELETE /v1/me/agent-tokens/{id}`

Minted by the phone for one agent; bound to the minting user and device.

```json
// POST body (optional; label defaults to "Agent", ≤ 100 characters)
{ "label": "phi" }
// 201
{ "id": "uuid", "token": "cat_<43 base64url chars>", "label": "phi", "device_id": "uuid",
  "created_at": "2026-10-31T20:00:00Z", "mcp_url": "https://cloud.example.com/v1/agent/mcp" }
```

- The token (`cat_` + 32 random bytes, base64url) is in this response only; the server stores
  SHA-256(token). At most 100 live tokens per user (`400` beyond: revoke some).
- `mcp_url` is built from the instance's public URL (with ACME, from `tls.domain`). The agent's
  HTTP client must trust the certificate: use ACME or a TLS-terminating proxy, or trust a
  self-signed instance's certificate explicitly.
- `GET` lists the user's live tokens, newest first:
  `[{"id", "label", "device_id", "created_at", "last_used_at"}]` — never the token
  (`last_used_at` is refreshed at most once a minute).
- `DELETE` revokes at once: the next agent request is `401`, and its calls in flight fail and get
  a `cancel` on the device. Idempotent; `200 {"ok": true}`; `404` if not yours. Deleting the
  device revokes its tokens too.

### Agent side: `cat_` token, no gate header

```
Authorization: Bearer cat_<43 chars>
```

`/v1/agent/*` is outside the instance gate: the 256-bit token is the credential (a gate header,
if sent, is ignored). A missing, malformed, unknown or revoked token — or one whose account is
disabled or whose device was deleted — is `401 {"error":"agent_token"}`. The token is read from
the `Authorization` header only. 600 requests per minute per token (`429 rate_limited`).

#### `GET /v1/agent/status`

```json
{ "device": { "id": "uuid", "name": "Ada's iPhone" }, "label": "Ada's iPhone", "online": true,
  "last_seen_at": "2026-10-31T20:00:00Z", "tools": 22 }
```

`online` is `false` when the link is offline, revoked, or was never registered (then `label` and
`last_seen_at` are `null` and `tools` is `0`). `Cache-Control: no-store`.

#### `POST /v1/agent/mcp` — MCP Streamable HTTP (stateless)

One JSON-RPC 2.0 message per request. A batch (JSON array), invalid JSON, or a message without a
`method` that is not a response is `400` with a JSON-RPC error (`-32600` / `-32700`,
`"id": null`). No `Mcp-Session-Id` is needed or returned. `GET` and `DELETE` (with a valid token)
answer `405 method_not_allowed` with `Allow: POST`: there is no server-initiated stream.

| method | answer |
|---|---|
| `initialize` | `{"protocolVersion": <the client's if one of 2025-06-18 / 2025-03-26 / 2024-11-05, else 2025-06-18>, "capabilities": {"tools": {"listChanged": false}}, "serverInfo": {"name": "copper-mobile", "version": "<server version>"}, "instructions": "<how to drive the phone, Jev first>"}` |
| `notifications/*` (also any other message without an `id`, and responses) | `202`, empty body |
| `ping` | `{}` |
| `tools/list` | `{"tools": [...]}` — the manifest the device last registered (empty if none), also while it is offline or turned off |
| `tools/call` | relayed to the device (below) |
| anything else | JSON-RPC error `-32601` |

`tools/call` (`params.name` required and `params.arguments` an object when present, else
`-32602`):

- **Turned off** (link revoked or never registered): at once, `isError: true` with "Agents are
  turned off on <device name>. Ask the person to turn on Settings › Agents › Let agents use this
  iPhone in Copper, then try again."
- **Not in the manifest**: at once, `isError: true` naming the tool.
- **Offline**: at once, `{"content": [{"type": "text", "text": "Copper isn't open on <device
  name>. Ask the person to open Copper on their iPhone and keep it on screen (Settings › Agents ›
  Let agents use this iPhone must be on), then try again."}], "isError": true}`.
- **Busy** (0.13.1): when the device already has 8 calls in flight (from any of its agents), at
  once, `isError: true` with "Copper on <device name> is busy with other calls; try again
  shortly." Nothing is sent to the device.
- Otherwise the device gets a `request` frame (deadline 200 s) and the server waits for its
  reply. A reply's `result` is the JSON-RPC result as sent; a reply `error` becomes
  `isError: true` with its message. A deadline miss sends `cancel` to the device and answers
  `isError: true` ("… timed out …"). If the device's stream drops mid-call, the request is sent
  again on the stream it opens next ([re-delivery](#get-v1melinksidframes--sse-mailbox)); if
  the device stays offline for 15 s while the call runs (Copper left the screen), the call is
  cancelled and answers `isError: true` then, instead of waiting out the deadline. Revoking the
  link or the token mid-call, disabling or deleting the account, or a server shutdown, answers
  `isError: true` too.
- With `text/event-stream` in the request's `Accept`, the answer is an SSE stream
  (`Cache-Control: no-store`, `X-Accel-Buffering: no`): a `: keepalive` comment after every 10 s
  of silence (proxies drop idle requests at 60 s), an `event: message` with
  `notifications/progress` (`{"progressToken": <the call's>, "progress": <seq>, "message": "…"}`)
  for each device progress frame when the call carried `params._meta.progressToken`, then an
  `event: message` with the JSON-RPC response, and the stream ends. Otherwise one
  `application/json` response (progress is not delivered). Every other method answers
  `application/json`.
- The call ends at its deadline (or any of the above) whether or not the agent reads its
  answer, and an agent that reads slowly holds up nothing else: up to 32 progress notifications
  wait for it, later ones are dropped until it catches up (the response is never dropped).
- The agent disconnecting mid-call (closing the request or the stream) sends `cancel` to the
  device.
- One log line per call — token id, device id, tool name, duration (`ms`), outcome (`status`) —
  and a `device_agent_calls_total{status}` count. Never arguments or results.

The `initialize` instructions tell the agent that this is the person's Copper on their iPhone,
signed in as them; that it only works while Copper is open on their screen; to reach for
`jev_run` first (the whole goal with concrete values and a stop condition; `DONE` is Jev's claim,
verified with `jev_observe` / `jev_extract`), then `browser_snapshot` / `browser_click` /
`browser_type` for whatever Jev is `BLOCKED` on.

```sh
# The phone minted $CAT for this agent; no gate header needed.
curl -s https://cloud.example.com/v1/agent/status -H "Authorization: Bearer $CAT"
curl -sN https://cloud.example.com/v1/agent/mcp -H "Authorization: Bearer $CAT" \
  -H 'Accept: application/json, text/event-stream' \
  -d '{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"jev_run",
       "arguments":{"goal":"Open example.com and read the headline; stop when it is visible"},
       "_meta":{"progressToken":1}}}'
```

---

## Canvases 🔒

Mounted under the same `/v1` gate and session auth. Since 0.11.0 they are served by the linked
Canvas Cloud: copper-cloud forwards each request as the signed-in user and returns the answer
unchanged ([canvas-link.md](canvas-link.md#the-proxy)); the routes are documented in Canvas
Cloud's [docs/canvas.md](https://github.com/copper-browser/canvas-cloud/blob/main/docs/canvas.md)
(pointers: [canvas.md](canvas.md)). Request bodies: 4 MiB for `/ops`, 64 KiB otherwise. No
Canvas Cloud linked → `503 canvas_unavailable`; unreachable, timed out, or the app key was
rejected → `502 canvas_upstream`; a Canvas Cloud `401` is never passed on (Copper would sign
out). Routes:

`GET/POST /v1/canvases`, `GET/PATCH/DELETE /v1/canvases/{id}`, `GET /v1/canvases/{id}/members`,
`DELETE /v1/canvases/{id}/members/{user_id}`, `GET/POST /v1/canvases/{id}/invites`,
`DELETE /v1/canvases/{id}/invites/{invite_id}`, `GET /v1/invites`,
`POST /v1/invites/{id}/accept|decline`,
`POST/GET/DELETE /v1/canvases/{id}/links` (and `DELETE .../links/{link_id}`),
`GET /v1/canvas-links/{token}`, `POST /v1/canvas-links/{token}/join`,
`GET /v1/people`, `GET /v1/canvases/{id}/ws`
(WebSocket, Yjs sync protocol), `GET /v1/canvases/{id}/state`, `GET /v1/canvases/{id}/read`,
`POST /v1/canvases/{id}/ops`, and chat mentions (0.6.0): `POST /v1/canvases/{id}/mentions`,
`GET /v1/mentions`, `POST /v1/mentions/read` ([Canvas Cloud › Chat and mentions](https://github.com/copper-browser/canvas-cloud/blob/main/docs/canvas.md#chat-and-mentions)).

### People

`GET /v1/people` is available to any signed-in user on the instance. It returns active accounts
(excluding the caller), sorted by display name. Through the link these are the Canvas Cloud's
enabled users: on a Canvas Cloud used only by this copper-cloud, the instance's Copper
accounts; a Canvas Cloud with other accounts (its own sign-ups, other linked apps) lists those
too:

```json
{"people":[{"id":"uuid","display_name":"Ada","email":"ada@example.com"}]}
```

`q` is a case-insensitive substring filter over display name or email. `limit` defaults to 20
and is capped at 50.

### Canvas share links

Any canvas member can create an editor link (`POST /v1/canvases/{id}/links`, body `{}` or
`{"role":"editor"}`):

```json
{"id":"uuid","token":"<43-char token>","canvas_id":"uuid","role":"editor","created_at":"2026-10-01T12:00:00Z"}
```

The token is returned only from creation. Owners can list links (`GET .../links`) and revoke one
or all (`DELETE .../links/{link_id}`, `DELETE .../links`, both `204`). The list contains `id`,
`role`, `created_by`, `created_at` and `uses`, never tokens. Preview (`GET /v1/canvas-links/{token}`)
returns the canvas name, owner id/display name, link role and whether the caller is already a
member; unknown or revoked tokens return `404 {"error":"link_not_found"}`. Joining
(`POST .../join`) is idempotent, adds an editor membership without downgrading an owner, removes
a pending email invite, increments `uses`, and returns the normal canvas list item. Link tokens
are 32 random bytes encoded base64url without padding and stored only as SHA-256 digests.

`GET /join/{token}` is an ungated, no-store HTML handoff page for web links, served by
copper-cloud itself (not proxied); it does not look up the token or reveal canvas data.

The admin endpoints that manage the link (`GET/PUT/DELETE /admin/api/canvas-link`,
`POST /admin/api/canvas-link/test`) are in [admin-api.md](admin-api.md#canvas-cloud-link).

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
