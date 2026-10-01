# Canvas protocol

The shared contract between the canvas page (TS, `Canvas/src`), the Copper host (Swift), and
the server (`crates/canvas`, Rust/`yrs`). REST routes and the room lifecycle are in
[canvas.md](canvas.md).

## Wire protocol

`GET /v1/canvases/{id}/ws` upgrades to a WebSocket that speaks **y-websocket** — the same
framing and message flow as the reference `y-websocket` server, implemented with
`yrs::sync` (the y-sync protocol: `DefaultProtocol`'s `Message`, `SyncMessage`,
`Awareness`).

### Connecting

```
GET /v1/canvases/<uuid|personal>/ws?token=<session token>      (or Authorization: Bearer …)
X-Copper-Instance: <instance key>
Upgrade: websocket
```

- `401` without a valid session/instance key, `404` if you are not a member, `400` if the
  request is not an upgrade.
- Any `?token=` / header combination the core `AuthUser` extractor accepts works.
- With `y-websocket`'s `WebsocketProvider`: `serverUrl = "wss://HOST:PORT/v1/canvases"`,
  `roomname = "<id>/ws"`, `params: { token }`. The socket must send the instance header (the
  Copper host sets it on its `URLSessionWebSocketTask` and relays frames to the page).

### Framing

Every WebSocket **binary** message carries one or more y-protocol messages back to back; each
starts with a lib0 var-uint message type. Text frames are ignored.

| Type | Name | Payload |
|---|---|---|
| `0` | sync | var-uint sub-type, then a var-length buffer: `0` SyncStep1 (state vector), `1` SyncStep2 (update), `2` Update (update) |
| `1` | awareness | var-length buffer: `count`, then per client `clientID` (var-uint), `clock` (var-uint), `state` (var-string JSON, `"null"` = gone) |
| `2` | auth | ignored (auth happens at upgrade) |
| `3` | awareness query | server answers with every known awareness state |

Updates are lib0 **v1** encoded (`Y.encodeStateAsUpdate`, `Y.applyUpdate`) — not v2.

### Flow

```
client                                  server
  │── HTTP upgrade (token, instance key) ──▶│  membership check, room loaded
  │◀── sync: SyncStep1(server SV) ─────────│  greeting
  │◀── awareness: current states ──────────│  (only if anyone is present)
  │── sync: SyncStep1(client SV) ─────────▶│
  │◀── sync: SyncStep2(what client lacks) ─│
  │── sync: SyncStep2(what server lacks) ─▶│  applied → persisted → broadcast as Update
  │── sync: Update … ─────────────────────▶│  applied → persisted → broadcast as Update
  │◀── sync: Update (others / REST ops) ───│
  │── awareness … ────────────────────────▶│  relayed to the other peers (not persisted)
  │◀── ping (every 30 s) ──────────────────│  any frame or pong within 90 s keeps it alive
```

The server never echoes a peer's own update back to it. Updates from REST (`/ops`, rename)
arrive as ordinary `Update` messages.

### Close codes

| Code | Meaning | Client should |
|---|---|---|
| `1000` | normal | — |
| `1001` | server shutting down | reconnect with backoff |
| `1007` | undecodable message or rejected update | fix the client; reconnecting resyncs |
| `1009` | message over 8 MiB | split the change |
| `1011` | server error (e.g. database write failed) | reconnect; the handshake resyncs |
| `1013` | too slow (fell 256 frames behind) | reconnect; the handshake resyncs |
| `4403` | you were removed from the canvas | stop; refresh the canvas list |
| `4404` | the canvas was deleted | stop; refresh the canvas list |

Client note: the initial `SyncStep2` carries the whole document and can be larger than
1 MiB once images are pasted — raise the client's maximum message size accordingly
(`URLSessionWebSocketTask.maximumMessageSize` defaults to 1 MiB).

## Document schema

One `Y.Doc` per canvas:

