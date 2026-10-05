# Security

## Threat model (v1)

copper-cloud is a small self-hosted service on the public internet holding users' browsing
data. Goals:

1. Strangers on the internet cannot use the API at all (gate credential: instance key or a
   personal access key), nor read traffic (TLS). In `directory` mode the admin decides, per
   person, who may connect.
2. Users of the same instance cannot see each other's data (every query is user-scoped).
3. A stolen database dump / disk snapshot without the master key reveals no synced content.
4. No secrets in logs.

Out of scope for v1: end-to-end encryption (the server can decrypt to serve clients), a
compromised server process or host, key rotation, multi-instance HA.

## Layers

### Gate: instance key or access key

Every `/v1` request except `POST /v1/auth/pair` must carry `X-Copper-Instance`. Without an
accepted credential the server answers `401 {"error":"instance_key"}` for every `/v1` path —
unknown routes included — and never reads request bodies. Two kinds of credential:

- **Instance key** — one shared secret per instance, ≥ 32 characters of `[A-Za-z0-9-_.~]`
  (generated: base64url of 32 random bytes), travelling inside the link code's URL fragment.
  The presented and configured keys are SHA-256'd and compared in constant time, so neither
  content nor length leaks. **Accepted only while `access_mode = open`.**
- **Access key** — per person, minted by an admin in the portal (or by pairing, below):
  `ck_` + 32 random bytes base64url. Only SHA-256(key) is stored (`access_keys.key_sha256`,
  unique index); the plaintext is shown once. Accepted in both modes while not revoked and not
  expired; revocation is immediate (keys are looked up per request, not cached). Optional
  `email` binding (signup must use that email) and `max_uses` (accounts that may be created
  with it). `last_used_at` is written at most once a minute.

`access_mode` (`server_settings`) is `open` or `directory`. Fresh installs (install.sh /
Terraform) start in `directory`, so a leaked link code of one person can be revoked without
re-keying everyone; instances that predate access keys stay `open`. The mode is cached in
memory for 5 seconds (one query per 5 s) — a mode switch takes effect within that window
(immediately on the server that handled the admin's change). If the database cannot be read
and nothing is cached the gate fails closed (500).

Which credential a request used is attached as a `GateIdentity` request extension; signup uses
it (access key → permitted, email binding enforced, one use consumed atomically in the signup
transaction; instance key → `allow_signup`; directory mode without a key → `403`).

### Pairing codes

A signed-in Copper can mint a single-use code (`cp_` + 24 random bytes base64url, SHA-256
stored, 10-minute TTL, ≤ 20 active per user) that signs another Copper into the same account.
`POST /v1/auth/pair` is the only gate-free `/v1` route: the code is the credential. It is rate
limited with `/v1/auth/*`, looked up by hash (an indexed equality on a 256-bit digest — no
timing oracle on the code), and consumed with `UPDATE … WHERE used_at IS NULL AND expires_at >
now() RETURNING` inside the transaction that creates the session, so two concurrent redemptions
cannot both succeed. The response carries the gate credential the new device must use: the
instance key in `open` mode, or — in `directory` mode — a fresh access key bound to the user's
email (label `"<device> via pairing"`), so every paired device is individually revocable.
Codes are revocable by their owner and by admins; used/expired rows are purged after a day.

### Admin accounts and the admin API

Admins (`admins`) are separate from users: they manage the instance from the web portal and
`/admin/api/*`; they have no synced data. The admin API is not behind the instance gate.

- Passwords: Argon2id like user passwords; login failures are constant-work and identical for
  unknown email vs wrong password. Login is rate limited per IP (own bucket,
  `limits.auth_per_minute`).
- Sessions: cookie `cc_admin` = 32 random bytes base64url, SHA-256 stored, fixed 7-day
  lifetime, `HttpOnly; SameSite=Strict; Path=/admin` and `Secure` (omitted only with
  `tls.mode = "off"`). Changing the password revokes the admin's other sessions;
  `admin reset-admin-password` revokes all.
- CSRF: `SameSite=Strict` plus a required `X-Requested-With: copper-cloud-portal` header on
  every non-GET request (a cross-site form or `fetch` cannot add it without a CORS preflight,
  which the server never grants). Missing → `403 {"error":"csrf"}`.
- Every admin mutation (including login/logout) is written to `admin_audit` (admin, action,
  target id, non-secret detail, time).
- Admin responses are `Cache-Control: no-store`. Access-key plaintext appears only in the
  `POST access-keys` response; lists never contain secrets. Canvas content is never exposed.
- The portal's static files are served with a strict CSP (`default-src 'self'`,
  `script-src 'self'` + SHA-256 hashes of the export's own inline bootstrap scripts, computed
  per file at serve time; `connect-src 'self'`; `frame-ancestors 'none'`), `nosniff`,
  `Referrer-Policy: same-origin` and `X-Frame-Options: DENY`.
- Initial credentials: install.sh generates a 24-character password (printed once, saved to
  `/etc/copper-cloud/admin-credentials`, `0600 root`); Terraform generates it into SSM
  SecureString `/copper-cloud/<name>/admin-password`. Change it after first login.

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
  rows are purged hourly. Password change revokes all other sessions; `admin reset-password`,
  `admin disable-user` and the portal's disable / reset-password revoke all (disable also
  disconnects the user's live canvas peers).
