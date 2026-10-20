# Canvas link

Since 0.11.0 copper-cloud does not store or serve canvases itself. Canvases live on a
**Canvas Cloud** — a separate, self-hostable collaborative canvas server
([copper-browser/canvas-cloud](https://github.com/copper-browser/canvas-cloud), MIT) — and
copper-cloud reaches it through a thin **canvas link** (crate `copper-cloud-canvas-link`,
`crates/canvas-link`): once an admin links a Canvas Cloud (URL + app key), copper-cloud
reverse-proxies the unchanged canvas API there, acting as the signed-in Copper user, and
re-publishes Canvas Cloud's canvas events on each user's `/v1/sync/events` stream.

Copper (macOS and Linux) needs no update: the same routes, JSON, status codes,
WebSocket protocol, close codes, SSE events and user ids as copper-cloud 0.10.

Moving an existing deployment's canvases over is a one-time step:
[canvas-migration.md](canvas-migration.md).

## Why

The canvas server (REST, Yjs rooms, ops/read, chat mentions, invites, share links, people)
used to be the `copper-cloud-canvas` crate inside this binary. It now is its own product with
its own users and auth (password accounts, personal access tokens, OIDC), linked apps,
an MCP surface for agents and an import format, so it can be used — and
hosted — without Copper. copper-cloud keeps what is Copper-specific: the instance gate,
Copper accounts and sessions, sync, intelligence keys, the admin portal, and the
`copper://` share-link landing page.

## Architecture

```
┌── Copper ──────────────────┐               ┌── copper-cloud ───────────┐               ┌── Canvas Cloud ────────────┐
│ canvas list, invites,      │               │ instance gate → session   │               │ /v1/canvases… /v1/invites… │
│ share links, mentions      │ TLS (pinned)  │ canvas link:              │ HTTPS/WSS, or │ /v1/mentions… /v1/people   │
│ canvas room (y-websocket)  │──────────────▶│   REST proxy              │──────────────▶│ /v1/canvas-links/…         │
│                            │ X-Copper-     │   WebSocket pipe          │ loopback http │ /v1/apps/self/* (users,    │
│ GET /v1/sync/events        │ Instance +    │   event bridge            │ Bearer cva_…  │   events, canvases, stats, │
│   ← event: canvas          │ Bearer token  │   provisioning            │ X-Canvas-User │   import, digest)          │
└────────────────────────────┘               │ Postgres: users, sync,    │◀── SSE ───────│ own users, sessions, PATs, │
                                             │   canvas_link, ledger,    │ /apps/self/   │   OIDC, MCP, Postgres      │
                                             │   legacy canvas tables    │ events        └────────────────────────────┘
                                             └───────────────────────────┘
```

- Copper only ever talks to copper-cloud, on the same host, port and pinned certificate as
  before. It never learns the Canvas Cloud's address.
- copper-cloud authenticates to Canvas Cloud as one **linked app** (`cva_…` key) and names
  the Copper user it acts for in `X-Canvas-User`. Every Copper user is a Canvas Cloud app user
  whose subject **and** user id are the Copper user id (see [Provisioning](#user-provisioning)).
- Canvas content (Yjs documents, chat, mention excerpts) is stored and encrypted by Canvas
  Cloud, in Canvas Cloud's database. copper-cloud stores the link (one row), a provisioning
  outbox, the migration ledger, and — untouched — the legacy canvas tables of 0.10.x and older.

The small recommended setup runs Canvas Cloud on the same host as copper-cloud, listening on
loopback with TLS off (`listen = "127.0.0.1:8480"`, `tls.mode = "off"`), and links it as
`http://127.0.0.1:8480`.

## What Copper clients see

Nothing changes while a Canvas Cloud is linked and reachable:

| Surface | Behaviour |
|---|---|
| Routes | `/v1/canvases…` (incl. `/v1/canvases/{id}/ws`), `/v1/invites…`, `/v1/canvas-links/…`, `/v1/mentions…`, `/v1/people` — any method, same paths and query strings |
| JSON | byte-for-byte what Canvas Cloud returns — the canvas API moved verbatim (top-level arrays, `201` vs `200` on invites, `404 link_not_found`, …) |
| Ids | canvas ids, user ids (members, chat `authorId`, mention ids) and share-link tokens are the same as on 0.8 (after [migration](canvas-migration.md)) |
| Live room | the same y-websocket protocol and close codes (`4403` removed, `4404` deleted, `1001` shutdown, …) |
| Notifications | `event: canvas` frames on `GET /v1/sync/events` with `{"type":"canvas","canvas_id","kind"}`, same kinds ([below](#event-bridge)) |
| `GET /v1/info` | same `name`/`version`/limits; `features` gains `"canvas_link"`, and a new `"canvas": {"linked": bool}` object |
| `/join/{token}` | the `copper://canvas/join/{token}?cloud={host}` landing page, served by copper-cloud, with a link to Canvas App (`/canvas/#/join/{token}`) for browsers |

Clients gate canvas features on the server **version** (share links/people ≥ 0.3.0, invite
reminders ≥ 0.5.0, mentions ≥ 0.6.0), not on `features`, so nothing has to be negotiated.

When no Canvas Cloud is linked, or it cannot be reached, every canvas route fails with a
non-2xx JSON error (`503 canvas_unavailable`, `502 canvas_upstream`) — never an empty `200`
list (Copper would drop the canvases from its list and close their tabs), never a
`404 not_found` (Copper may cache that as "feature unsupported"), and never a `401` (Copper
signs the user out). Copper retries `GET`s a couple of times, keeps what it had, and shows the
error message; sync, history and SSE keep working.

## The proxy

`CanvasLink::router()` is mounted inside `/v1`, where the canvas router used to be, so the
instance gate, request tracing/metrics and rate limits run first. Routes (any method):
`/canvases`, `/canvases/{*rest}`, `/people`, `/invites`, `/invites/{*rest}`,
`/canvas-links/{*rest}`, `/mentions`, `/mentions/{*rest}`.

For each request (`crates/canvas-link/src/proxy.rs`):

1. **Authenticate the Copper user** with the core `AuthUser` extractor (`Authorization:
   Bearer <session>` or `?token=`). No or bad session → copper-cloud's own `401 session`, as
   for every `/v1` route.
2. **Load the link** (decrypted settings cached 5 s; changes made through the admin API apply
   at once, CLI changes within 5 s). None, or `enabled = false` →
   `503 canvas_unavailable`.
3. **Forward** to `{canvas cloud url}/v1{same path}{same query minus every token= and instance= parameter}`.

### Headers

| Direction | Forwarded | Never forwarded | Added |
|---|---|---|---|
| request | `Content-Type`, `Accept`, `If-Match`, `If-None-Match`, `If-Modified-Since`, `If-Unmodified-Since`, `If-Range` | `Authorization` (the Copper session), `Cookie`, `X-Copper-Instance`, hop-by-hop headers, everything else | `Authorization: Bearer <app key>`, `X-Canvas-User: <Copper user id>`, `X-Forwarded-For: <client IP>` (as resolved by copper-cloud, honouring `trust_proxy`) |
| response | status, body (streamed), `Content-Type`, `Cache-Control`, `ETag`, `Last-Modified`, `Location`, `Retry-After`, `Content-Disposition` | everything else | — |

The `?token=` query parameter (Copper's session token for WebSocket tooling and browsers) and
`?instance=` (the gate credential a browser's room socket carries, below) are consumed by
copper-cloud and removed; all other parameters (`q`, `limit`, `sv`, `unread`, `full`, `ids`,
`types`, …) pass unchanged. Redirects from Canvas Cloud are not followed.

### Bodies, limits and timeouts

| What | Limit |
|---|---|
| `POST /v1/canvases/{id}/ops` request body | 4 MiB (`413 payload_too_large` above; checked on `Content-Length` and while reading) |
| every other proxied request body | 64 KiB (`413 payload_too_large`) |
| request body read deadline | 60 s (`400`) |
| connect (TCP + TLS) to Canvas Cloud | 5 s |
| whole request incl. the response body | 30 s; `/ops` 120 s |
| WebSocket handshake to Canvas Cloud (TCP + TLS + upgrade) | 15 s |
| WebSocket message / frame, both sides | 64 MiB / 16 MiB |

Response bodies are streamed through, not buffered. Content limits (op counts, name length,
mention rate limits, …) are Canvas Cloud's and reach Copper as Canvas Cloud's own errors.

### Status codes

| Situation | Copper gets |
|---|---|
| Canvas Cloud answers anything but `401` (`2xx`, `400`, `403`, `404`, `409`, `413`, `422`, `429`, `5xx`, …) | that status and body, verbatim |
| Canvas Cloud answers `401 {"error":"unknown_user"}` | copper-cloud provisions the user (`PUT /v1/apps/self/users/{id}`) and retries **once**; the retry's answer is handled like the first |
| provisioning after `unknown_user` fails | `502 {"error":"canvas_upstream","message":"could not provision this user on Canvas Cloud (…)"}` |
| any other `401`, or `unknown_user` again after the retry | `502 {"error":"canvas_upstream","message":"Canvas Cloud rejected this Copper Cloud's app key/user (<code>: <message>)"}` (logged at `error`: check the link's app key) |
| Canvas Cloud unreachable (DNS, connect, TLS, pin mismatch) | `502 {"error":"canvas_upstream","message":"Canvas Cloud is unreachable (…)"}` |
| connect/request timeout | `502 {"error":"canvas_upstream","message":"Canvas Cloud did not answer in time"}` |
| connection broken mid-request | `502 {"error":"canvas_upstream","message":"Canvas Cloud request failed (…)"}` |
| no Canvas Cloud linked (or the link is disabled) | `503 {"error":"canvas_unavailable","message":"no Canvas Cloud is linked to this Copper Cloud"}` |
| request body over the limit | `413 {"error":"payload_too_large"}` (copper-cloud's own) |
| no / expired Copper session | `401 {"error":"session"}` (copper-cloud's own, as before) |

**Why a `401` from Canvas Cloud is never forwarded.** Both Copper clients treat
`401 {"error":"session"}` — in practice any `401` on these routes — as "your session ended"
and sign the user out. A `401` from Canvas Cloud means the *app* was rejected (revoked or
rotated app key, unknown subject), which is an operator problem, not the user's: signing
every Copper out over it would be wrong and would not fix anything. So it becomes a `502`,
which Copper treats as a transient server error.

A Canvas Cloud that lacks a route answers `404 not_found`, which is passed through; keep the
Canvas Cloud at least as new as the canvas API your Coppers expect.

### WebSocket pipe

`GET /v1/canvases/{id}/ws` — HTTP/1.1 `Upgrade` or HTTP/2 extended `CONNECT` from Copper, as
before (`crates/canvas-link/src/ws.rs`):

1. copper-cloud first opens the **upstream** WebSocket to the same path on Canvas Cloud
   (`ws://` or `wss://`, query minus `token=` and `instance=`, app key + `X-Canvas-User` + `X-Forwarded-For`,
   HTTP/1.1, 15 s budget).
2. If Canvas Cloud refuses the handshake with an HTTP status (`403`, `404`, …), Copper gets
   that status and body **without** an upgrade — exactly what the old server did for
   non-members. A refused `401` is handled like REST: `unknown_user` → provision and retry
   once; anything else → `502 canvas_upstream`. A transport failure or timeout →
   `502 canvas_upstream` (`"Canvas Cloud WebSocket is unreachable (…)"`,
   `"Canvas Cloud did not answer the WebSocket handshake in time"`).
3. Only then is Copper's connection upgraded, and binary and text frames are piped both ways
   unchanged (Yjs sync, awareness, auth messages — no re-framing).

- **Ping/pong** is per side: copper-cloud pings Copper every 30 s and answers Canvas Cloud's
  pings; control frames are not forwarded. There is **no idle timeout** — a room can stay open
  indefinitely.
- **Close frames are forwarded with the same code and reason** in both directions (`4401`,
  `4403`, `4404`, `1001`, `1013`, …); `1005`/`1006`/`1015` (local-only codes) are forwarded as a
  close without a code.
- copper-cloud's own closes: Canvas Cloud connection lost, or a frame to Canvas Cloud not
  accepted within 60 s → Copper gets `1011` "canvas server connection lost"; Copper gone →
  Canvas Cloud gets `1001` "client went away"; copper-cloud shutting down (SIGTERM) → Copper
  gets `1001` "server shutting down" and Canvas Cloud a `1001` too.
- Copper reconnects as it always did (1–30 s backoff); `4403`/`4404` stop the Linux client's
  reconnect loop as before.

## Event bridge

While a Canvas Cloud is linked, one background task (`crates/canvas-link/src/bridge.rs`)
holds an SSE stream on `GET {url}/v1/apps/self/events` with the app key:

- Each `event: canvas` with `data: {"subject","user_id","canvas_id","kind"}` is published to
  the Copper user whose id is `subject` (falling back to `user_id`) as the core
  `Event::Canvas {canvas_id, kind}` — Copper's `GET /v1/sync/events` then carries the same
  frame as before the split:

  ```
  event: canvas
  data: {"type":"canvas","canvas_id":"…","kind":"member_added"}
  ```

- `Last-Event-ID` resume: the bridge sends the last event id it saw when it reconnects (reset
  when the link points at a new URL). When Canvas Cloud cannot resume from it (it restarted,
  or the bridge was away longer than its 4096-event buffer) it sends `event: resync`; the
  bridge then sends `event: resync` (`data: {"type":"resync","reason":"canvas"}`) to every
  connected Copper user, who refetch as after a lagged stream, followed by
  `event: canvas` (`data: {"type":"canvas","kind":"resync"}`, no `canvas_id`) for clients
  that refresh their canvas list and invites only on `canvas` events (Copper for Mac), so no
  canvas change is missed silently.
- Reconnects with exponential backoff, 1 s → 30 s; treats 45 s without any bytes (Canvas
  Cloud sends a keep-alive every 15 s) as a dead stream; follows link changes within 5 s.

The `kind`s are Canvas Cloud's, unchanged from 0.8:

| `kind` | Sent to | When |
|---|---|---|
| `created` | owner | canvas created |
| `renamed` | members | rename |
| `deleted` | former members | delete |
| `invited` | the invitee (if they have an account) | new invite, or a re-send reminder (at most every 30 s per invite) |
| `invite_revoked` | the invitee (if they have an account) | owner or inviter revoked a pending invite |
| `invite_declined` | the inviter | invite declined |
| `member_added` | members | invite accepted / share link joined |
| `member_removed` | members + the removed user | member removed / left |
| `update` | members | document changed (at most once per 2 s per canvas, plus one trailing) |
| `mention` | each newly mentioned member | someone @mentioned you in the canvas chat |
| `mention_read` | you | your mentions on that canvas were marked read |
| `resync` (copper-cloud, 0.11.0; no `canvas_id`) | every connected user | the bridge could not resume Canvas Cloud's event stream: refetch canvases, invites and mentions |

Only users of this app receive events through the bridge, which is every Copper user.

`GET /v1/people` is answered by Canvas Cloud for the signed-in user as this app's caller: it
lists every other user linked to this app (every provisioned Copper account), never Canvas
Cloud's own sign-ups or another app's users (Canvas Cloud's `docs/canvas.md`, `GET /people`).

## User provisioning

Every Copper account exists on Canvas Cloud as an app user (`crates/canvas-link/src/provision.rs`):

- **subject = id hint = the Copper `users.id`** (UUID string). Canvas Cloud creates new app
  users with the hinted id, so Canvas Cloud user ids equal Copper user ids: member lists,
  chat `authorId`s inside documents and mention ids stay valid, and migrated canvases need no
  id rewriting.
- What is pushed: `email`, `display_name` (`null` when empty), `disabled`, `id`.

```http
PUT /v1/apps/self/users/0192f1d0-7a3e-7c41-9a55-2f7c1d2e9b10
{"email":"ana@example.com","display_name":"Ana","disabled":false,"id":"0192f1d0-7a3e-7c41-9a55-2f7c1d2e9b10"}

POST /v1/apps/self/users:batch
{"users":[{"subject":"0192f1d0-…","email":"ana@example.com","display_name":"Ana","disabled":false,"id":"0192f1d0-…"}]}
```

When users are pushed:

| Trigger | What happens |
|---|---|
| link (`canvas link`, portal, `PUT /admin/api/canvas-link`) | every user, `users:batch` in chunks of 1000 (the CLI waits for it unless `--no-sync`; the admin API runs it in the background) |
| `copper-cloud canvas sync-users [--prune]` | every user, now; with `--prune` also deletes Canvas Cloud users of this link whose Copper account no longer exists (and the canvases they own there) |
| a new account (signup, SSO sign-in, `admin create-user`, admin portal) | queued in the creating transaction by the `canvas_link_user_created` trigger on `users` (migration 0030), so even another process's insert counts; pushed within seconds (the outbox is polled every 5 s) and retried while Canvas Cloud is down |
| Canvas Cloud answers `401 unknown_user` to a proxied request | just-in-time `PUT` of that user, then the request is retried once (a user that no longer exists in Copper is deleted on Canvas Cloud instead) |
| reconcile loop — at start, on every link change, then every 10 min | users whose email / display name / disabled flag changed since this process last pushed them (kept in memory, so after a restart everyone is pushed once); newly disabled users are also disconnected (`POST /v1/apps/self/users/{id}/disconnect`); app users whose Copper account no longer exists are only **counted and logged** (`canvas users reconciled … orphaned=N`) — reconcile never deletes, so restoring an older copper-cloud backup cannot wipe newer users' canvases; delete them deliberately with `canvas sync-users --prune` |
| admin API `PATCH /admin/api/users/{id}` | outbox `upsert_user`, plus `disconnect_user` when it disables the user |
| admin API `DELETE /admin/api/users/{id}` | outbox `delete_user` — Canvas Cloud deletes the app user and the canvases they own |

**Outbox** (`canvas_link_outbox`): admin-API changes are queued in the same transaction as the
change (only while a link exists) and pushed right after commit; failures are retried with
backoff (30 s × 2^attempts, at most 1 h apart) and dropped with a warning after 50 attempts.
The worker also polls every 30 s. Unlinking empties the outbox (it targeted that server).
Changes made with `copper-cloud admin …` on the shell do not use the outbox; the server's
reconcile loop picks up edits and disables within 10 minutes. A user deleted on the shell stays
on Canvas Cloud (with their canvases) until you run `copper-cloud canvas sync-users --prune`.

**Id mismatches.** If Canvas Cloud already has an account with a Copper user's email and the
app trusts email (`apps create` default), Canvas Cloud links that existing account — with its
own id — to the subject. The proxy still works for that person, but ids inside documents no
longer match theirs. `canvas link` / `canvas sync-users` report each such user and exit `1`;
`canvas migrate` refuses to import anything until every user maps to its own id. Link a Canvas
Cloud whose accounts do not overlap with Copper's, or create the app with `--no-trust-email`
(the user then fails with `409 email_taken` instead).

## TLS to Canvas Cloud

| URL | Verification |
|---|---|
| `https://…` (default) | the web PKI: the bundled Mozilla root set (`webpki-roots`) — **not** the operating system's trust store, so a private CA is not trusted; use a pin |
| `https://…` + fingerprint pin | exactly one leaf certificate: its SHA-256 (DER) must equal the pin; names, issuer and validity dates are not checked (the TLS handshake signature still is) — for a self-signed Canvas Cloud |
| `http://…` | allowed only for a loopback host (`localhost`, `127.0.0.0/8`, `[::1]`): a Canvas Cloud on the same machine |

REST, the event stream and the WebSocket all use the same policy, over HTTP/1.1 (rustls,
`ring`). The URL may carry a path prefix (`https://host[:port][/prefix]`), never credentials,
a query or a fragment. A pin is 64 hex characters, any case, `:` separators allowed. To read
the fingerprint of a running Canvas Cloud:

```sh
timeout 10 openssl s_client -connect canvas.example.com:443 -servername canvas.example.com </dev/null 2>/dev/null \
  | openssl x509 -outform DER | sha256sum
```

Re-link with the new pin whenever the Canvas Cloud certificate changes; until then every
canvas request is `502 canvas_upstream` (`"Canvas Cloud certificate does not match the pinned
fingerprint"`).

## Linking

### On the Canvas Cloud

A Copper operator sets, on the Canvas Cloud (`/etc/canvas-cloud/canvas-cloud.toml`; see the
[Canvas Cloud README](https://github.com/copper-browser/canvas-cloud)):

```toml
# Copper canvases contain copper:// links (link shapes). Canvas Cloud accepts only
# http, https and mailto by default; without this, ops that set a copper:// URL are rejected.
[ops]
extra_url_schemes = ["copper"]
```

(environment: `CANVAS_CLOUD_OPS__EXTRA_URL_SCHEMES=copper`), and creates the app copper-cloud
signs in as:

```sh
sudo canvas-cloud apps create --name "Copper Cloud" --allow-import   # prints cva_… once
```

`--allow-import` lets the app create canvases with `POST /v1/apps/self/import`, which
`copper-cloud canvas migrate` needs; it is harmless to keep. Add `--no-trust-email` if the
Canvas Cloud also has its own (non-Copper) accounts — see [id mismatches](#user-provisioning).
`canvas-cloud apps list`, `apps rotate <id>` (new key, old one stops working) and
`apps revoke <id>` manage it later.

For the same-host setup, also:

```toml
listen = "127.0.0.1:8480"

[tls]
mode = "off"
```

### CLI

```sh
umask 077
sudo canvas-cloud apps create --name "Copper Cloud" --allow-import > /root/canvas-app.key   # on the Canvas Cloud host; or use - to read stdin
sudo copper-cloud canvas link --url http://127.0.0.1:8480 --key-file /root/canvas-app.key
# self-signed Canvas Cloud on another host:
sudo copper-cloud canvas link --url https://canvas.example.com:8443 --key-file /root/canvas-app.key \
    --fingerprint 9F:86:D0:81:…
shred -u /root/canvas-app.key
```

```
linked http://127.0.0.1:8480 (Canvas Cloud 0.1.0, key …x7Qa)
provisioned 12/12 users on Canvas Cloud
```

| Command | What it does |
|---|---|
| `copper-cloud canvas link --url URL --key-file PATH [--fingerprint HEX] [--no-sync]` | checks `GET /v1/info` and `GET /v1/apps/self/stats` with the key (5 s each); only if both pass stores the link (enabled) and provisions every user (skipped with `--no-sync`). Re-running it replaces the link (key rotation, new URL, new pin). The key is read from the file (`-` = stdin), never from argv; 8–4096 printable ASCII characters. Exit `1` if a user could not be provisioned or got a different id (the link stays saved) |
| `copper-cloud canvas unlink` | removes the link and the outbox ([Unlinking](#unlinking)) |
| `copper-cloud canvas status` | the link (the key is never shown), a fresh reachability check and the migration ledger summary; exit `1` when linked but unreachable |
| `copper-cloud canvas sync-users [--prune]` | provisions every user now (works with a disabled link too); `--prune` then deletes Canvas Cloud users of this link that no longer exist in Copper, with their canvases there |
| `copper-cloud canvas migrate [--dry-run] [--only ID]… [--replace] [--concurrency N] [--json]` | copies legacy canvases into the linked Canvas Cloud — [canvas-migration.md](canvas-migration.md) |

The CLI talks to the database directly (run it as root, like the other admin commands); a
running server picks up a CLI change within 5 seconds. These commands need copper-cloud's
database migrations applied (`copper-cloud migrate`; `serve` and `install.sh` do it).

### Portal

**Canvas Cloud** in the admin portal: link status (reachable / unreachable / turned off,
version, last check, linked since, publicly trusted or pinned certificate), **Test
connection**, **Unlink**, the event bridge ("Live updates": connected, last event, last
error), the migration summary (migrated / failed / legacy canvases, with the command to run),
and a form to link or replace the app key (write-only: the stored key is never shown). It
shows `canvas-cloud apps create --name "Copper Cloud" --allow-import` for creating the key.
The **Canvases** page and the overview show an "unlinked" state when no Canvas Cloud is
linked.

### Admin API

Same conventions as the rest of [admin-api.md](admin-api.md) (admin session cookie,
`X-Requested-With: copper-cloud-portal` on non-GET, `{"error","message"}` errors). Every mutation is audited.

#### `GET /admin/api/canvas-link`

Re-probes Canvas Cloud when the last check is older than 15 s.

```json
{
  "linked": true,
  "url": "http://127.0.0.1:8480",
  "enabled": true,
  "tls_fingerprint": null,
  "linked_at": "2026-10-19T18:02:11Z",
  "updated_at": "2026-10-19T18:02:11Z",
  "updated_by": "cli",
  "status": { "reachable": true, "version": "0.1.0", "error": null, "checked_at": "2026-10-19T18:30:00Z" },
  "bridge": { "connected": true, "last_event_at": "2026-10-19T18:29:41Z", "last_error": null },
  "migrations": { "migrated": 36, "failed": 0, "legacy_canvases": 36 }
}
```

Not linked: `linked: false`, `enabled: false`, every other field `null`, `status` all `null`,
`bridge` `{"connected": false, "last_event_at": null, "last_error": null}`, and `migrations`
as above. `updated_by` is the admin's email or `"cli"`. `tls_fingerprint` is the lower-case
pin or `null`. `migrations` counts ledger rows `migrated` / `failed` and the rows of the legacy
`canvases` table. The app key is never returned.

#### `PUT /admin/api/canvas-link`

```json
{ "url": "https://canvas.example.com", "app_key": "cva_…", "tls_fingerprint": null }
```

Validates the input, probes `GET /v1/info` + `GET /v1/apps/self/stats` with the key (5 s
each), and only then stores the link (replacing any previous one; `linked_at` is kept when the
URL is unchanged) and starts provisioning every user in the background. `200` → the
`GET` body. `400 bad_request` with a human message when the URL, key or fingerprint is invalid,
or the probe fails (`"could not reach Canvas Cloud at …: …"`, `"Canvas Cloud at … rejected the
app key (…)"`, `"Canvas Cloud at … did not pass the check (…)"`); nothing is stored then.
Audit: `canvas_link.set` (detail: `url`, `tls_fingerprint`, `key_last4`, `version`).

#### `DELETE /admin/api/canvas-link`

Removes the link and the outbox. `200 {"ok": true}` (also when nothing was linked). Audit:
`canvas_link.remove` (only when something was linked).

#### `POST /admin/api/canvas-link/test`

```json
{ "ok": true, "version": "0.1.0", "latency_ms": 4, "error": null }
{ "ok": false, "version": null, "latency_ms": null, "error": "Canvas Cloud is unreachable (Connection refused (os error 111))" }
```

Always `200`; not linked → `ok: false`, `error: "no Canvas Cloud is linked"`. Audit:
`canvas_link.test` (when linked).

#### Endpoints backed by the link

| Endpoint | Source | When unlinked / unreachable |
|---|---|---|
| `GET /admin/api/overview` `counts.canvases`, `counts.live_rooms`, `counts.live_peers` | `GET /v1/apps/self/stats` | `null` |
| `GET /admin/api/users` `canvas_count` | `GET /v1/apps/self/users` (cached 30 s) | `null` |
| `GET /admin/api/canvases` | `GET /v1/apps/self/canvases` (same item shape as before, plus top-level `linked`) | unlinked: `200` empty list with `"linked": false`; unreachable: `502 canvas_upstream` |
| `DELETE /admin/api/canvases/{id}` | `DELETE /v1/apps/self/canvases/{id}` (Canvas Cloud closes the room with `4404` and notifies members) | `503 canvas_unavailable` / `502 canvas_upstream`; unknown id `404` |
| `PATCH` / `DELETE /admin/api/users/{id}` | outbox → Canvas Cloud ([provisioning](#user-provisioning)) | queued, retried |

## Status and doctor

```sh
sudo copper-cloud canvas status
```

```
canvas link: http://127.0.0.1:8480
  enabled:      true
  pinned cert:  -
  linked at:    2026-10-19T18:02:11Z
  updated:      2026-10-19T18:02:11Z by cli
  reachable:    yes (Canvas Cloud 0.1.0)
  migrations:   36 migrated, 0 failed, 36 legacy canvases
```

`copper-cloud doctor` has a `canvas-link` check:

```
  ok    canvas-link http://127.0.0.1:8480 (Canvas Cloud 0.1.0, 12 users, 36 canvases, 4 ms)
  warn  canvas-link no Canvas Cloud linked: canvases are unavailable — copper-cloud canvas link --url … --key-file …
  warn  canvas-link http://127.0.0.1:8480 is linked but disabled
  FAIL  canvas-link http://127.0.0.1:8480: Canvas Cloud is unreachable (Connection refused (os error 111))
  FAIL  canvas-link https://canvas.example.com: Canvas Cloud answered 401 <code>: <message>
```

`ok` = linked and both probes passed (the app key is accepted); `warn` = not linked (the
instance runs, canvases are `503`); `FAIL` = linked but unreachable or the key is refused
(non-zero exit). The event bridge lives in the server process, so its state is in the portal
(**Live updates**) and `GET /admin/api/canvas-link` `bridge`, not in the CLI or doctor.

## Unlinking

`copper-cloud canvas unlink`, portal **Unlink**, or `DELETE /admin/api/canvas-link`:

- The `canvas_link` row and every queued outbox item are deleted. **Nothing on Canvas Cloud
  is deleted** — users, canvases and the app stay; revoke the app there
  (`canvas-cloud apps revoke <id>`) if it should stop working.
- From then on (CLI: within 5 s) every canvas route answers `503 canvas_unavailable`, the
  event bridge and the reconcile loop stop, and the overview/people canvas counts are `null`.
- Canvas WebSockets that are already open stay connected to Canvas Cloud until they close;
  restart copper-cloud (`systemctl restart copper-cloud`, which closes them with `1001`) to
  cut them at once.
- Copper users keep their local copies and see canvas requests fail with the message above;
  nothing is removed from their canvas list (a failed fetch is never adopted).
- Linking the **same** Canvas Cloud again restores everything as it was. Linking a
  **different** one shows users that server's canvases — for Copper a successful list without
  their old canvases means those canvases are gone, so their tabs close. Migrate first.

## Security model

- **Copper users authenticate to copper-cloud only** (instance gate + session), exactly as
  before. Canvas Cloud trusts copper-cloud's `X-Canvas-User` assertion for this app's users;
  it never sees a Copper session token or gate credential (the `Authorization` and
  `X-Copper-Instance` headers, `?token=` and `?instance=` are stripped).
- **What the app key can do** on Canvas Cloud: act as any user of this app (every Copper
  user) on the whole canvas API; create, update, disconnect and delete this app's users (with
  `trust_email`, provisioning an email that already has a Canvas Cloud account links that
  account to the app); list, delete and digest the canvases those users own; read every
  canvas event of those users; import canvases (only with `allow_import`). It cannot act as users that are not linked
  to this app, or use Canvas Cloud's admin CLI (but those users' names and emails are visible
  through `GET /v1/people`, as to any Canvas Cloud user). Treat it like the
  `master_key`: whoever holds it can read and change every Copper user's canvases.
- **Where it is stored**: `canvas_link.app_key_sealed`, AES-256-GCM under a random per-row
  data key (`data_key_wrapped`, wrapped by the KEK derived from `master_key`) with AAD
  `canvas_link:app_key` — the same envelope as the intelligence keys. A database dump without
  the `master_key` does not reveal it. It is never returned by the admin API, printed by the
  CLI (only its last four characters), written to logs or audit rows (`key_last4` only), or
  passed on argv; the outgoing header is marked sensitive.
- **Who can change the link**: portal admins (session + CSRF) and root on the host (the CLI
  reads the 0600 config). Both are audited (`canvas_link.set` / `remove` / `test`).
- **Transport**: HTTPS verified against the web PKI or a pinned leaf; plain HTTP only to a
  loopback address. Run a same-host Canvas Cloud with `listen` on `127.0.0.1` so it is not
  reachable from the network without TLS.
- **Data at rest**: canvas documents, chat and mention excerpts are encrypted by Canvas Cloud
  under its own `master_key`, in its own database. copper-cloud keeps the legacy canvas tables
  (sealed under copper-cloud's `master_key`) untouched for rollback; see
  [canvas-migration.md](canvas-migration.md#after-the-migration).
- **Logs** never contain the app key, session tokens, document contents or update bytes.

## Metrics and logs

| Metric | Type | Meaning |
|---|---|---|
| `canvas_link_ws_pipes` | gauge | proxied canvas WebSockets open now |
| `canvas_link_events_total` | counter | canvas events bridged into `/v1/sync/events` |

Proxied requests appear in the normal `request` log lines and `http_*` metrics (route
`/v1/canvases/{*rest}` etc.); `502`/`503` carry `span.error` `canvas_upstream` /
`canvas_unavailable` and the message. Room, peer and document metrics are Canvas Cloud's.
Useful log filters:

```sh
journalctl -u copper-cloud -o cat | jq -c 'select(.span.error=="canvas_upstream" or .span.error=="canvas_unavailable") | .span | {path, status, error_message}'
journalctl -u copper-cloud -o cat | jq -c 'select(.message | test("canvas"; "i")) | {level, message, error}'
```

Notable messages: `canvas event bridge connected` / `disconnected`, `canvas users
reconciled`, `canvas link outbox processed`, `provisioned user on Canvas Cloud`, `Canvas Cloud
answered 401 to the app; check the canvas link's app key`, `Canvas Cloud linked this Copper user
to an existing account with a different id; chat author ids will not match`.

## Code map

| Path | What |
|---|---|
| `crates/canvas-link/src/lib.rs` | `CanvasLink` handle: router, settings cache, background tasks, `503`/`502` bodies |
| `crates/canvas-link/src/proxy.rs` | REST proxy, header/query rules, `401` handling |
| `crates/canvas-link/src/ws.rs` | WebSocket pipe |
| `crates/canvas-link/src/bridge.rs` | SSE client + event bridge |
| `crates/canvas-link/src/provision.rs` | batch/JIT provisioning, reconcile loop, outbox |
| `crates/canvas-link/src/settings.rs` | the sealed `canvas_link` row, URL/key/pin validation |
| `crates/canvas-link/src/upstream.rs`, `tls.rs` | HTTP/WS client, timeouts, TLS pinning |
| `crates/canvas-link/src/admin.rs` | admin API / CLI / doctor helpers, audit |
| `crates/canvas-link/src/landing.rs` | `GET /join/{token}` landing page |
| `crates/canvas-link/src/info.rs` | adds `canvas` to `GET /v1/info` |
| `crates/canvas-migrate/` | `copper-cloud canvas migrate` ([canvas-migration.md](canvas-migration.md)) |
| `crates/copper-cloud/migrations/0030_canvas_link.sql` | `canvas_link`, `canvas_link_outbox`, `canvas_migrations` |

## Testing

`cargo test -p copper-cloud --test canvas_link` runs the link against an in-process mock Canvas
Cloud (axum) on the native Postgres test database (`COPPER_CLOUD_TEST_DATABASE_URL`, default
`postgres://localhost:5432/copper_cloud_test_core`): admin link lifecycle, request/response
forwarding and header rules, upstream failures and `401` mapping, the WebSocket pipe incl.
close codes, the event bridge into `/v1/sync/events`, admin canvases/overview/people and user
hooks, reconcile, and the public landing page. `cargo test -p copper-cloud-canvas-link` covers
the client, TLS pinning and SSE parsing without a database.
