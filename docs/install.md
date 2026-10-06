# Install

## One command on a VM

Fresh Ubuntu 22.04/24.04, Debian 12, Amazon Linux 2023 or Fedora, x86_64 or aarch64, with
port 443 open:

```sh
curl -fsSL https://raw.githubusercontent.com/copper-browser/copper-cloud/main/install.sh | sudo sh
```

From a release tarball (files at the archive root):

```sh
tar xzf copper-cloud-0.6.0-linux-aarch64.tar.gz
sudo ./install.sh
```

What it does (every step is idempotent):

1. Detects the distro (apt or dnf) and CPU; installs `curl`/`tar`/CA certificates if missing.
2. Installs the binary to `/usr/local/bin/copper-cloud` — from `COPPER_CLOUD_BINARY` (binary
   or `.tar.gz`), else a `copper-cloud` next to `install.sh`, else `COPPER_CLOUD_BINARY_URL`,
   else the GitHub release `COPPER_CLOUD_VERSION` (default latest; sha256-verified). Replaced
   atomically.
3. Creates the `copper-cloud` system user, `/etc/copper-cloud` and `/var/lib/copper-cloud`.
4. If no config exists yet:
   - without `DATABASE_URL`: installs PostgreSQL, creates role `copper_cloud` (random password,
     SCRAM) and database `copper_cloud`, and allows that role only on `127.0.0.1`/`::1`;
   - detects the public host (`COPPER_CLOUD_PUBLIC_HOST`, else the EC2 public IP, else an
     external lookup, else the first local address);
   - writes `/etc/copper-cloud/copper-cloud.toml` (0600) with a fresh instance key and master
     key (or `COPPER_CLOUD_INSTANCE_KEY` / `COPPER_CLOUD_MASTER_KEY`).
   An existing config is never modified.
5. `copper-cloud tls-init` — self-signed certificate with the public host as SAN (skipped for
   ACME; an existing certificate is kept).
6. `copper-cloud migrate`, then the portal admin: if no admin with
   `COPPER_CLOUD_ADMIN_EMAIL` exists (and, without that variable, no admin at all),
   `copper-cloud admin create-admin` with `COPPER_CLOUD_ADMIN_PASSWORD` or a generated
   24-character password, saved to `/etc/copper-cloud/admin-credentials` (0600). On a
   **fresh** instance (no admin and no user yet) it also sets the access mode
   (`COPPER_CLOUD_ACCESS_MODE`, default `directory`); existing instances keep theirs.
7. Installs `/etc/systemd/system/copper-cloud.service`, opens the port in ufw/firewalld when
   they are active, enables and (re)starts the service.
8. Waits for `/healthz` (pinned TLS probe, 60 s; 180 s for ACME), writes the link code to
   `/etc/copper-cloud/link-code` (0600) and prints it with the portal URL (and the admin
   password when it generated one). In `directory` mode the link code is a one-account access
   key ("Installer link code", kept across re-runs); in `open` mode it is the instance link code.

Re-running upgrades the binary, applies migrations and restarts; keys, certificate, config and
data stay.

### Installer inputs

