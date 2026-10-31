# Changelog

Newest first. Each release also has fuller notes in `docs/vX.Y.Z.md`.

## 0.13.1

Device agent relay fixes ([notes](docs/v0.13.1.md)). No migrations; a binary swap either way.

- A pending request is sent again (same id, the time it has left) on every stream the phone
  opens, so a stream that dies mid-call no longer leaves the agent waiting out the 200 s
  deadline; the call fails after 15 s offline as before
  ([re-delivery](docs/api.md#get-v1melinksidframes--sse-mailbox)).
- At most 8 calls in flight per link; one more answers at once, `isError: true`, "Copper on
  <device> is busy with other calls; try again shortly." (status `busy`).
- Each relayed call waits in its own task: it ends at its deadline whether or not the agent reads
  its SSE answer; a slow reader loses progress notifications beyond 32 (never the response) and
  never holds up a frames POST or other calls.
- Disabling or deleting a user closes their phones' streams with `revoked`, fails their calls in
  flight (status `user_revoked`) and drops their rate limits: at once from the admin API, within
  10 s from `copper-cloud admin disable-user` / `delete-user`.

## 0.13.0

Agents can use a person's Copper for iOS through the cloud ([notes](docs/v0.13.0.md)). One new
migration (`0016_device_agent`); upgrading from 0.12.x is a binary swap.

- **Device agent relay** (`/v1/info` feature `device_agent`, [api.md](docs/api.md#device-agent-relay)):
  - device side (gate + session): `POST`/`GET /v1/me/links`, `GET`/`DELETE /v1/me/links/{id}`,
    the SSE mailbox `GET /v1/me/links/{id}/frames` and its upload
    `POST /v1/me/links/{id}/frames` (body up to 32 MiB), agent tokens
    `POST`/`GET /v1/me/agent-tokens` and `DELETE /v1/me/agent-tokens/{id}`;
  - agent side (no gate header, `Authorization: Bearer cat_…`): `GET /v1/agent/status` and
    `POST /v1/agent/mcp` — MCP Streamable HTTP (stateless) with JSON or SSE answers, 10 s
    keepalive comments, progress notifications, `cancel` to the device on a 200 s deadline or
    when the agent goes away, and an immediate answer when Copper is not open; 600
    requests/minute per token.
- Deleting a device (`DELETE /v1/devices/{id}`, admin API too) turns off its link and agent
  tokens.
- New metrics: `device_agent_streams`, `device_agent_calls_total{status}`,
  `agent_rate_limited_total`; one `agent call` log line per call, never arguments or results.

## 0.12.0 and earlier

See [v0.12.0](docs/v0.12.0.md) · [v0.11.1](docs/v0.11.1.md) · [v0.11.0](docs/v0.11.0.md) ·
[v0.10.0](docs/v0.10.0.md) · [v0.9.0](docs/v0.9.0.md) · [v0.8.1](docs/v0.8.1.md) ·
[v0.8.0](docs/v0.8.0.md) · [v0.7.0](docs/v0.7.0.md) · [v0.6.0](docs/v0.6.0.md) ·
[v0.5.0](docs/v0.5.0.md) · [v0.4.0](docs/v0.4.0.md) · [v0.3.0](docs/v0.3.0.md).