| Root | Type | Contents |
|---|---|---|
| `shapes` | `Y.Map<shapeId, Y.Map<prop, any>>` | one nested map per shape, so each property is last-writer-wins on its own |
| `agents` | `Y.Map<agentId, object>` | plain objects, replaced whole: `{name, color, cursor: {x,y} \| null, status: "idle"\|"thinking"\|"writing", updatedAt}` |
| `meta` | `Y.Map` | `name`, `createdBy` (user id) |
| `comments` | — | optional; not touched by the server |

Shape props (all shapes): `id`, `type`, `x`, `y`, `w`, `h` (integers when written by the page
or server), `color`, `by` (display name of the creator), `createdAt`, `updatedAt` (ms epoch),
`z` (stacking, higher on top).

| `type` | Extra props | Default size |
|---|---|---|
| `sticky` | `text` (**`Y.Text`**, markdown), `fontSize?` | 200×200 |
| `text` | `text` (**`Y.Text`**), `fontSize?`, `align?` (`left`/`center`/`right`) | 240×36 |
| `frame` | `title`, `image?: {src, naturalW, naturalH}` | 480×320 |
| `arrow` | `from`, `to`: `{x,y}` or `{ref: shapeId, side?: top\|right\|bottom\|left}`; `label?` | no box of its own (stored 0,0,0,0; drawn from its ends) |
| `image` | `src` (`data:image/…` ≤ 2 MiB, or `https:`), `naturalW`, `naturalH` | natural size, ≤ 480 wide |
| `link` | `url`, `title`, `favicon?` | 300×84 |

Colours: `yellow`, `pink`, `blue`, `green`, `purple`, `gray`, `white`, or `#rgb[a]` /
`#rrggbb[aa]`. Defaults: sticky yellow, image/link white, the rest gray.

Readers are tolerant: a plain-string `text` is read like a `Y.Text`; missing sizes take the
type default; unknown colours fall back to the default. Writers turn a plain-string body into
a `Y.Text` on the next edit and splice text minimally so concurrent typing merges.

## Ops

`POST /v1/canvases/{id}/ops` on the server and `copperCanvas.apply` on the page run the same
ops with the same validation and results. A batch runs in one transaction, in order; a bad
op is skipped and reported, the rest still apply.

```json
{ "ops": [ … ], "as": { "id": "agent:planner", "name": "Planner", "color": "#7c3aed" },
  "confirm": true, "near": { "x": 0, "y": 0 } }
```

`as` may also be a plain name (`"as": "Planner"` → id `agent:Planner`); object form defaults
`name` to `Agent` and `id` to `agent:<lowercased name>`. A bare array of ops is accepted.

| Op | Shape |
|---|---|
| `add` | `{op:"add", shape:{type, id?, x?, y?, w?, h?, color?, …}}` — omitted `x`/`y`: the nearest free spot (24 px gap, 32 px grid, walking outward from `near`, ties right → down → left → up). `id` (optional): 1–64 of `A–Z a–z 0–9 _ - : .`; server-generated ids are UUID v7. `z` defaults to the current max + 1. A link without a `title` gets its host name. Sizes clamp to the type's minimum. |
| `update` | `{op:"update", id, patch:{…}}` (`props` also accepted) — merges props; `image: null` removes a frame image. `id`, `type`, `by`, `createdAt`, `updatedAt` are ignored. |
| `move` | `{op:"move", id, dx, dy}` — a frame carries every shape lying wholly inside it; an arrow shifts its free ends. |
| `resize` | `{op:"resize", id, w, h}` — not for arrows; clamps to the minimum size. |
| `delete` | `{op:"delete", id}` — arrows ending on the shape are deleted too. |
| `connect` | `{op:"connect", from, to, label?, color?, fromSide?, toSide?, id?}` — adds an arrow between two (non-arrow) shapes. |
| `clear` | `{op:"clear", confirm:true}` — removes every shape. **Server:** requires `confirm: true` on the op or the request. |

