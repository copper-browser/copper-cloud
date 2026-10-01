# Operations

## Health

```sh
sudo copper-cloud doctor
```

```
copper-cloud doctor (0.1.0)
  ok    config      /etc/copper-cloud/copper-cloud.toml
  ok    database    PostgreSQL 16.4 (postgres://copper_cloud:***@127.0.0.1:5432/copper_cloud)
  ok    migrations  7 applied, none pending
  ok    accounts    3 users, 5 live sessions, signup enabled
  ok    tls         self-signed, fp=…, 3640 days left, names: 203.0.113.10, localhost, 127.0.0.1
  ok    listener    0.0.0.0:443 answering /healthz over HTTPS (HTTP/1.1 200 OK), fingerprint matches
  ok    metrics     http://127.0.0.1:9464/metrics
doctor: all checks passed
```

Exit status is non-zero if any check FAILs (warnings do not fail). Checks: config parses and
validates (and is not group/world-readable), database reachable, no pending migrations,
certificate present/parseable/not expiring within 30 days/matches its key (or ACME cache),
the listener answers `/healthz` presenting the pinned certificate, metrics bind. Doctor never
prints the link code or keys.

- `copper-cloud healthcheck --wait 60` — exit 0 once `/healthz` answers (scripts, installers).
- `GET /healthz` → `ok` (no key needed; suitable for load-balancer checks).
- `copper-cloud link-code` — print the link code again (root: the config is 0600).

## Service

```sh
sudo systemctl status copper-cloud
sudo systemctl restart copper-cloud     # SIGTERM → drains in-flight requests ≤ 10 s
sudo journalctl -u copper-cloud -f
```

`Restart=always` brings it back after crashes; startup waits up to 60 s for the database.

## Logs

JSON lines on stdout → journald. One `request` line per HTTP request:

```json
{"timestamp":"2026-10-01T18:46:22.889Z","level":"INFO","message":"request","target":"copper_cloud_core::observe",
 "span":{"method":"PUT","path":"/v1/sync/docs/spaces","route":"/v1/sync/docs/{domain}","ip":"198.51.100.7",
         "user_id":"0192…","status":200,"latency_ms":3.1,"name":"request"}}
```

5xx are logged at `error` with the internal error chain. Useful filters:

```sh
journalctl -u copper-cloud -o cat | jq -c 'select(.level=="ERROR")'
journalctl -u copper-cloud -o cat | jq -c 'select(.span.status==429)'
journalctl -u copper-cloud -o cat | jq -c 'select(.message=="login failed")'
```

Change verbosity with `log_level = "debug"` or `RUST_LOG=copper_cloud_core=debug,info`
(systemd: `systemctl edit copper-cloud` → `Environment=RUST_LOG=…`). `log_format = "pretty"`
for humans. Logs never contain tokens, passwords, keys or payloads.

## Metrics

Prometheus text on `metrics_bind` (default `127.0.0.1:9464`, loopback only; `"off"` disables):

```sh
curl -s http://127.0.0.1:9464/metrics
```

| Metric | Type | Labels |
|---|---|---|
| `http_requests_total` | counter | `route` (matched template, `unmatched`), `status` |
| `http_request_duration_seconds` | histogram | `route` |
| `sync_doc_bytes` | histogram | — (plaintext size of accepted doc writes) |
| `history_rows` | counter | — (entries appended) |
| `sse_subscribers` | gauge | — (open event streams) |
| `auth_rate_limited_total` | counter | — |
| `db_pool_size`, `db_pool_idle` | gauge | — (refreshed on scrape) |
| `process_uptime_seconds` | gauge | — |

Canvas metrics (rooms, peers, bytes) are documented in [canvas.md](canvas.md). To scrape
remotely, bind to a private interface (`metrics_bind = "10.0.0.5:9464"`) and firewall it, or
scrape through an SSH tunnel / node agent.

Starter alerts: `rate(http_requests_total{status=~"5.."}[5m]) > 0`, p99 of
`http_request_duration_seconds` > 1 s, `up == 0`, `db_pool_idle == 0` sustained.

## Backups

What to back up:

1. **The database** (`copper_cloud`): all users, sessions, sync docs, history, canvases.
   ```sh
   sudo -u postgres pg_dump -Fc copper_cloud > copper_cloud-$(date +%F).dump
   # restore into an empty database:
   sudo -u postgres pg_restore -d copper_cloud --clean --if-exists copper_cloud-2026-10-01.dump
   ```
   On RDS use automated snapshots / PITR.
2. **`/etc/copper-cloud/copper-cloud.toml`** — the `master_key` is required to decrypt every
   payload in the database, and `instance_key` is in every Copper's link. Store it separately
   from the DB dumps (password manager / secrets store).
3. **`/etc/copper-cloud/tls/`** (self-signed) — keeping the certificate keeps the pinned
   fingerprint valid. If lost, `tls-init` creates a new one and every Copper must re-link.

A restore onto a new VM: install with the same `COPPER_CLOUD_INSTANCE_KEY` and
`COPPER_CLOUD_MASTER_KEY` (or copy the config), copy `tls/`, restore the dump, `migrate`,
start.

## Upgrades

```sh
# from a release tarball
tar xzf copper-cloud-X.Y.Z-linux-$(uname -m).tar.gz && sudo ./install.sh
# or straight from GitHub
curl -fsSL https://raw.githubusercontent.com/copper-browser/copper-cloud/main/install.sh | sudo COPPER_CLOUD_VERSION=X.Y.Z sh
```

The installer swaps the binary atomically, runs `migrate`, restarts and waits for health.
Migrations are forward-only; take a DB backup first. Downgrading across a migration is not
supported. Clients reconnect automatically (SSE and canvas WebSockets drop during restart).

## Administration

```sh
sudo copper-cloud admin users
sudo copper-cloud admin create-user --email ada@example.com --password-stdin <<<'a strong password'
sudo copper-cloud admin reset-password --email ada@example.com --password-stdin
sudo copper-cloud admin disable-user --email ada@example.com    # blocks login, revokes sessions
sudo copper-cloud admin enable-user --email ada@example.com
sudo copper-cloud admin delete-user --email ada@example.com --yes   # deletes ALL their data
sudo copper-cloud admin disable-signup   # only admin-created accounts from now on
sudo copper-cloud admin enable-signup
```

`--password` also works but lands in shell history; prefer `--password-stdin`.

## Troubleshooting

| Symptom | Check |
|---|---|
| Copper says the certificate does not match | `copper-cloud link-code` and re-link; was `tls-init --force` run or the VM rebuilt without `tls/`? |
| every request is 401 `instance_key` | the client's key differs from `instance_key` (env `COPPER_CLOUD_INSTANCE_KEY` overrides the file) |
| 429 on login | 10 auth requests/min/IP; behind a proxy set `trust_proxy = true` |
| service restarts in a loop | `journalctl -u copper-cloud -n 50`: DB unreachable, bad config, port in use |
| ACME never gets a certificate | DNS must point at the VM and :443 must be reachable from the internet; look for `acme` log lines; `tls.acme_staging = true` while testing |
| "pending migrations" in doctor | `sudo copper-cloud migrate` (serve also migrates at start) |
