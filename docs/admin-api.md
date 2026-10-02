# Admin API (the portal's contract)

The admin portal (`portal/`, a Next.js static export embedded in the binary and served from
`/`) talks to the server only through `/admin/api/*`. This document is the complete
contract. Everything here is JSON over the same TLS listener as `/v1`.

The admin API is **not** behind the instance gate (`X-Copper-Instance` is ignored here) — it
has its own accounts (`admins` table, created with `copper-cloud admin create-admin` or by
`install.sh` / Terraform) and its own cookie sessions.

## Conventions

### Session cookie

`POST /admin/api/login` sets:

```
Set-Cookie: cc_admin=<43-char token>; Path=/admin; HttpOnly; Secure; SameSite=Strict; Max-Age=604800
```

* `Secure` is omitted only when the server runs with `tls.mode = "off"` (plain HTTP behind a
  proxy / local dev).
* Sessions last **7 days** from login (absolute, not sliding). The DB stores only
  SHA-256(token).
* The browser sends it automatically on same-origin requests to `/admin/...`; JS cannot read
  it. Use `credentials: 'same-origin'` (the default for same-origin `fetch`).
* Missing / unknown / expired cookie → `401 {"error":"admin_session"}`. The portal should
  route to `/login` on any 401 from anything except `POST login` itself.

### CSRF header (required on every non-GET request)

Every `POST`, `PATCH`, `DELETE` (including `login` and `logout`) must carry:

```
X-Requested-With: copper-cloud-portal
```

Without it → `403 {"error":"csrf","message":"forbidden"}`. `GET`/`HEAD` do not need it (it
is harmless to always send it).

```ts
async function api<T>(path: string, init: RequestInit = {}): Promise<T> {
  const res = await fetch(`/admin/api/${path}`, {
    credentials: 'same-origin',
    ...init,
    headers: {
      'X-Requested-With': 'copper-cloud-portal',
      ...(init.body ? { 'Content-Type': 'application/json' } : {}),
      ...init.headers,
    },
  });
  if (res.status === 401 && path !== 'login') location.href = '/login';
  if (!res.ok) throw await res.json(); // {error, message}
  return res.json();
}
```

### Errors

Always `{"error": "<code>", "message": "<human text>"}`:

| Status | `error` | When |
|---|---|---|
| 400 | `bad_request` | invalid JSON / field (the `message` says which) |
| 401 | `admin_session` | no / bad / expired cookie |
| 401 | `credentials` | wrong email or password (`login`, `password`) |
| 403 | `csrf` | missing `X-Requested-With: copper-cloud-portal` |
| 404 | `not_found` | unknown id or unknown `/admin/api/...` route |
| 413 | `payload_too_large` | body over 16 KiB |
| 429 | `rate_limited` | `login` rate limit (`Retry-After: 60`) |
| 500 | `internal` | server bug / database failure |

### Pagination (every list endpoint)

Query: `?limit=` (default **100**, clamped to 1…**500**) and `?offset=` (default 0). Lists are
sorted **newest first**. Response envelope:

```json
{ "items": [ … ], "total": 42, "limit": 100, "offset": 0 }
```

`total` is the number of rows matching the filters (ignoring limit/offset).

### Types

* Timestamps: RFC 3339 UTC strings (`"2026-10-01T18:04:05.123Z"`), `null` when absent.
* IDs: UUID strings.
* Request bodies: JSON, ≤ 16 KiB, `Content-Type` optional.

### Audit

Every mutation (login, logout, password, settings, key create/revoke, user update / delete /
reset-password, device delete, canvas delete, pairing-code revoke) writes an `admin_audit`
row, readable with `GET audit`.

---

## Session

### `POST /admin/api/login`

Rate limited per client IP (`limits.auth_per_minute`, default 10/min — a separate bucket from
`/v1/auth`).

```json
// request
{ "email": "admin@cloud.example.com", "password": "…" }
// 200 (+ Set-Cookie: cc_admin=…)
{
  "admin": {
    "id": "0192f1c4-…",
    "email": "admin@cloud.example.com",
    "created_at": "2026-10-01T17:00:00Z",
    "last_login_at": "2026-10-01T18:04:05Z"
  }
}
```

