# Canvases

Since 0.11.0 canvases are served by a linked **Canvas Cloud**
([copper-browser/canvas-cloud](https://github.com/copper-browser/canvas-cloud)), not by
copper-cloud. The canvas API moved there verbatim and is documented in Canvas Cloud's
[docs/canvas.md](https://github.com/copper-browser/canvas-cloud/blob/main/docs/canvas.md).

What remains in copper-cloud is the **canvas link** — a reverse proxy that serves the same
routes under `/v1` to signed-in Coppers, acting as the user, plus the event bridge into
`GET /v1/sync/events`, user provisioning and the `/join/{token}` landing page:
[canvas-link.md](canvas-link.md). Moving an existing deployment's canvases:
[canvas-migration.md](canvas-migration.md).

Every route below is still reachable at `/v1/…` on copper-cloud with the same request and
response; while no Canvas Cloud is linked (or it is unreachable) they answer
`503 canvas_unavailable` (`502 canvas_upstream`) — see
[canvas-link.md › Status codes](canvas-link.md#status-codes).

## Model

Moved: [Canvas Cloud › Model](https://github.com/copper-browser/canvas-cloud/blob/main/docs/canvas.md#model).
Canvas user ids are Copper user ids ([provisioning](canvas-link.md#user-provisioning)).

## REST

Moved: [Canvas Cloud › REST](https://github.com/copper-browser/canvas-cloud/blob/main/docs/canvas.md#rest);
proxied unchanged ([the proxy](canvas-link.md#the-proxy)).

### `GET /canvases`

Moved to [Canvas Cloud › REST](https://github.com/copper-browser/canvas-cloud/blob/main/docs/canvas.md#rest).

### `POST /canvases {name}` → `201` canvas

Moved to [Canvas Cloud › REST](https://github.com/copper-browser/canvas-cloud/blob/main/docs/canvas.md#rest).

### `GET /canvases/{id}` → canvas · `PATCH /canvases/{id} {name}` → canvas · `DELETE /canvases/{id}` → `{"ok":true}`

Moved to [Canvas Cloud › REST](https://github.com/copper-browser/canvas-cloud/blob/main/docs/canvas.md#rest).

### `GET /canvases/{id}/members`

Moved to [Canvas Cloud › REST](https://github.com/copper-browser/canvas-cloud/blob/main/docs/canvas.md#rest).

### `DELETE /canvases/{id}/members/{user_id}` → `{"ok":true}`

Moved to [Canvas Cloud › REST](https://github.com/copper-browser/canvas-cloud/blob/main/docs/canvas.md#rest).

### `POST /canvases/{id}/invites {email}` → `201` new invite · `200` already pending (re-send)

Moved to [Canvas Cloud › REST](https://github.com/copper-browser/canvas-cloud/blob/main/docs/canvas.md#rest).

### `GET /canvases/{id}/invites`

Moved to [Canvas Cloud › REST](https://github.com/copper-browser/canvas-cloud/blob/main/docs/canvas.md#rest).

### `DELETE /canvases/{id}/invites/{invite_id}` → `204`

Moved to [Canvas Cloud › REST](https://github.com/copper-browser/canvas-cloud/blob/main/docs/canvas.md#rest).

### `GET /invites`

Moved to [Canvas Cloud › REST](https://github.com/copper-browser/canvas-cloud/blob/main/docs/canvas.md#rest).

### `POST /invites/{id}/accept` → canvas (your view, `role: editor`) · `POST /invites/{id}/decline` → `{"ok":true}`

Moved to [Canvas Cloud › REST](https://github.com/copper-browser/canvas-cloud/blob/main/docs/canvas.md#rest).

### Canvas share links

Moved: [Canvas Cloud › Canvas share links](https://github.com/copper-browser/canvas-cloud/blob/main/docs/canvas.md#canvas-share-links).
Tokens of links created before 0.11.0 keep working after [migration](canvas-migration.md#what-is-preserved).

### `GET /canvases/{id}/state[?sv=<base64>]`

Moved to [Canvas Cloud › REST](https://github.com/copper-browser/canvas-cloud/blob/main/docs/canvas.md#rest).

### `GET /canvases/{id}/read[?full=true&ids=a,b&types=sticky,text]`

Moved to [Canvas Cloud › REST](https://github.com/copper-browser/canvas-cloud/blob/main/docs/canvas.md#rest);
the format is in [Canvas Cloud › Read](https://github.com/copper-browser/canvas-cloud/blob/main/docs/canvas-protocol.md#read).

### `GET /people`

Moved to [Canvas Cloud › REST](https://github.com/copper-browser/canvas-cloud/blob/main/docs/canvas.md#rest).
Through copper-cloud it lists the instance's other Copper accounts (every Copper user is
provisioned).

### `POST /canvases/{id}/ops`

Moved to [Canvas Cloud › REST](https://github.com/copper-browser/canvas-cloud/blob/main/docs/canvas.md#rest)
and [Canvas Cloud › Ops](https://github.com/copper-browser/canvas-cloud/blob/main/docs/canvas-protocol.md#ops).
The proxy accepts request bodies up to 4 MiB here and allows 120 s.

### Chat and mentions

Moved: [Canvas Cloud › Chat and mentions](https://github.com/copper-browser/canvas-cloud/blob/main/docs/canvas.md#chat-and-mentions).

#### `POST /canvases/{id}/mentions {message_id, user_ids, excerpt}` → `200`

Moved to [Canvas Cloud › Chat and mentions](https://github.com/copper-browser/canvas-cloud/blob/main/docs/canvas.md#chat-and-mentions).

#### `GET /mentions[?unread=1&limit=n]`

Moved to [Canvas Cloud › Chat and mentions](https://github.com/copper-browser/canvas-cloud/blob/main/docs/canvas.md#chat-and-mentions).

#### `POST /mentions/read {ids} | {canvas_id}` → `{"ok": true, "updated": n}`

Moved to [Canvas Cloud › Chat and mentions](https://github.com/copper-browser/canvas-cloud/blob/main/docs/canvas.md#chat-and-mentions).

### `GET /canvases/{id}/ws` — live room

Moved to Canvas Cloud; copper-cloud pipes the WebSocket byte for byte, close codes included:
[canvas-link.md › WebSocket pipe](canvas-link.md#websocket-pipe).

### Change notifications

Still delivered as `event: canvas` on copper-cloud's `GET /v1/sync/events`, bridged from
Canvas Cloud: [canvas-link.md › Event bridge](canvas-link.md#event-bridge) (kinds table there).

## Rooms

Moved: [Canvas Cloud › Rooms](https://github.com/copper-browser/canvas-cloud/blob/main/docs/canvas.md#rooms).

### Limits

Canvas limits are Canvas Cloud's ([Limits](https://github.com/copper-browser/canvas-cloud/blob/main/docs/canvas.md#limits));
the proxy's own body limits and timeouts: [canvas-link.md › Bodies, limits and timeouts](canvas-link.md#bodies-limits-and-timeouts).

### Metrics

Room metrics are Canvas Cloud's ([Metrics](https://github.com/copper-browser/canvas-cloud/blob/main/docs/canvas.md#metrics));
copper-cloud exports `canvas_link_ws_pipes` and `canvas_link_events_total`
([canvas-link.md › Metrics and logs](canvas-link.md#metrics-and-logs)).

## Web share-link landing page

Still served by copper-cloud, unchanged: `GET /join/{token}` (no gate, no database, no
Canvas Cloud call) hands off to `copper://canvas/join/{token}?cloud={host}`
(`crates/canvas-link/src/landing.rs`).

## Security model

Canvas Cloud's: [Security model](https://github.com/copper-browser/canvas-cloud/blob/main/docs/canvas.md#security-model).
The link's (app key, sealing, TLS): [canvas-link.md › Security model](canvas-link.md#security-model).

## Testing

The canvas engine tests moved to Canvas Cloud; the link's tests: [canvas-link.md › Testing](canvas-link.md#testing).
