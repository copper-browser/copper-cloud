# Canvases

The `copper-cloud-canvas` crate (`crates/canvas`) serves collaborative whiteboards: a REST
API for canvases, members and invites, and one live Yjs room per open canvas that speaks the
standard `y-websocket` protocol. The wire protocol, document schema and ops/read JSON are in
[canvas-protocol.md](canvas-protocol.md).

Everything here is mounted under `/v1`, behind the instance-key gate
(`X-Copper-Instance`) and session auth (`Authorization: Bearer <token>` or `?token=`), like
the rest of the [API](api.md). Errors use the common `{"error","message"}` shape.

## Model

| Table | What |
|---|---|
| `canvases` | `id`, `owner_id`, `name`, `kind` (`personal`/`shared`), `doc_key_wrapped`, timestamps |
| `canvas_members` | `(canvas_id, user_id)`, `role` (`owner`/`editor`), `added_at` — the owner has a row too |
| `canvas_invites` | email invites: `email` (lower-cased), `invited_by`, `status` (`pending`/`accepted`/`declined`), `token` (hash, reserved) |
| `canvas_updates` | append-only Yjs update log: `seq bigserial`, `canvas_id`, `"update"` (sealed) |
| `canvas_snapshots` | one compacted state per canvas: `seq` (last update folded in), `state` (sealed) |

Migrations: `crates/copper-cloud/migrations/0100_canvases.sql` (the canvas crate owns
`0100`–`0199`).

- **Personal canvas**: every user has exactly one (`kind = personal`, name `Personal`),
  created on first use (`GET /canvases`, or any `/canvases/personal/...` route) — a unique
  partial index on `owner_id WHERE kind = 'personal'` makes concurrent first use safe. It can
  never be renamed, deleted, shared or left (`400`).
- **Roles**: `owner` (rename, delete, remove members) and `editor` (read/write the document,
  invite others, leave). Both can edit the board.
- **Access is membership**: every lookup joins `canvas_members` on the caller. A canvas you
  are not a member of — or that does not exist — is `404` on every route (no existence leak).
  A member who lacks the role for an action gets `403`.

`{id}` in every route below is a canvas UUID **or the literal `personal`**.

## REST

### `GET /canvases`

Your canvases, Personal first, then most recently updated.

```json
[
  {
    "id": "0192…", "name": "Personal", "kind": "personal", "role": "owner",
    "owner": { "id": "0192…", "email": "ann@example.com", "display_name": "Ann" },
    "member_count": 1,
    "created_at": "2026-10-01T12:00:00Z", "updated_at": "2026-10-01T12:05:00Z"
  }
]
```

`updated_at` follows document activity (bumped at most every 2 s while a room is busy).

### `POST /canvases {name}` → `201` canvas

Creates a `shared` canvas owned by you (owner member row included). `name` is trimmed,
control characters dropped, 1–200 characters. The document starts with `meta.name` and
`meta.createdBy`.

### `GET /canvases/{id}` → canvas · `PATCH /canvases/{id} {name}` → canvas · `DELETE /canvases/{id}` → `{"ok":true}`

Rename and delete are owner-only (`403` for editors) and `400` on the Personal canvas.
Rename also updates the document's `meta.name` (synced to connected peers). Delete removes
the canvas, members, invites, updates and snapshot (cascade) and closes its room: connected
sockets get close code `4404`.

### `GET /canvases/{id}/members`

```json
[{ "user_id": "…", "email": "…", "display_name": "…", "role": "owner", "added_at": "…" }]
```

Owner first, then by join time. Any member may list.

### `DELETE /canvases/{id}/members/{user_id}` → `{"ok":true}`

The owner removes anyone else; any member removes themselves (leave). The owner cannot leave
(`400` — delete the canvas instead). Removed users' open sockets close with `4403`.

### `POST /canvases/{id}/invites {email}` → `201` invite (`200` if already pending)

Owner or editor. The email is trimmed and lower-cased; inviting yourself is `400`, an
existing member `409`, the Personal canvas `400`. An email with no account yet is fine: the
invite waits until someone signs up with it. Even when the email already belongs to a user,
they must accept — nobody is added to a canvas without consent.