Wrong email or password → `401 {"error":"credentials"}` (constant-time; does not reveal which).
Emails are case-insensitive.

### `POST /admin/api/logout`

Deletes the current session (if any) and clears the cookie. Always `200 {"ok": true}`.

### `GET /admin/api/me`

```json
{
  "admin": { "id": "…", "email": "admin@cloud.example.com", "created_at": "…", "last_login_at": "…" },
  "session": { "created_at": "…", "expires_at": "…" }
}
```

### `POST /admin/api/password` — change the signed-in admin's password

```json
// request
{ "old": "current password", "new": "at least 10 characters" }
// 200 — every OTHER session of this admin is revoked
{ "ok": true, "revoked_sessions": 1 }
```

Wrong `old` → `401 {"error":"credentials"}` (the portal should show an inline error, not
redirect — check the code). `new` shorter than 10 chars → `400`.

---

## Instance

### `GET /admin/api/overview`

```json
{
  "version": "0.3.0 (abc1234)",
  "uptime_s": 86400,
  "access_mode": "directory",
  "allow_signup": true,
  "counts": {
    "users": 12,
    "devices": 19,
    "canvases": 15,
    "access_keys": 7,
    "live_rooms": 2,
    "live_peers": 3
  },
  "public_url": "cloud.example.com:443",
  "tls": { "mode": "self-signed", "fingerprint": "9f86d081…(64 hex)" }
}
```

* `counts.access_keys` = **active** keys (not revoked, not expired, not exhausted).
* `tls.mode`: `"self-signed" | "acme" | "off"`; `fingerprint` is `null` for ACME (publicly
  trusted) or when no certificate file is readable.

### `GET /admin/api/settings`

```json
{
  "access_mode": "directory",
  "allow_signup": true,
  "instance_link_code": "copper-cloud://cloud.example.com:443/#k=<instance key>&fp=…"
}
```

* `access_mode`:
  * `"open"` — anyone holding the shared **instance link code** may connect (and sign up if
    `allow_signup`); access keys also work.
  * `"directory"` — only **access keys** minted here pass the gate; the instance key is
    rejected. Signup requires an access key (and its email, if the key has one).
    `allow_signup` is ignored in this mode (holding a key implies permission).
* `allow_signup`: self-service signup in `open` mode (the very first user can always sign up).
* `instance_link_code`: the shared link code (works only in `open` mode). It is a secret —
  show it behind a "reveal" control. `null` if the TLS certificate is not readable.

### `PATCH /admin/api/settings`

```json
// request (either field optional)
{ "access_mode": "open", "allow_signup": false }
// 200 → same shape as GET settings
```

Invalid `access_mode` → `400`. The gate picks up a mode change within 5 seconds.

---

## Access keys

An access key is a per-person gate credential: `ck_` + 43 base64url chars (32 random bytes).
Only its SHA-256 is stored; the plaintext is returned **once**, by `POST`.

Key object (`GET` items and the `POST` response):

```json
{
  "id": "0192f1c9-…",
  "label": "Ana's MacBook",
  "email": "ana@example.com",
  "created_at": "2026-10-01T18:10:00Z",
  "created_by": "admin@cloud.example.com",
  "expires_at": "2026-10-31T18:10:00Z",
  "revoked_at": null,
  "last_used_at": "2026-10-02T09:00:00Z",
  "uses": 1,
  "max_uses": 1,
  "status": "active"
}
```

* `email`: when set, signup with this key must use this email (case-insensitive).
* `created_by`: admin email; `null` for keys minted by pairing (label `"<device> via
  pairing"`) or whose admin was deleted.
* `uses`: number of sign-ins (signup / login) performed through the key. When `max_uses` is
  set and `uses >= max_uses`, the key can no longer **sign anyone in** (status `exhausted`),
  but Coppers already signed in with it keep working. So `max_uses: 1` = a one-person
  invite.
* `status`: `"active" | "revoked" | "expired" | "exhausted"` (first that applies:
  revoked, then expired, then exhausted).
* `last_used_at`: last gated request with this key (updated at most once a minute).

### `GET /admin/api/access-keys?limit=&offset=&status=`

`status` (optional) filters to one status. Response: pagination envelope of key objects.

### `POST /admin/api/access-keys`

