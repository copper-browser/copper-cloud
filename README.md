# Copper Cloud

Self-hosted sync server for the [Copper](https://github.com/copper-browser/Copper)
browser. One Rust daemon (`copper-cloud`), one Postgres. Install it on any Linux VM with one
command, or deploy to AWS with Terraform (`deploy/aws`).

What it syncs between your Coppers: spaces + pinned tabs, settings (safe subset), bookmarks,
history (append-only; people can delete theirs) and each device's open tabs. It can also hand
every signed-in Copper the instance's AI keys (Jev + LLM router), set once by the admin, so
nobody pastes keys by hand.

Shared canvases (live Yjs rooms, chat, mentions, invites, share links) are served by a linked
[Canvas Cloud](https://github.com/copper-browser/canvas-cloud) (since 0.11.0): copper-cloud
proxies the unchanged canvas API to it as the signed-in user, so Coppers need no update. See
[docs/canvas-link.md](docs/canvas-link.md); upgrading a 0.10.x (or older) instance with canvases:
[docs/canvas-migration.md](docs/canvas-migration.md).

People can join with **company single sign-on** (Google Workspace or any OpenID Connect
provider): share one `https://HOST/start` page, they install Copper and click *Continue with
Google* — no account to create, no password ([docs/sso.md](docs/sso.md)). **Agent
integrations** let a trusted service pair headless Coppers (e.g. one per bot) to its owners'
accounts ([docs/sso.md](docs/sso.md#agent-integrations)).

## Install on a VM (Ubuntu 22.04/24.04, Debian 12, Amazon Linux 2023, Fedora)

```sh
curl -fsSL https://raw.githubusercontent.com/copper-browser/copper-cloud/main/install.sh | sudo sh
```

or from a release tarball:

```sh
tar xzf copper-cloud-0.11.0-linux-x86_64.tar.gz && sudo ./install.sh
```

The installer sets up Postgres (unless you pass `DATABASE_URL`), a `copper-cloud` system user,
`/etc/copper-cloud/copper-cloud.toml` with fresh keys, a self-signed certificate (or Let's
Encrypt with `COPPER_CLOUD_DOMAIN=…`), a hardened systemd unit and a **portal admin account**,
then prints:

```
Admin portal: https://203.0.113.10/
  email:    admin@203.0.113.10
  password: <24 chars>   (shown once; also in /etc/copper-cloud/admin-credentials)

Access mode: directory
  Link code for your first Copper (Settings › Cloud › Connect; creates one account):

  copper-cloud://203.0.113.10:443/#k=ck_<access key>&fp=<certificate sha256>
```

Paste the link code into **Copper › Settings › Cloud › Connect** and create your account. To
let other people in, sign in to the **admin portal** and mint each person an access key
(Access keys › New key) — each comes with its own link code, revocable any time. A signed-in
Copper can also pair another Mac with a single-use pairing code. Choose the admin with
`COPPER_CLOUD_ADMIN_EMAIL` / `COPPER_CLOUD_ADMIN_PASSWORD`, or start in `open` mode (one shared
link code for everyone) with `COPPER_CLOUD_ACCESS_MODE=open`. Re-running the installer upgrades
in place and keeps keys, certificate, admins and data. Details: [docs/install.md](docs/install.md),
[docs/operations.md](docs/operations.md#admin-portal).

For canvases, install a [Canvas Cloud](https://github.com/copper-browser/canvas-cloud) (the
small setup: same host, `127.0.0.1:8480`, TLS off), create an app key there with
`canvas-cloud apps create --name "Copper Cloud" --allow-import`, and link it:

```sh
sudo copper-cloud canvas link --url http://127.0.0.1:8480 --key-file /root/canvas-app.key
```

Until then canvas requests answer `503 canvas_unavailable` and `doctor` warns. Details:
[docs/canvas-link.md](docs/canvas-link.md#linking).

AWS (EC2 + RDS + Elastic IP; link code and admin password in SSM): see
[deploy/aws/README.md](deploy/aws/README.md).

## Run locally (development)

Needs Rust (stable) and a native PostgreSQL (`createdb copper_cloud_dev`).

```sh
cargo run -- init-config --write /tmp/cc.toml
COPPER_CLOUD_DATABASE_URL=postgres://localhost:5432/copper_cloud_dev \
  cargo run -- --config /tmp/cc.toml serve
# another shell:
cargo run -- --config /tmp/cc.toml link-code     # copper-cloud://localhost:8443/#k=…&fp=…
COPPER_CLOUD_DATABASE_URL=postgres://localhost:5432/copper_cloud_dev \
  cargo run -- --config /tmp/cc.toml doctor
curl -k https://localhost:8443/healthz           # ok
printf 'a dev admin password\n' | COPPER_CLOUD_DATABASE_URL=postgres://localhost:5432/copper_cloud_dev \
  cargo run -- --config /tmp/cc.toml admin create-admin --email admin@example.com --password-stdin
open https://localhost:8443/                     # admin portal (needs portal/out, see below)
```

The admin portal is the Next.js app in `portal/` (`cd portal && bun install && bun run build`
→ `portal/out`); debug builds serve `portal/out` from disk, release builds embed it.

`init-config` defaults to `listen = 0.0.0.0:8443`, `public_url = localhost:8443`, a
self-signed certificate in `<config dir>/tls/` (created on first start) and Prometheus metrics
on `127.0.0.1:9464`. Every config key can be overridden with `COPPER_CLOUD_<KEY>`
(`COPPER_CLOUD_TLS__MODE=off` for plain HTTP).

## CLI

| Command | What it does |
|---|---|
| `copper-cloud [serve]` | run the server (applies migrations first; `--no-migrate` to skip) |
| `copper-cloud migrate` | apply database migrations |
| `copper-cloud link-code [--sso]` | print the link code (`--sso`: the single sign-on join link, no secret) |
| `copper-cloud doctor` | check config, database, migrations, TLS, listener, SSO, Canvas Cloud link |
| `copper-cloud healthcheck --wait 60` | wait for `/healthz` (pinned TLS) |
| `copper-cloud tls-init [--force] [--name N]` | create the self-signed certificate |
| `copper-cloud init-config --write PATH` | write a config with fresh keys |
| `copper-cloud admin users\|create-user\|reset-password\|delete-user\|disable-user\|enable-user\|disable-signup\|enable-signup` | user administration |
| `copper-cloud admin create-admin\|reset-admin-password\|list-admins\|delete-admin` | portal admin accounts |
| `copper-cloud admin access-mode\|set-access-mode open\|directory\|create-access-key` | who may pass the gate |
| `copper-cloud admin sso show\|set-google\|set-oidc\|disable\|enable\|delete\|password-signin on\|off` | company single sign-on (client secret from stdin only, never shown) |
| `copper-cloud admin integration create\|list\|rotate\|revoke` | agent-integration keys (`cai_…`, printed once) |
| `copper-cloud admin set-instance-name NAME` | the name on the `/start` onboarding page |
| `copper-cloud admin history delete --user EMAIL\|UUID [--since T] [--until T] [--device ID] [--dry-run]` | delete (or count) a user's synced history wholesale, by time window/device only (never by content) |
| `copper-cloud intelligence set\|show\|clear\|enable\|disable` | cloud-wide Jev + LLM router keys every signed-in Copper receives (keys from files/stdin, shown masked), plus the org-wide agent round budget (`--agent-max-turns`, `clear --agent`) |
| `copper-cloud addons list\|slots\|upload FILE --slot SLOT\|enable ID --everyone\|--people a@x,b@y\|disable ID\|settings ID --json '{…}'\|delete ID --yes` | addons: web bundles that replace a slot of the Copper window (e.g. the agent sidebar), handed to everyone or listed people ([docs/addons.md](docs/addons.md)); also `copper-cloud admin addons …` |
| `copper-cloud canvas link --url URL --key-file F [--fingerprint HEX] [--no-sync]` | link a Canvas Cloud (app key from a file/stdin, never argv) and provision every user |
| `copper-cloud canvas unlink\|status\|sync-users [--prune]` | remove the link / show it and test it / push every user now (`--prune`: also delete Canvas Cloud users that no longer exist here) |
| `copper-cloud canvas migrate [--dry-run] [--only ID]… [--replace] [--concurrency N] [--json]` | copy legacy canvases into the linked Canvas Cloud, verified by digest ([docs/canvas-migration.md](docs/canvas-migration.md)) |
| `copper-cloud version` | version |

All commands take `--config PATH` (default `/etc/copper-cloud/copper-cloud.toml`, env
`COPPER_CLOUD_CONFIG`).

## Documentation

- [Architecture](docs/architecture.md) — components, data model, request path
- [HTTP API](docs/api.md) — every endpoint with requests and responses
- [Admin API](docs/admin-api.md) — the admin portal's `/admin/api/*` contract
- [Addons](docs/addons.md) — slot addons: manifest, bundles, who gets what, API, CLI, security
- [Canvas link](docs/canvas-link.md) — how canvases are served by a linked Canvas Cloud:
  proxy, events, provisioning, linking, security
- [Canvas migration](docs/canvas-migration.md) — moving legacy canvases to Canvas Cloud
  (runbook, rollback)
- [Canvas](docs/canvas.md) · [Canvas protocol](docs/canvas-protocol.md) — pointers to the
  canvas API, now documented in Canvas Cloud
- [Security](docs/security.md) — threat model, access keys, pairing, admin sessions,
  encryption at rest, TLS pinning
- [Install](docs/install.md) — VM one-liner, manual install, configuration reference
- [Operations](docs/operations.md) — doctor, logs, metrics, backups, upgrades
- [Onboarding a team](docs/onboarding.md) — single sign-on first, link codes, people, keeping links valid
- [Single sign-on + agent integrations](docs/sso.md) — Google / OIDC setup, `/start`, agent pairing
- [v0.11.0 release notes](docs/v0.11.0.md) · [v0.10.0](docs/v0.10.0.md) · [v0.9.0](docs/v0.9.0.md) · [v0.8.1](docs/v0.8.1.md) · [v0.8.0](docs/v0.8.0.md) · [v0.7.0](docs/v0.7.0.md) · [v0.6.0](docs/v0.6.0.md) · [v0.5.0](docs/v0.5.0.md) · [v0.4.0](docs/v0.4.0.md) · [v0.3.0](docs/v0.3.0.md)

## Development

```sh
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
createdb copper_cloud_test_core   # once
createdb copper_cloud_test_migrate   # once, for crates/canvas-migrate
cargo test --workspace
```

Tests use a real native Postgres (`COPPER_CLOUD_TEST_DATABASE_URL`, default
`postgres://localhost:5432/copper_cloud_test_core`; the migration tests use
`COPPER_CLOUD_TEST_MIGRATE_DATABASE_URL`, default
`postgres://localhost:5432/copper_cloud_test_migrate`); never a
container. The canvas-link tests run against an in-process mock Canvas Cloud. Releases: push a
`vX.Y.Z` tag matching the crate version; CI builds `copper-cloud-X.Y.Z-linux-{x86_64,aarch64}.tar.gz`
(+ `.sha256`) and publishes the GitHub release.

## Roadmap

- **Google SSO (TODO):** sign in with Google (OIDC) as an alternative to email + password.
  Not implemented in v1; see [docs/security.md](docs/security.md#google-sso-todo).
- Master-key rotation (v1 documents a manual procedure only).

## License

[MIT](LICENSE)