| Variable | Default | Meaning |
|---|---|---|
| `DATABASE_URL` | local Postgres | external Postgres, e.g. `postgres://user:pw@db:5432/copper_cloud?sslmode=require` |
| `COPPER_CLOUD_PUBLIC_HOST` | detected | host/IP in the link code and certificate |
| `COPPER_CLOUD_PORT` | `443` | listen + public port |
| `COPPER_CLOUD_DOMAIN` | — | enables ACME (Let's Encrypt) for this domain (needs :443 reachable, DNS pointing here) |
| `COPPER_CLOUD_ACME_EMAIL` | — | ACME account contact |
| `COPPER_CLOUD_ALLOW_SIGNUP` | `true` | `false`: only the first user may sign up with the instance key (open mode) |
| `COPPER_CLOUD_ADMIN_EMAIL` | `admin@<public host>` | portal admin account (`admin@copper-cloud.local` when the host is an IPv6 literal or has no dot) |
| `COPPER_CLOUD_ADMIN_PASSWORD` | generated (24 chars, printed once) | portal admin password, ≥ 10 characters; never changes an existing admin |
| `COPPER_CLOUD_ACCESS_MODE` | `directory` | fresh instances only: `directory` (personal access keys) or `open` (shared instance key) |
| `COPPER_CLOUD_INSTANCE_KEY` | generated | ≥ 32 chars of `A-Za-z0-9-_.~` |
| `COPPER_CLOUD_MASTER_KEY` | generated | base64url of exactly 32 bytes |
| `COPPER_CLOUD_BINARY` | — | local binary or release tarball |
| `COPPER_CLOUD_BINARY_URL` | — | URL of a binary or release tarball |
| `COPPER_CLOUD_VERSION` | `latest` | GitHub release to fetch (e.g. `0.6.0`) |
| `GITHUB_TOKEN` | — | for a private GitHub repository |

`sudo -E` (or `sudo VAR=… sh install.sh`) is needed to pass variables through sudo.

### Uninstall

```sh
sudo ./install.sh --uninstall   # stop + remove service and binary; keep config, keys, data
sudo ./install.sh --purge       # also delete /etc/copper-cloud, /var/lib/copper-cloud and the local copper_cloud database
```

## Manual install

```sh
# 1. binary
sudo install -m 0755 copper-cloud /usr/local/bin/copper-cloud
sudo useradd --system --user-group --home-dir /var/lib/copper-cloud --shell /usr/sbin/nologin copper-cloud
sudo install -d -m 0750 -o root -g copper-cloud /etc/copper-cloud

# 2. database (any Postgres ≥ 13)
sudo -u postgres psql -c "CREATE ROLE copper_cloud LOGIN PASSWORD '…'" \
                      -c "CREATE DATABASE copper_cloud OWNER copper_cloud"

# 3. config (keys generated; database URL via env keeps it out of `ps`)
sudo COPPER_CLOUD_DATABASE_URL='postgres://copper_cloud:…@127.0.0.1:5432/copper_cloud' \
  copper-cloud init-config --write /etc/copper-cloud/copper-cloud.toml \
  --public-url 203.0.113.10 --listen 0.0.0.0:443 --tls-dir /etc/copper-cloud/tls
sudo chown copper-cloud:copper-cloud /etc/copper-cloud/copper-cloud.toml

# 4. TLS + schema
sudo copper-cloud tls-init
sudo chown -R root:copper-cloud /etc/copper-cloud/tls && sudo chmod 0640 /etc/copper-cloud/tls/key.pem
sudo copper-cloud migrate

# 5. service
sudo install -m 0644 packaging/copper-cloud.service /etc/systemd/system/
sudo systemctl daemon-reload && sudo systemctl enable --now copper-cloud
sudo copper-cloud healthcheck --wait 30 && sudo copper-cloud link-code
```

## Configuration reference

`/etc/copper-cloud/copper-cloud.toml` (override path with `--config` or
`COPPER_CLOUD_CONFIG`). Every key can be set from the environment as
`COPPER_CLOUD_<KEY>`; nested keys use `__` (`COPPER_CLOUD_TLS__MODE`,
`COPPER_CLOUD_LIMITS__MAX_BLOB_BYTES`). Environment wins over the file. If the default path
does not exist the server runs from environment variables alone.

| Key | Default | |
|---|---|---|
| `listen` | `0.0.0.0:443` | socket address |
| `public_url` | — (required; `tls.domain` if set) | `host[:port]` clients use; scheme/path are stripped |
| `database_url` | — (required) | `postgres://…`; add `?sslmode=require` for remote DBs |
| `db_max_connections` | `20` | pool size |
| `instance_key` | — (required) | shared secret for `X-Copper-Instance` |
| `master_key` | — (required) | base64url 32 bytes; wraps all data keys |
| `allow_signup` | `true` | runtime override: `admin enable-signup` / `disable-signup` |
| `trust_proxy` | `false` | use `X-Forwarded-For` for client IPs (only behind a proxy) |
| `metrics_bind` | `127.0.0.1:9464` | Prometheus listener; `"off"` disables |
| `log_format` | `json` | `json` or `pretty` |
| `log_level` | `info` | tracing filter (`RUST_LOG` overrides) |
| `tls.mode` | `self-signed` | `self-signed`, `acme`, `off` |
| `tls.cert_path` / `tls.key_path` | `/etc/copper-cloud/tls/{cert,key}.pem` | PEM |
| `tls.domain` | — | required for `acme` |
| `tls.acme_email` | — | ACME contact |
| `tls.acme_cache_dir` | `/var/lib/copper-cloud/acme` | account + certificate cache |
| `tls.acme_staging` | `false` | Let's Encrypt staging directory |
| `limits.max_blob_bytes` | `8000000` | max decoded sync doc |
| `limits.max_history_batch` | `2000` | entries per push / max page size |
| `limits.max_history_entry_bytes` | `16384` | one history entry |
| `limits.auth_per_minute` | `10` | auth requests per IP per minute |

Unknown keys in the file are rejected (typo protection); unknown `COPPER_CLOUD_*` environment
variables are ignored (the installer's inputs share the prefix).

### Behind a reverse proxy

Set `tls.mode = "off"`, `trust_proxy = true`, `listen = "127.0.0.1:8080"`, and terminate TLS
in the proxy. Link codes then carry an `fp` only if `tls.cert_path` points at the proxy's
certificate; with a publicly trusted proxy certificate clients use normal validation. The proxy
must pass `X-Copper-Instance`, allow long-lived responses (SSE: disable buffering) and
WebSocket upgrades on `/v1/canvases/*/ws`.