```json
// request
{ "label": "Ana's MacBook", "email": "ana@example.com", "expires_in_days": 30, "max_uses": 1 }
// 201
{
  "id": "0192f1c9-…",
  "label": "Ana's MacBook",
  "email": "ana@example.com",
  "created_at": "…",
  "created_by": "admin@cloud.example.com",
  "expires_at": "…",
  "revoked_at": null,
  "last_used_at": null,
  "uses": 0,
  "max_uses": 1,
  "status": "active",
  "key": "ck_4fJ9…(43 chars)",
  "link_code": "copper-cloud://cloud.example.com:443/#k=ck_4fJ9…&fp=9f86…"
}
```

* `label` required, 1–200 chars. `email` optional (validated). `expires_in_days` optional,
  1–3650 (omit/null = never). `max_uses` optional, 1–1000000 (omit/null = unlimited).
* `key` and `link_code` are returned **only here, once**. The portal shows them in a "shown
  once" dialog with copy buttons. The person pastes the `link_code` into Copper
  (Settings › Cloud › Connect).

### `DELETE /admin/api/access-keys/{id}` — revoke

`200` → the key object with `revoked_at` set / `status: "revoked"`. Takes effect on the next
request (no cache). Revoking an already-revoked key is a no-op `200`. Unknown id → `404`.

---

## People (users)

User object:

```json
{
  "id": "0192f1d0-…",
  "email": "ana@example.com",
  "display_name": "Ana",
  "created_at": "2026-10-01T18:20:00Z",
  "last_seen_at": "2026-10-02T09:00:00Z",
  "disabled": false,
  "device_count": 2,
  "canvas_count": 3
}
```

`last_seen_at` = latest device activity (`null` if never). `canvas_count` = canvases the user
is a member of (including their Personal canvas, which is created the first time their Copper
lists canvases). There is no "create user" call: people sign up from Copper with the access
key you gave them (directory mode) or the instance link code (open mode).

### `GET /admin/api/users?limit=&offset=&q=`

`q` (optional): case-insensitive substring of email or display name. Pagination envelope.

### `PATCH /admin/api/users/{id}`

```json
// request (either optional)
{ "disabled": true, "display_name": "Ana P." }
// 200 → user object
```

Disabling revokes all of the user's sessions immediately, disconnects their live canvas
peers and blocks sign-in (`401 account_disabled` in Copper). `display_name` must not be empty.
Unknown id (or not a UUID) → `404`.

### `DELETE /admin/api/users/{id}`

