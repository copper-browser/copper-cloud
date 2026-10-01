# Security

## Threat model (v1)

copper-cloud is a small self-hosted service on the public internet holding users' browsing
data. Goals:

1. Strangers on the internet cannot use the API at all (instance key), nor read traffic (TLS).
2. Users of the same instance cannot see each other's data (every query is user-scoped).
3. A stolen database dump / disk snapshot without the master key reveals no synced content.
4. No secrets in logs.

Out of scope for v1: end-to-end encryption (the server can decrypt to serve clients), a
compromised server process or host, key rotation, multi-instance HA.

## Layers

### Instance key

Every request except `GET /healthz` must carry `X-Copper-Instance: <instance_key>`. Both the
presented and configured keys are SHA-256'd and compared in constant time, so neither content
nor length leaks. Without it the server answers `401 {"error":"instance_key"}` for every path —
unknown routes included — and never reads request bodies.

The key is ≥ 32 characters of `[A-Za-z0-9-_.~]` (generated: base64url of 32 random bytes). It
travels inside the link code's URL fragment.

### TLS

Always on, except `tls.mode = "off"`, which is only for deployments behind a TLS-terminating
proxy (and should be paired with `trust_proxy = true`).

- **self-signed**: the link code carries `fp=<sha256 hex of the leaf certificate DER>`.
  Clients must pin exactly that certificate (compare the presented leaf's DER hash; do not
  fall back to system trust). The certificate is valid 10 years so pins do not silently break;
  `tls-init --force` rotates it and every Copper must then re-link with the new code.
- **acme**: a publicly trusted Let's Encrypt certificate; the link code has no `fp` and
  clients use normal WebPKI validation.

rustls only (ring provider), TLS 1.2+ with rustls' safe defaults; HTTP/2 or HTTP/1.1 via ALPN.

### Accounts and sessions

- Passwords: Argon2id, m = 64 MiB, t = 3, p = 1, random salt, PHC string. ≥ 10 characters.
  Hashing runs on the blocking pool with bounded concurrency (memory stays bounded under a
  login flood).
- Login failures are indistinguishable for unknown email vs wrong password, and do the same
  Argon2 work (a dummy hash is verified when the user does not exist).
- Session tokens: 32 bytes from the OS CSPRNG, base64url. The database stores only
  SHA-256(token), so a DB leak does not yield usable sessions. Sliding 90-day expiry; expired
  rows are purged hourly. Password change revokes all other sessions; `admin reset-password`
  and `admin disable-user` revoke all.
- Rate limiting: `limits.auth_per_minute` (10) per client IP on `/v1/auth/*` (signup, login,
  logout, password; `GET /v1/auth/me` is exempt because a 256-bit bearer token cannot be
  brute-forced and clients use it as a status probe). IPv6 clients are keyed by /64.
  `X-Forwarded-For` is honored only with `trust_proxy = true`.
- Signup: open by default; `allow_signup = false` (or `admin disable-signup`) restricts it to
  the very first user, after which the admin CLI creates accounts.

### Authorization

Every query that touches user data is filtered by the authenticated `user_id` (docs, history,
devices, sessions). `tabs:<device_id>` docs are writable only by the owning device. Canvas
access is checked per membership by the canvas crate.

### Encryption at rest

```
master_key (config, 32 B) ──HKDF-SHA256(salt "copper-cloud", info "copper-cloud/v1/key-encryption-key")──▶ KEK
KEK ──AES-256-GCM(aad "copper-cloud/v1/wrapped-key")──▶ users.data_key_wrapped, canvases.doc_key_wrapped
data_key ──AES-256-GCM(nonce 12 B random, aad "<user_id>:<domain>")──▶ sync_docs.payload, history.payload
```

- Sealed blob layout: `nonce(12) ‖ ciphertext ‖ tag(16)`.
- AAD binds each blob to its user and domain: copying a row to another user or domain makes
  it fail authentication.
- The master key never leaves the config file / process memory; the derived KEK cipher is held
  in memory, raw key bytes are zeroized after derivation.
- What is *not* encrypted: emails, display names, device names, timestamps, sizes, versions,
  domain names, history `visited_at`. These are needed for indexing/ordering.

**Back up the master key separately from database backups.** Losing it makes all synced data
unrecoverable; leaking it together with a DB dump exposes everything.

#### Key rotation (out of scope for v1)

There is no online rotation. Manual procedure if the master key must change: stop the
service, run a one-off job that unwraps every `data_key_wrapped`/`doc_key_wrapped` with the old
KEK and re-wraps with the new (data blobs themselves need no re-encryption), swap the key in the
config, start. Rotating a user's data key would require re-encrypting their rows. Instance-key
rotation is just a config change + new link codes for every Copper.

### Logging

Request spans record method, path (never the query string), matched route, client IP,
`user_id`, status and latency. Never logged: tokens, passwords, instance/master keys, request
or response bodies, payloads. `Config`'s `Debug` redacts keys and the database password.
500s log the internal error chain server-side only; clients get `{"error":"internal"}`.

### Process hardening (systemd unit)

Dedicated `copper-cloud` user; `CAP_NET_BIND_SERVICE` only; `NoNewPrivileges`,
`ProtectSystem=strict` (config and certificate read-only, state in `/var/lib/copper-cloud`),
`ProtectHome`, `PrivateTmp`, `PrivateDevices`, kernel/cgroup/clock protection,
`RestrictAddressFamilies=AF_INET AF_INET6 AF_UNIX`, `MemoryDenyWriteExecute`,
`SystemCallFilter=@system-service ~@privileged`, `UMask=0077`. Config file `0600`
(`copper-cloud`), TLS key `0640 root:copper-cloud`, link-code file `0600 root`.

### Database

The installer's local Postgres listens on localhost only; the `copper_cloud` role gets a random
40-character password and `scram-sha-256` auth on `127.0.0.1`/`::1` only. For RDS use
`?sslmode=require` (or stricter) in `DATABASE_URL`.

## Google SSO (TODO)

Not implemented in v1; email + password only. Plan: optional OIDC "Sign in with Google"
(`[auth.google] client_id`, `client_secret`, allowed hosted domain) using the authorization
code + PKCE flow from Copper, the server verifying the ID token (issuer, audience, `hd`,
`email_verified`), linking by verified email and issuing the same opaque session tokens.
Password login stays available for self-hosters without a Google project.

## Reporting

Security issues: contact the maintainers privately (do not open a public issue).
