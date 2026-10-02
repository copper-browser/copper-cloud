# Copper Cloud

Self-hosted sync + collaborative canvas server for the [Copper](https://github.com/copper-browser/Copper)
browser. One Rust daemon (`copper-cloud`), one Postgres. Install it on any Linux VM with one
command, or deploy to AWS with Terraform (`deploy/aws`).

What it syncs between your Coppers: spaces + pinned tabs, settings (safe subset), bookmarks,
history (append-only), each device's open tabs, and shared canvases (live Yjs rooms).

## Install on a VM (Ubuntu 22.04/24.04, Debian 12, Amazon Linux 2023, Fedora)

```sh
curl -fsSL https://raw.githubusercontent.com/copper-browser/copper-cloud/main/install.sh | sudo sh
```

or from a release tarball:

```sh
tar xzf copper-cloud-0.3.0-linux-x86_64.tar.gz && sudo ./install.sh
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
| `copper-cloud link-code` | print the link code |
| `copper-cloud doctor` | check config, database, migrations, TLS, listener |
| `copper-cloud healthcheck --wait 60` | wait for `/healthz` (pinned TLS) |
| `copper-cloud tls-init [--force] [--name N]` | create the self-signed certificate |
| `copper-cloud init-config --write PATH` | write a config with fresh keys |
| `copper-cloud admin users\|create-user\|reset-password\|delete-user\|disable-user\|enable-user\|disable-signup\|enable-signup` | user administration |
| `copper-cloud admin create-admin\|reset-admin-password\|list-admins\|delete-admin` | portal admin accounts |
| `copper-cloud admin access-mode\|set-access-mode open\|directory\|create-access-key` | who may pass the gate |
| `copper-cloud version` | version |

All commands take `--config PATH` (default `/etc/copper-cloud/copper-cloud.toml`, env
`COPPER_CLOUD_CONFIG`).

## Documentation

- [Architecture](docs/architecture.md) — components, data model, request path
- [HTTP API](docs/api.md) — every endpoint with requests and responses
- [Admin API](docs/admin-api.md) — the admin portal's `/admin/api/*` contract
- [Canvas](docs/canvas.md) — canvases REST + WebSocket rooms
- [Security](docs/security.md) — threat model, access keys, pairing, admin sessions,
  encryption at rest, TLS pinning
- [Install](docs/install.md) — VM one-liner, manual install, configuration reference
- [Operations](docs/operations.md) — doctor, logs, metrics, backups, upgrades
- [v0.3.0 release notes](docs/v0.3.0.md)

## Development

```sh
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
createdb copper_cloud_test_core && createdb copper_cloud_test_canvas   # once
cargo test --workspace
```

Tests use a real native Postgres (`COPPER_CLOUD_TEST_DATABASE_URL`, default
`postgres://localhost:5432/copper_cloud_test_core`); never a container. Releases: push a
`vX.Y.Z` tag matching the crate version; CI builds `copper-cloud-X.Y.Z-linux-{x86_64,aarch64}.tar.gz`
(+ `.sha256`) and publishes the GitHub release.

## Roadmap

- **Google SSO (TODO):** sign in with Google (OIDC) as an alternative to email + password.
  Not implemented in v1; see [docs/security.md](docs/security.md#google-sso-todo).
- Master-key rotation (v1 documents a manual procedure only).

## License

MIT