Permanently deletes the user and everything they own: sessions, devices, sync docs, history,
pairing codes, access keys bound to their email, their canvases (Personal and shared ones
they own — including other members' access to them) and their memberships/invites. Live
canvas rooms of deleted canvases are closed.

```json
{ "ok": true }
```

### `POST /admin/api/users/{id}/reset-password`

```json
// request
{ "password": "new password, 10+ chars" }
// 200 — all of the user's sessions are revoked
{ "ok": true, "revoked_sessions": 2 }
```

---

## Devices

Device ids are generated by each Copper install and are unique **per user** (one install
signed into two accounts appears twice, once per user).

Device object:

```json
{
  "id": "6b1e…",
  "user_id": "0192f1d0-…",
  "user_email": "ana@example.com",
  "name": "Ana's MacBook Pro",
  "created_at": "…",
  "last_seen_at": "…"
}
```

### `GET /admin/api/devices?limit=&offset=&user_id=`

`user_id` optional filter. Pagination envelope, newest (`created_at`) first.

### `DELETE /admin/api/devices/{id}?user_id=`

Removes the device: its sessions are revoked and its `tabs:<id>` sync doc deleted. **Pass
`user_id`** (from the device object) to remove exactly that row; without it, every account's
row for that device id is removed.

```json
{ "ok": true, "deleted": 1 }
```

---

## Canvases

Canvas object (content is never exposed):

```json
{
  "id": "0192f1d5-…",
  "name": "Roadmap",
  "kind": "shared",
  "owner_id": "0192f1d0-…",
  "owner_email": "ana@example.com",
  "member_count": 3,
  "share_link_count": 2,
  "created_at": "…",
  "updated_at": "…"
}
```

`kind`: `"personal" | "shared"`. `share_link_count` counts active (not revoked) canvas
share links; link tokens and canvas content are never exposed to the admin API.

### `GET /admin/api/canvases?limit=&offset=&kind=`

`kind` optional filter. Pagination envelope.

### `DELETE /admin/api/canvases/{id}`

Deletes the canvas, its history, members and invites; connected peers are disconnected. A
deleted Personal canvas is recreated empty the next time its owner opens it.

```json
{ "ok": true }
```

---

## Pairing codes

Single-use codes a signed-in Copper mints (`POST /v1/auth/pairing`) to link and sign in
another Mac. Valid 10 minutes. Admins can see and revoke the active ones.

Pairing-code object:

```json
{
  "id": "0192f1e0-…",
  "user_id": "0192f1d0-…",
  "user_email": "ana@example.com",
  "device_name": "Ana's Mac mini",
  "created_by_device": "6b1e…",
  "created_at": "…",
  "expires_at": "…"
}
```

### `GET /admin/api/pairing-codes?limit=&offset=&user_id=`

Active (unused, unexpired) codes only. Pagination envelope.

### `DELETE /admin/api/pairing-codes/{id}`

Revokes an unused code. `{ "ok": true }`; unknown/used → `404`.

---

## Audit log

### `GET /admin/api/audit?limit=&offset=`

```json
{
  "items": [
    {
      "id": 118,
      "admin_id": "0192f1c4-…",
      "admin_email": "admin@cloud.example.com",
      "action": "access_key.create",
      "target": "0192f1c9-…",
      "detail": { "label": "Ana's MacBook" },
      "at": "2026-10-01T18:10:00Z"
    }
  ],
  "total": 118, "limit": 20, "offset": 0
}
```

`id` is a number (sequence). `admin_email` is `null` if that admin was deleted. `detail` is an
object or `null`. Actions:

| `action` | `target` | `detail` |
|---|---|---|
| `login` / `logout` | admin id | `null` |
| `password.change` | admin id | `null` |
| `settings.update` | `"settings"` | the changed fields, e.g. `{"access_mode":"open"}` |
| `access_key.create` | key id | `{label, email}` |
| `access_key.revoke` | key id | `{label}` |
| `user.update` | user id | the changed fields + `{email}` (+ `revoked_sessions` when disabling) |
| `user.delete` | user id | `{email}` |
| `user.reset_password` | user id | `{email}` |
| `device.delete` | device id | `{user_id}` (or `{user_ids:[…]}` when deleted without `?user_id=`) |
| `canvas.delete` | canvas id | `{name}` |
| `pairing_code.revoke` | code id | `{user_id}` |

---

## Non-JSON responses to expect

* `405` with an empty body for a known route with the wrong method (e.g. `PUT settings`).
* Everything else is JSON, including 404s for unknown `/admin/api/...` paths.

## Hosting notes for the portal build

* Served from `/` by the binary: exact file → `<path>.html` → `<path>/index.html` → SPA
  fallback `index.html`. So `/keys` serves `keys.html` (Next `output: 'export'` default) or
  `keys/index.html` (`trailingSlash: true`) — both work.
* `/_next/static/*` is served `Cache-Control: public, max-age=31536000, immutable`; HTML is
  `no-store`.
* CSP: `default-src 'self'; img-src 'self' data:; style-src 'self' 'unsafe-inline';
  script-src 'self' <hashes>; connect-src 'self'; frame-ancestors 'none'`. Next's static
  export emits inline bootstrap `<script>` tags (`self.__next_f.push(…)`); the server
  computes the SHA-256 of every inline `<script>` in each embedded HTML file and adds them
  as `'sha256-…'` sources to that file's `script-src`, so the export works unmodified. Do
  not generate inline scripts at runtime (`eval`, `new Function`, injected `<script>`), and
  do not load anything from other origins (fonts, CDNs) — bundle everything.
  Also sent: `X-Content-Type-Options: nosniff`, `Referrer-Policy: same-origin`.
* Paths owned by the server and never served from the portal: `/v1/*`, `/admin/api/*`,
  `/healthz`, `/metrics`.
* Dev: `next dev` proxies `/admin/api` to `https://127.0.0.1:8443`. With `tls.mode = "off"`
  locally the cookie has no `Secure` flag so plain-HTTP dev works.