Aliases: `text` on a frame or link is its `title`; `text`/`title` on an arrow is its
`label`; `title` on a sticky/text is its `text`. Ids may be written `shape:<id>`; endpoints
may be `"id"`, `{ref}`, `{id}` or `{x,y}`.

Validation (first failing prop is reported): unknown props for the type, non-finite numbers,
non-positive `w`/`h`, bad colours, `text` over 20 000 characters (titles/labels are cut to
500), `fontSize` outside 6–240, `src` not `data:image/…` (≤ 2 MiB) or `https:`, `url` not
`http(s)`/`mailto`/`copper` (bare hosts get `https://`, URLs are normalised), refs to missing
shapes or to arrows, an arrow joining a shape to itself.

### Result

```json
{
  "applied": 3,
  "ids": ["0192…", "0192…", null, "0192…"],
  "errors": [{ "index": 2, "op": "update", "error": "no shape nope" }]
}
```

`ids[i]` is the id op `i` created or touched — `null` when it failed, and for `clear`.
Request-level problems use `index: -1` (`"at most 500 ops per call (got 600)"`).

### Example

```json
{ "as": { "name": "Planner" },
  "ops": [
    { "op": "add", "shape": { "id": "goal", "type": "sticky", "text": "Ship v1", "color": "green" } },
    { "op": "add", "shape": { "id": "risk", "type": "sticky", "text": "Load test" } },
    { "op": "connect", "from": "goal", "to": "risk", "label": "blocked by" },
    { "op": "add", "shape": { "type": "frame", "title": "Q4", "x": -400, "y": -300, "w": 1200, "h": 700 } },
    { "op": "move", "id": "risk", "dx": 40, "dy": 0 }
  ] }
```

## Read

`GET /v1/canvases/{id}/read?full=true&ids=a,b&types=sticky,text` and `copperCanvas.read`
return the same shape (the server has no viewport or selection, so `viewport` is omitted and
`selection` is `[]`):

```json
{
  "canvas": { "id": "0192…", "name": "Roadmap", "kind": "shared" },
  "shapes": [
    { "id": "goal", "type": "sticky", "x": -100, "y": -100, "w": 200, "h": 200,
      "color": "green", "z": 1, "by": "Planner", "text": "Ship v1", "frame": "q4" },
    { "id": "a1", "type": "arrow", "x": 100, "y": 0, "w": 124, "h": 0, "color": "gray", "z": 3,
      "by": "Planner", "label": "blocked by", "from": { "ref": "goal" }, "to": { "ref": "risk" } },
    { "id": "pic", "type": "image", "x": 0, "y": 400, "w": 480, "h": 240, "color": "white", "z": 4,
      "src": "data:image/png (812 KB)", "naturalW": 960, "naturalH": 480 },
    { "id": "doc", "type": "link", "x": 500, "y": 0, "w": 300, "h": 84, "color": "white", "z": 5,
      "url": "https://example.com/", "title": "example.com" }
  ],
  "agents": [
    { "id": "agent:planner", "name": "Planner", "color": "hsl(212 62% 48%)",
      "cursor": { "x": 162, "y": 0 }, "status": "idle", "updatedAt": 1790000000000 }
  ],
  "selection": [],
  "count": 4
}
```

- Shapes are sorted top-to-bottom, then left-to-right (`y`, `x`, `id`); coordinates rounded.
- Per type: sticky/text → `text` (+ `fontSize`, `align`); frame → `title` (+ `image`); arrow
  → `label`, `from`, `to` and a box computed from its drawn path; image → `src`,
  `naturalW`, `naturalH`; link → `url`, `title`.
- `text`, `title`, `label` are cut to 500 characters plus `…` unless `full=true`. `data:`
  sources are described (`data:image/png (812 KB)`), never echoed (with `full`, only if
  ≤ 4 KiB).
- `frame` is the innermost frame wholly containing the shape.
- `agents`: entries written within the last 2 minutes, busiest first, then by name.
- `count` is every readable shape, before `ids`/`types` filtering.