```json
{
  "id": "…", "canvas_id": "…", "canvas_name": "Roadmap", "email": "bob@example.com",
  "status": "pending",
  "invited_by": { "id": "…", "email": "ann@example.com", "display_name": "Ann" },
  "created_at": "…"
}
```

### `GET /canvases/{id}/invites`

Pending invites of a canvas (any member).

### `GET /invites`

Pending invites addressed to **your** email (case-insensitive), with canvas name and inviter.

### `POST /invites/{id}/accept` → canvas (your view, `role: editor`) · `POST /invites/{id}/decline` → `{"ok":true}`

Only the invitee can accept or decline; anything else (wrong user, already used, declined,
unknown) is `404`. Invites are single-use.

### `GET /canvases/{id}/state[?sv=<base64>]`

`{"state": "<base64 of encode_state_as_update_v1>"}` — the whole document as one Yjs update
(standard base64, padded). With `sv` (a base64 lib0-v1 state vector) only what that state
vector is missing is returned. Apply with `Y.applyUpdate(doc, bytes)`.

### `GET /canvases/{id}/read[?full=true&ids=a,b&types=sticky,text]`

The agent-friendly read format (see [canvas-protocol.md](canvas-protocol.md#read)), identical
to the page's `copperCanvas.read`. Text is cut to 500 characters unless `full=true`.

### `POST /canvases/{id}/ops`

```json
{ "ops": [ { "op": "add", "shape": { "type": "sticky", "text": "Hello" } } ],
  "as": { "name": "Planner", "color": "purple" },
  "near": { "x": 0, "y": 0 },
  "confirm": false }
```

Applies [canvas ops](canvas-protocol.md#ops) to the live document in one transaction, then
persists and broadcasts the change exactly like a WebSocket update — connected clients see
it immediately. Returns `{"applied": n, "ids": [...], "errors": [...]}` (`ids[i]` belongs to
op `i`, `null` when it failed). Lets remote agents draw without a client.

- `as` (optional): who is drawing. Shapes created get `by = as.name`; without `as`, `by` is
  your display name (or email). With `as`, the agent's entry in the `agents` map is set to
  `writing` with its cursor on the last thing touched, then back to `idle` 1.5 s later.
- `near`: where auto-placement looks for free space (default `0,0`).
- `confirm: true` is required for `{op:"clear"}` (or put `confirm: true` on the op).
- At most 500 ops per call; the body may be a bare array of ops too. Envelope problems
  (`ops` not an array, bad `as`, too many ops) are reported like the page does: `200` with
  `errors: [{"index": -1, …}]`. Malformed JSON is `400`.

### `GET /canvases/{id}/ws` — live room

WebSocket upgrade (HTTP/1.1 `GET`, or HTTP/2 extended `CONNECT`). Auth: the `Authorization`
header, or `?token=` for tools that cannot set headers; the `X-Copper-Instance` header is
still required. Non-members get `404` before the upgrade. Protocol details:
[canvas-protocol.md](canvas-protocol.md#wire-protocol).

Example URL: `wss://HOST:PORT/v1/canvases/personal/ws?token=…`. A stock
`y-websocket` `WebsocketProvider(serverUrl = "wss://HOST:PORT/v1/canvases", room = "<id>/ws",
doc, { params: { token } })` works as-is (its socket must also send the instance header).

### Change notifications

Members get `event: canvas` on `GET /v1/sync/events`
(`{"type":"canvas","canvas_id":"…","kind":"…"}`) — refetch the list or the canvas:

| `kind` | Sent to | When |
|---|---|---|
| `created` | owner | canvas created |
| `renamed` | members | rename |
| `deleted` | former members | delete |
| `invited` | the invitee (if they have an account) | new invite |
| `invite_declined` | the inviter | invite declined |
| `member_added` | members | invite accepted |
| `member_removed` | members + the removed user | member removed / left |
| `update` | members | document changed (at most once per 2 s per canvas, plus one trailing) |

## Rooms

One in-memory room per open canvas: a `yrs::Doc` behind a `tokio::sync::RwLock`, an
awareness table, and one `tokio::sync::broadcast` channel (256 frames) fanned out to peers.

- **Load**: decrypt the snapshot, then every update with `seq > snapshot.seq`, in order.
- **Write path** (WebSocket update, REST ops, rename): take the write lock → apply in one
  transaction → seal and `INSERT` the update the transaction produced → broadcast it to the
  other peers. The lock is held across the insert, so persisted order = applied order =
  broadcast order, and **nothing is broadcast before it is durable**. If the insert fails
  the in-memory document is discarded (reloaded from Postgres on next use) and all peers are
  disconnected (`1011`); y-websocket clients reconnect and their sync handshake re-sends
  whatever the server is missing, so no edit is lost.
- **Compaction**: every 200 persisted updates, `encode_state_as_update_v1` of the room doc is
  sealed into `canvas_snapshots` and the folded updates are deleted — one transaction.
- **Eviction**: a room with no peers and no REST use for 60 s is dropped from memory (it is
  only removed when nothing else holds it, so an in-flight write can never be orphaned).
- **Awareness** (cursors/presence) is relayed between peers and kept in memory only, never
  persisted. When a socket closes, the awareness states it published are removed and peers
  are told.
- **Backpressure**: each peer has a read loop and a write task. Replies go through a bounded
  per-peer queue (32); a peer that falls more than 256 broadcast frames behind, or does not
  accept a frame within 15 s, is disconnected (`1013`) and resyncs on reconnect. The server
  pings every 30 s and drops peers silent for 90 s.
- **Shutdown**: on the process shutdown signal every socket is closed with `1001`.

### Limits

| What | Limit |
|---|---|
| WebSocket message / frame | 8 MiB (the page stores images as `data:` URLs up to 2 MiB) |
| `POST /ops` body | 4 MiB; 500 ops per call |
| Other canvas request bodies | 64 KiB |
| Canvas name | 200 characters |
| Awareness clients per socket | 16 |
| Sticky/text body | 20 000 characters; titles/labels cut to 500 |

### Metrics

| Name | Type | Meaning |
|---|---|---|
| `canvas_rooms` | gauge | rooms in memory |
| `canvas_peers` | gauge | connected WebSocket peers |
| `canvas_updates_persisted_total` | counter | Yjs updates written to `canvas_updates` |
| `canvas_update_bytes_total` | counter | plaintext bytes of those updates |
| `canvas_compactions_total` | counter | snapshot compactions |
| `canvas_ws_bytes_in_total` / `canvas_ws_bytes_out_total` | counter | WebSocket payload bytes |

`copper_cloud_canvas::rooms_metrics()` returns the live `{rooms, peers}` counts.

## Security model

- **Authentication** is the core session (`AuthUser`); the instance-key gate runs first.
- **Authorization** is membership, checked on every REST call and at WebSocket upgrade.
  Removing a member or deleting a canvas disconnects affected sockets immediately. (A session
  revoked by logout does not cut an already-open socket; it fails on reconnect.)
- **Encryption at rest**: each canvas has a random 32-byte `doc_key`, wrapped by the KEK
  derived from `master_key` (`canvases.doc_key_wrapped`). Every update and snapshot is
  AES-256-GCM `nonce(12) || ciphertext || tag` under that key with AAD = the canvas id's 16
  raw bytes, so rows copied to another canvas fail to decrypt. Key rotation is out of scope
  (see [security.md](security.md)).
- **Invite tokens**: a random secret is generated per invite but only its SHA-256 is stored
  (reserved for future shareable links); v1 is email-invite only and never returns it.
- **Logging**: canvas and user ids, op counts and error kinds only — never tokens, document
  contents, awareness state or update bytes.
- **Input**: y-protocol frames are decoded with `yrs`; undecodable frames or rejected updates
  close that socket (`1007`) without touching the room. Ops are validated like the page
  validates them (types, colours, URLs, sizes); bad ops are skipped and reported.

## Testing

`cargo test -p copper-cloud-canvas` runs unit tests plus integration tests against the
native Postgres database `copper_cloud_test_canvas` (`postgres://localhost:5432/…`, current
OS user): REST lifecycle, Personal canvas, invites, 404 scoping, two-client sync + awareness,
persistence across eviction, compaction, ops fan-out, revocation.

A cross-implementation test drives the real page code (Yjs, `y-websocket`,
`Canvas/src/ops.ts`) with Bun and checks that the server's `/read` equals the page's
`readCanvas` on the same document:

```sh
COPPER_CANVAS_DIR=~/src/copper-canvasweb/Canvas \
  cargo test -p copper-cloud-canvas --test js_interop -- --ignored
```
