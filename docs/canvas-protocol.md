# Canvas protocol

The canvas wire protocol, document schema, ops and read formats moved with the canvas server
to **Canvas Cloud**: [docs/canvas-protocol.md](https://github.com/copper-browser/canvas-cloud/blob/main/docs/canvas-protocol.md)
in [copper-browser/canvas-cloud](https://github.com/copper-browser/canvas-cloud). Nothing about
the protocol changed: copper-cloud 0.11.0 proxies it unchanged ([canvas-link.md](canvas-link.md)).

## Wire protocol

Moved: [Canvas Cloud › Wire protocol](https://github.com/copper-browser/canvas-cloud/blob/main/docs/canvas-protocol.md#wire-protocol).
copper-cloud's side is a byte-exact pipe: [canvas-link.md › WebSocket pipe](canvas-link.md#websocket-pipe).

### Connecting

Moved: [Canvas Cloud › Connecting](https://github.com/copper-browser/canvas-cloud/blob/main/docs/canvas-protocol.md#connecting).
Copper still connects to copper-cloud's `GET /v1/canvases/{id}/ws` with its gate credential
and session; copper-cloud opens the Canvas Cloud socket first and only then upgrades.

### Framing

Moved: [Canvas Cloud › Framing](https://github.com/copper-browser/canvas-cloud/blob/main/docs/canvas-protocol.md#framing).
Frames are not re-framed by the proxy (64 MiB message / 16 MiB frame caps).

### Flow

Moved: [Canvas Cloud › Flow](https://github.com/copper-browser/canvas-cloud/blob/main/docs/canvas-protocol.md#flow).

### Close codes

Moved: [Canvas Cloud › Close codes](https://github.com/copper-browser/canvas-cloud/blob/main/docs/canvas-protocol.md#close-codes).
The proxy forwards close codes and reasons unchanged and adds `1011` (Canvas Cloud connection
lost) and `1001` (copper-cloud shutting down).

## Document schema

Moved: [Canvas Cloud › Document schema](https://github.com/copper-browser/canvas-cloud/blob/main/docs/canvas-protocol.md#document-schema).

## Ops

Moved: [Canvas Cloud › Ops](https://github.com/copper-browser/canvas-cloud/blob/main/docs/canvas-protocol.md#ops).
Note for Copper operators: `copper://` URLs in link shapes need `ops.extra_url_schemes =
["copper"]` on the Canvas Cloud ([canvas-link.md › On the Canvas Cloud](canvas-link.md#on-the-canvas-cloud)).

### Result

Moved: [Canvas Cloud › Result](https://github.com/copper-browser/canvas-cloud/blob/main/docs/canvas-protocol.md#result).

### Example

Moved: [Canvas Cloud › Example](https://github.com/copper-browser/canvas-cloud/blob/main/docs/canvas-protocol.md#example).

## Read

Moved: [Canvas Cloud › Read](https://github.com/copper-browser/canvas-cloud/blob/main/docs/canvas-protocol.md#read).