- Rate limiting: `limits.auth_per_minute` (10) per client IP on `/v1/auth/*` (signup, login,
  logout, password, pair; `GET /v1/auth/me` and pairing-code management are exempt because a
  256-bit bearer token cannot be brute-forced and clients use them as status probes). IPv6
  clients are keyed by /64. Admin login has a separate bucket of the same size.
  `X-Forwarded-For` is honored only with `trust_proxy = true`.
- Signup: with the instance key (open mode) it follows `allow_signup` (`admin
  disable-signup` / the portal restricts it to the very first user, after which the admin
  creates accounts); with an access key it is permitted (subject to the key's email binding
  and `max_uses`).

### Authorization

Every query that touches user data is filtered by the authenticated `user_id` (docs, history,
devices, sessions). `tabs:<device_id>` docs are writable only by the owning device. Canvas
access is checked per membership by the canvas crate. Share-link preview and join routes require
an active session; disabled accounts have no valid sessions and cannot use links. Link joins are
serialized on the link row, use the membership primary key for idempotency, and never replace an
existing owner role.

`GET /v1/people` is intentionally a team-server directory: any signed-in user of an instance
can list its active users (excluding themselves), optionally filtering by a case-insensitive
substring of display name or email. Deleted accounts are absent and disabled accounts are
excluded.

### Share-link tokens

A random 32-byte secret is generated for every canvas share link, but only its SHA-256 digest is
stored. The plaintext is returned only from link creation, never from list/preview responses and
never in logs. Owners can revoke one or all links; deleting a canvas cascades its links. Email
invite tokens follow the same digest-only storage rule.

### Cloud-wide intelligence keys

An admin can store one Jev (TypeSafe) key and one LLM router (LiteLLM) key per instance; every
signed-in user receives them in clear from `GET /v1/intelligence`. This is deliberate: the keys
are shared credentials of the instance, so **anyone who can sign in can use (and copy) them**.
Scope them accordingly — prefer a dedicated LiteLLM virtual key with its own budget per
instance over a personal key, and rotate by setting a new key (Coppers pick it up on their
next fetch; `updated_at` changes).

- Storage: `intelligence_settings` (singleton row). A random 32-byte data key is wrapped by the
  KEK (`data_key_wrapped`); each API key is AES-256-GCM sealed under it with AAD
  `intelligence:jev` / `intelligence:router`. Endpoint, model and URL are plaintext.
- Reads require the gate credential **and** a valid user session (disabled users have none).
  The response is `Cache-Control: no-store`. The admin toggle `enabled = false` withholds the
  keys without deleting them.
- Admin surfaces are write-only: the admin API, portal and `copper-cloud intelligence show`
  only ever show the last four characters (none for keys under 12 characters).
- The CLI reads keys from files or stdin (`--jev-key-file F`, `--router-key-file -`), never
  from argv, so they stay out of shell history and `ps`.
- Every change (`intelligence.update` / `intelligence.clear`, admin or CLI) and user reads
  (`intelligence.read`, at most once per user per hour) are written to `admin_audit`; audit
  details and logs never contain key material.

### Encryption at rest

```
master_key (config, 32 B) ──HKDF-SHA256(salt "copper-cloud", info "copper-cloud/v1/key-encryption-key")──▶ KEK
KEK ──AES-256-GCM(aad "copper-cloud/v1/wrapped-key")──▶ users.data_key_wrapped, canvases.doc_key_wrapped,
                                                        intelligence_settings.data_key_wrapped
data_key ──AES-256-GCM(nonce 12 B random, aad "<user_id>:<domain>")──▶ sync_docs.payload, history.payload
intelligence data_key ──AES-256-GCM(aad "intelligence:jev" | "intelligence:router")──▶ jev_key_sealed, router_key_sealed
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
rotation is just a config change + new link codes for every Copper still on the instance key
(Coppers on access keys are unaffected); a single person's access key is rotated by revoking
it and minting a new one in the portal.

### Logging

Request spans record method, path (never the query string), matched route, client IP,
`user_id`, status and latency. Never logged: tokens, passwords, instance/master keys, access
keys, intelligence (Jev / router) keys, pairing codes, admin cookies, request or response bodies, payloads (access keys and
pairing codes are logged by id only). `Config`'s `Debug` redacts keys and the database password.
500s log the internal error chain server-side only; clients get `{"error":"internal"}`.

### Process hardening (systemd unit)

Dedicated `copper-cloud` user; `CAP_NET_BIND_SERVICE` only; `NoNewPrivileges`,
`ProtectSystem=strict` (config and certificate read-only, state in `/var/lib/copper-cloud`),
`ProtectHome`, `PrivateTmp`, `PrivateDevices`, kernel/cgroup/clock protection,
`RestrictAddressFamilies=AF_INET AF_INET6 AF_UNIX`, `MemoryDenyWriteExecute`,
`SystemCallFilter=@system-service ~@privileged`, `UMask=0077`. Config file `0600`
(`copper-cloud`), TLS key `0640 root:copper-cloud`, link-code and admin-credentials files
`0600 root`.

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
