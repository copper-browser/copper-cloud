# Migrating canvases to Canvas Cloud

copper-cloud ≤ 0.10.x stored canvases in its own database (the `canvases`, `canvas_members`,
`canvas_invites`, `canvas_updates`, `canvas_snapshots`, `canvas_share_links` and
`canvas_mentions` tables, migrations `0100`–`0103`). From 0.11.0 canvases are served by a linked
[Canvas Cloud](https://github.com/copper-browser/canvas-cloud) ([canvas-link.md](canvas-link.md)).
This page is the one-time move of an existing deployment's canvases, and the runbook for it.

A fresh 0.11.0 install has empty legacy tables: link a Canvas Cloud and skip the migration.

## In short

`copper-cloud canvas migrate` (crate `copper-cloud-canvas-migrate`, `crates/canvas-migrate`):

1. checks the link (`GET /v1/apps/self/stats`, 30 s) and that the app may import;
2. provisions every Copper user on Canvas Cloud with `users:batch` (subject = id = Copper user
   id); if any user does not come back with **its own id**, it stops and imports nothing;
3. for each legacy canvas — two at a time by default, each read in its own `REPEATABLE READ,
   READ ONLY` transaction — decrypts it with copper-cloud's `master_key` and streams it as one
   import-v1 NDJSON body to `POST /v1/apps/self/import`;
4. verifies each import: the counts Canvas Cloud applied must equal the counts sent, and the
   digest of the document Canvas Cloud now stores (`GET /v1/apps/self/canvases/{id}/digest`)
   must equal the digest the import returned;
5. records each result in the `canvas_migrations` ledger and prints a report.

It **never modifies or deletes the legacy tables**; its only database writes are ledger rows.
Rollback is reinstalling the previous release ([Rollback](#rollback)).

## What is preserved

| Data | After the migration |
|---|---|
| canvas id, name, kind (Personal / shared), owner, `created_at`, `updated_at` | identical — Copper keeps its tabs and links (`copper://canvas/<id>`) |
| the document (shapes, order, meta, agents, chat, …) | identical content: the digest of the stored document equals the digest of the legacy updates applied in order |
| members and roles (`owner` / `editor`), `added_at` | identical |
| user ids | identical: Canvas Cloud app users are created with the Copper user id, so member lists, chat `authorId`s inside documents and mention senders/recipients still match |
| invites — `pending`, `accepted`, `declined`, `revoked` — with inviter, `created_at`, `accepted_at`, `nudged_at` (reminder throttle) | identical; pending invites can still be accepted or declined, and invite ids are unchanged |
| share links: id, role, creator, `created_at`, `uses` | identical, and the stored token digests are copied, so **every existing share-link URL keeps working** |
| mentions: id, chat message id, from, to, excerpt, `created_at`, `read_at` | identical (excerpts are decrypted and re-sealed under Canvas Cloud's key); unread mentions stay unread |

What is **not** preserved:

- **Update history.** The legacy snapshot (if any) and every update after it are replayed into
  one document, and Canvas Cloud stores it as **one compacted snapshot** with no update rows.
  The final state is the same; the individual updates, their order and their per-update
  timestamps (`canvas_updates.created_at`) are gone. Copper does not use them.
- Live room state (who is connected, awareness/cursors): rooms reopen on Canvas Cloud.
- Encryption keys: each canvas gets a new random document key under Canvas Cloud's
  `master_key`.

## Prerequisites

1. **Canvas Cloud installed and running**, with `/v1/apps/self/import` and
   `/v1/apps/self/canvases/{id}/digest` (the release that ships the linked-apps API). The
   recommended small setup is **the same host, on loopback, without TLS**; any reachable host
   works ([TLS to Canvas Cloud](canvas-link.md#tls-to-canvas-cloud)).
   On AWS (`deploy/aws`) the copper-cloud VM is replaced on every release, so run Canvas Cloud
   somewhere durable instead ([deploy/aws/README.md](../deploy/aws/README.md#canvas-cloud)).
2. On that Canvas Cloud: `[ops] extra_url_schemes = ["copper"]` (Copper canvases contain
   `copper://` link shapes), and an **app key created with `--allow-import`**
   ([Linking › On the Canvas Cloud](canvas-link.md#on-the-canvas-cloud)).
   If that Canvas Cloud already has its own accounts with the same emails as Copper users,
   create the app with `--no-trust-email` — otherwise those users map to existing ids and the
   migration refuses to run.
3. **A backup** of the copper-cloud database (`pg_dump`, or an RDS snapshot) and of
   `/etc/copper-cloud/copper-cloud.toml` ([operations.md](operations.md#backups)). The
   migration does not change legacy rows, but the upgrade applies migration `0030`.
4. A maintenance window: copper-cloud is **stopped** from just before the upgrade until the
   migration finished. Sync, history and sign-in are unavailable for that time; Coppers keep
   working offline and catch up. For scale: the Exowatt dev1 instance had 36 canvases,
   64 memberships, 889 updates (≈ 88 MB stored), 22 invites, 5 share links and 5 mentions.
5. The release tarball or installer for copper-cloud 0.11.0.

## Runbook

All commands as root on the copper-cloud host unless noted. Every step can be re-run.

### 0. Same-host Canvas Cloud (if not installed yet)

The Canvas Cloud installer listens on `0.0.0.0:443` by default, which copper-cloud already
uses. Install on another port, then move it to loopback with TLS off:

```sh
curl -fsSL https://raw.githubusercontent.com/copper-browser/canvas-cloud/main/install.sh \
  | sudo CANVAS_CLOUD_PORT=8480 sh
```

Edit `/etc/canvas-cloud/canvas-cloud.toml` — set `listen` and add or change the two tables:

```toml
listen = "127.0.0.1:8480"

[tls]
mode = "off"

[ops]
extra_url_schemes = ["copper"]
```

```sh
sudo ufw delete allow 8480/tcp 2>/dev/null || true     # the installer opened it (ufw/firewalld)
sudo systemctl restart canvas-cloud
timeout 10 curl -fsS http://127.0.0.1:8480/v1/info
umask 077
sudo canvas-cloud apps create --name "Copper Cloud" --allow-import > /root/canvas-app.key
```

`apps create` prints only the key on stdout (the "created app <id>" line goes to stderr; note
the id for `apps rotate` / `apps revoke`).

The installer creates a separate `canvas_cloud` database on the same native PostgreSQL.

### 1. Stop copper-cloud and back up

```sh
sudo systemctl stop copper-cloud
sudo -u postgres pg_dump -Fc copper_cloud > /root/copper_cloud-pre-0.11-$(date +%F).dump
# RDS: aws rds create-db-snapshot --db-instance-identifier copper-cloud-<name> \
#        --db-snapshot-identifier copper-cloud-<name>-pre-0-11
sudo -u postgres psql -w -d copper_cloud -Atc 'SELECT count(*) FROM canvases'   # legacy canvases
```

### 2. Install the new release

```sh
tar xzf copper-cloud-0.11.0-linux-$(uname -m).tar.gz && sudo ./install.sh
# or: curl -fsSL https://raw.githubusercontent.com/copper-browser/copper-cloud/main/install.sh \
#       | sudo COPPER_CLOUD_VERSION=0.11.0 sh
```

The installer applies the database migrations (adds `canvas_link`, `canvas_link_outbox`,
`canvas_migrations`) and **restarts the service and waits for it to be healthy**. Nothing is
linked yet, so it serves sync normally and answers every canvas route with
`503 canvas_unavailable` — no canvas can change anywhere. Stop it again before linking:

```sh
sudo systemctl stop copper-cloud
```

Why: a running copper-cloud picks up a new link within 5 seconds and starts proxying. Linked
before the canvases are migrated, Coppers would get successful canvas lists **without** their
canvases (Copper then drops them and closes their tabs), and people could create new canvases
that the migration does not know about.

The `canvas` commands do not apply database migrations themselves; if you upgraded without
`install.sh`, run `sudo copper-cloud migrate` first.

### 3. Link

```sh
sudo copper-cloud canvas link --url http://127.0.0.1:8480 --key-file /root/canvas-app.key
```

```
linked http://127.0.0.1:8480 (Canvas Cloud 0.1.0, key …x7Qa)
provisioned 12/12 users on Canvas Cloud
```

A non-zero exit after `linked …` means some users failed or were mapped to a different id
(listed); the link is saved. Fix the cause and run `sudo copper-cloud canvas sync-users` until
it exits 0. Keep `/root/canvas-app.key` until step 7.

### 4. Dry run

```sh
sudo copper-cloud canvas migrate --dry-run
```

Reads, decrypts and encodes every legacy canvas without sending anything (only the preflight
request reaches Canvas Cloud, and no users are provisioned). It catches a wrong `master_key`,
undecryptable rows and oversized lines before anything is written. Expect every canvas
`dry-run` and the last line `ok`.

### 5. Migrate

```sh
sudo copper-cloud canvas migrate
# or with the JSON report kept for the record:
sudo copper-cloud canvas migrate --json > /root/canvas-migrate-$(date +%F).json
```

Exit `0` = every user provisioned and no canvas failed. Otherwise read the report
([Reading the report](#reading-the-report), [Troubleshooting](#troubleshooting)), fix, and run
it again: migrated canvases are skipped, failed ones are retried.

### 6. Start and check

```sh
sudo systemctl start copper-cloud
sudo copper-cloud healthcheck --wait 60
sudo copper-cloud canvas status          # reachable: yes; N migrated, 0 failed, N legacy canvases
sudo copper-cloud doctor                 # canvas-link: ok
```

In the admin portal: **Canvas Cloud** shows the link reachable and live updates connected;
**Canvases** lists the same canvases as before.

### 7. Verify with a client and spot-check documents

- Open Copper: the canvas list, an open canvas tab, chat, the mentions inbox, pending
  invites, and a share link (`copper://canvas/join/…`) all behave as before; an edit appears
  live on a second Mac.
- Compare a few documents with what they held before. With your own account (any member of
  the canvas), `GET /v1/canvases/{id}/read?full=true` through copper-cloud returns the same
  JSON as the 0.8 server did. To capture the "before" side, run the first two commands in
  step 1 before stopping the old server, and the rest after step 6:

  ```sh
  H="X-Copper-Instance: <your access key or instance key>"; U=https://cloud.example.com
  T=$(timeout 15 curl -fsSk -H "$H" "$U/v1/auth/login" \
        -d '{"email":"you@example.com","password":"…","device":{"name":"migration check"}}' | jq -r .token)
  timeout 30 curl -fsSk -H "$H" -H "Authorization: Bearer $T" "$U/v1/canvases/$ID/read?full=true" | jq -S . > before.json
  # after step 6:
  timeout 30 curl -fsSk -H "$H" -H "Authorization: Bearer $T" "$U/v1/canvases/$ID/read?full=true" | jq -S . > after.json
  diff before.json after.json && echo identical
  timeout 15 curl -fsSk -H "$H" -H "Authorization: Bearer $T" -X POST "$U/v1/auth/logout" -d '{}'
  ```

  Or ask Canvas Cloud directly, as that user (same answer as through the proxy):

  ```sh
  timeout 30 curl -fsS -H "Authorization: Bearer $(cat /root/canvas-app.key)" -H "X-Canvas-User: <member's Copper user id>" \
    "http://127.0.0.1:8480/v1/canvases/$ID/read?full=true" | jq -S .
  ```

- Then `shred -u /root/canvas-app.key`.

## Re-runs, `--only` and `--replace`

| Command | What it does |
|---|---|
| `canvas migrate` | every legacy canvas not yet `migrated` in the ledger; ledger-`migrated` canvases are reported `already` without contacting Canvas Cloud |
| `canvas migrate --only ID [--only ID]…` | just those canvases, **even if** the ledger says migrated: a canvas that exists on Canvas Cloud with the digest the ledger recorded is reported `already` (and an `imported` row is confirmed as `migrated`); different content fails with "use --replace". An id that is not a legacy canvas is reported failed |
| `canvas migrate --replace` | every legacy canvas, ledger ignored, imported with `mode=replace`: a canvas that already exists on Canvas Cloud is deleted and re-imported atomically from the legacy data (its room is closed first). Only canvases that an import by this app created can be replaced. **Edits made on Canvas Cloud since are lost** |
| `canvas migrate --only ID --replace` | overwrite one canvas from the legacy data |
| `--concurrency N` | canvases in flight at once, 1–16 (default 2) |
| `--dry-run` | read + decrypt + count, send nothing, write nothing |
| `--json` | the report as JSON instead of the table |

Without `--replace`, before uploading a canvas the tool asks Canvas Cloud for its digest; a
canvas that already exists is never uploaded again. It counts as done (`already`) only when
the ledger recorded exactly that digest — so a re-run after a crash or a partial run is safe
and cheap. Anything else needs a decision (`--replace`) and is reported failed.

A Personal canvas whose owner already has an **empty** Personal canvas on Canvas Cloud (created
automatically when their Copper listed canvases) replaces it. A **non-empty** one fails with
`personal_exists`; `--replace` overwrites it.

## The ledger

`canvas_migrations` in copper-cloud's database, one row per legacy canvas attempted:

| Column | |
|---|---|
| `canvas_id` (pk) | the legacy (= Canvas Cloud) canvas id |
| `status` | `imported` — accepted by Canvas Cloud, digest not yet verified (written before the check); `migrated` — counts and digest verified; `failed` — see `detail.error` |
| `digest` | lower-case hex SHA-256 of the canonical document (what Canvas Cloud stores) |
| `updates` | updates sent (the snapshot counts as one) |
| `migrated_at` | time of the last write |
| `detail` | JSON: `name`, `kind`, `owner`, `import_status` (`created` / `replaced` / `none` / `unverified`), `bytes`, `snapshot`, `members`, `invites`, `links`, `mentions`, `warnings`, `canvas_cloud` (URL), `tool` (version), `error` |

A failure never downgrades a `migrated` row unless Canvas Cloud stored something that did not
verify. `canvas status`, the portal and `GET /admin/api/canvas-link` show
`migrated` / `failed` / legacy counts. Query it directly:

```sh
sudo -u postgres psql -w -d copper_cloud -c \
  "SELECT canvas_id, status, updates, detail->>'name' AS name, detail->>'error' AS error
     FROM canvas_migrations ORDER BY migrated_at"
```

The ledger is kept after unlinking and is the record of what was moved where.

## Digest verification

The digest is the lower-case hex SHA-256 of a canonical JSON rendering of the whole Yjs
document (every root type, map keys sorted, arrays in order, numbers as `f64`, binary as
base64; an empty document hashes `{}`). It depends only on the content, not on how the
updates were encoded, ordered or compacted — so the import's compacted snapshot has the same
digest as the legacy updates. Canvas Cloud computes it at import time (returned in the import
response) and again on `GET /v1/apps/self/canvases/{id}/digest`; the tool requires both to
match, and the counts Canvas Cloud applied (`updates_applied`, `members`, `invites`, `links`,
`mentions`) to equal what it sent.

## Reading the report

```
canvas migrate → http://127.0.0.1:8480
users: 12 provisioned (12 new), 0 failed
ID                                    NAME                      KIND      UPDATES      BYTES  MEMBERS  STATUS    DIGEST        ERROR
0192f1d5-3c1a-7d2e-8f00-5b6a7c8d9e0f  Personal                  personal       41    1.2 MiB        1  migrated  4f1c9a03be27
0192f1d5-41aa-7c3d-9e11-2a3b4c5d6e7f  Roadmap                   shared        212   12.4 MiB        3  migrated  9b07e2d1c4aa
0192f1d6-0b2c-7e4f-8a22-1c2d3e4f5a6b  Notes                     shared          0        0 B        0  already   77de01a9f3c2  (recorded as migrated in the ledger)
total: 3 canvases — 2 migrated, 0 replaced, 1 already, 0 dry-run, 0 failed; 253 updates, 13.6 MiB, 4 members, 4 invites, 2 links, 1 mentions
ok
```

- **users**: on a real run, `provisioned (new)` / `failed`, with one `user <id> <email>: <error>`
  line per failure; a dry run only counts them.
- one row per canvas, in legacy creation order. `UPDATES` includes the snapshot; `BYTES` is
  the decrypted size of what was sent; `DIGEST` shows 12 characters; `ERROR` is the error, or a
  note in parentheses. Canvases skipped from the ledger show zero counts. `warning:` lines follow a row (e.g. an undecryptable mention excerpt
  sent empty, or an update over Canvas Cloud's default 32 MiB line limit).
- `STATUS`: `migrated` (created and verified), `replaced` (overwritten with `--replace` and
  verified), `already` (nothing to do), `dry-run`, `failed`.
- `error: …` = a run-level failure; the last line is `ok` or `FAILED` (exit `0` / `1`).

`--json` prints the same as one object: `canvas_cloud`, `dry_run`, `replace`, `started_at`,
`finished_at`, `users {total, provisioned, created, failed[{user_id, email, error}]}`,
`canvases[{id, name, kind, owner, status, updates, bytes, snapshot, members, invites, links,
mentions, digest, note, error, warnings}]`, `totals {canvases, migrated, replaced, already,
dry_run, failed, updates, bytes, members, invites, links, mentions}`, `error`, `ok`.

## Troubleshooting

Run-level errors stop the run (nothing more is imported); per-canvas errors fail that canvas
only.

| Message | Cause / fix |
|---|---|
| `no Canvas Cloud is linked (copper-cloud canvas link …)` | link first (step 3); a disabled link is fine for migrating |
| `canvas-cloud preflight: GET …/v1/apps/self/stats failed` + a transport error | Canvas Cloud not running / wrong URL / TLS: `timeout 10 curl -fsS <url>/v1/info`, `systemctl status canvas-cloud` |
| `… answered 401 … (the app key was rejected — revoked or wrong key?)` | the app was revoked or rotated: create/rotate it on Canvas Cloud and run `canvas link` again with the new key |
| `… answered 404 … (is this URL a Canvas Cloud? /v1/apps/self is missing)` | the URL is not a Canvas Cloud, or one without the linked-apps API |
| `the canvas-cloud app lacks allow_import (recreate it with canvas-cloud apps create --allow-import)` / `canvas-cloud refused the import (403) …; nothing was migrated by this run` / `not attempted: the app lacks allow_import` | create a new app with `--allow-import`, re-link with its key, re-run |
| `the canvas_migrations ledger table is missing — run copper-cloud's database migrations (0030_canvas_link.sql) first` | `sudo copper-cloud migrate` |
| `cannot unwrap the doc key of canvas …: is this copper-cloud's master_key?` | the config's `master_key` (or `COPPER_CLOUD_MASTER_KEY`) is not the one the canvases were written with: restore the original config |
| `N user(s) could not be provisioned with their copper user id; no canvas was migrated …` with `canvas-cloud mapped it to user … instead of … (email already used by another canvas-cloud account?)` | that Canvas Cloud already has an account with the user's email and the app trusts email: use a Canvas Cloud without overlapping accounts, or an app created with `--no-trust-email`, then re-link |
| `user …: missing from the users:batch response` / `<code>: <message>` | Canvas Cloud rejected that user (e.g. invalid email); fix the account in Copper or on Canvas Cloud |
| `provisioning users: POST …/users:batch answered …` | Canvas Cloud refused the batch; see its logs |
| `exists in canvas-cloud with different content (digest …; it changed since this ledger recorded it (edited in canvas-cloud?)) — use --replace …` | someone edited it on Canvas Cloud after the migration: leave it (the Canvas Cloud copy is newer) or `--only ID --replace` to restore the legacy content |
| `exists in canvas-cloud with different content (…; this ledger has no verified record of importing it) — use --replace …` | a canvas with that id exists on Canvas Cloud from another import (another copper-cloud database, an earlier attempt without a ledger): `--only ID --replace` after checking |
| `canvas exists in canvas-cloud but no digest is available (use --replace)` | the digest endpoint failed for an existing canvas; retry, or `--only ID --replace` |
| `409 personal_exists … — the owner already has a non-empty Personal canvas in canvas-cloud (use --replace)` | the owner used canvases on Canvas Cloud before the migration (e.g. copper-cloud ran linked): `--only ID --replace` overwrites their Canvas Cloud Personal canvas with the legacy one |
| `422 unknown_subject …` | a member/inviter/mention user is not provisioned (deleted between steps?): re-run |
| `verification failed: GET digest: HTTP 500` | Canvas Cloud stored the canvas but cannot read it back. Canvas Cloud before 0.1.0 could store an unreadable snapshot of a text edited inside an emoji (a yrs bug); 0.1.0 verifies snapshots. Upgrade Canvas Cloud, then `--only ID --replace` |
| `422 unverifiable_snapshot` | Canvas Cloud could not encode this canvas in a form that reads back identically (yrs bug with a text split inside an emoji); nothing was stored. The canvas stays on the legacy tables; report it with the canvas id |
| `422 bad_update` / `422 incomplete_history` | a legacy update does not decode, or updates are missing: the legacy document is damaged. Check it with the dry run and `GET …/read` on the old release |
| `413 …` | the canvas is bigger than Canvas Cloud's `limits.import_max_bytes` (default 512 MiB) or a line over `limits.import_max_line_bytes` (32 MiB): raise them on Canvas Cloud, restart it, re-run |
| `reading the legacy canvas: update seq N does not decrypt` / `the snapshot (seq N) does not decrypt` | a damaged or foreign row in the legacy tables; nothing was imported for that canvas |
| `count mismatch: sent (updates, members, invites, links, mentions) = …, canvas-cloud applied …` | Canvas Cloud stored something different from what was sent: report it; `--only ID --replace` to retry |
| `digest mismatch: the import returned … but canvas-cloud now stores …` | the canvas changed right after the import (someone was editing on Canvas Cloud), or a Canvas Cloud bug: make sure copper-cloud is stopped, `--only ID --replace` |
| `verification failed: GET digest: … (the ledger keeps status imported; re-run to verify)` | the digest check could not be made; re-run (the canvas is accepted once its digest matches) |
| `verification failed: canvas-cloud reports the imported canvas does not exist` | deleted on Canvas Cloud right after the import: re-run |
| `not a legacy canvas (no such id in copper-cloud's canvases table)` | wrong `--only` id |
| `Canvas Cloud did not answer in time` / timeouts | big canvases: the import timeout is 120 s + 2 s per MiB stored; lower `--concurrency`, check Canvas Cloud's load |

## After the migration

- The legacy tables stay as they were, read by nobody but `canvas migrate`. They are the
  rollback copy; keep them (and the pre-upgrade backup) until you no longer need to roll back.
  They are not exposed through any route and are not changed by 0.11.0, except that deleting a
  Copper user still removes their legacy rows (foreign-key cascade) and, through the admin
  API, legacy invites addressed to their email.
- **Backups now include the Canvas Cloud database** (and its config with its `master_key`):
  the canvases live there ([operations.md](operations.md#backups)).

## Rollback

The legacy tables are untouched, so the previous release still has every canvas as it was at
the moment of the migration. Make sure the new copper-cloud is fully stopped first
(`systemctl is-active copper-cloud` says `inactive`); the SQL below works as the
`postgres` superuser or as the database owner.

```sh
sudo systemctl stop copper-cloud
# Older releases refuse to start with a migration it does not know (sqlx: "migration 30 was previously
# applied but is missing"): remove 0030's trigger, link tables and record; keep the ledger.
sudo -u postgres psql -w -d copper_cloud -v ON_ERROR_STOP=1 -c \
  "BEGIN; DROP TRIGGER canvas_link_user_created ON users; DROP FUNCTION canvas_link_user_created(); DROP TABLE canvas_link_outbox; DROP TABLE canvas_link; DELETE FROM _sqlx_migrations WHERE version = 30; COMMIT;"
tar xzf copper-cloud-0.10.0-linux-$(uname -m).tar.gz && sudo ./install.sh   # reinstall the previous release
```

(Or restore the pre-upgrade dump / RDS snapshot — that also discards every sync and account
change made since.) `canvas_migrations` is created with `IF NOT EXISTS`, so upgrading again later
re-applies `0030` cleanly; link again then.

What happens to work done after the cutover:

- **Canvas edits, chat, new canvases, invites, share links and mentions made on Canvas Cloud
  after the migration are not in the legacy tables** and are not copied back. After a rollback,
  Coppers see each canvas as it was at migration time; canvases created after the cutover are
  missing from the list, so Copper removes them and closes their tabs. They still exist on
  Canvas Cloud (export them from there if needed). A Copper that still holds a newer version of
  an open document may push it back into the legacy room when it reconnects — do not rely on it.
- Sync data, accounts and devices are unaffected (same tables in both releases).
- Users deleted after the upgrade have lost their legacy canvases too (cascade).
- Upgrading again after a rollback: the ledger still says `migrated` and Canvas Cloud still has
  the earlier imports, so a plain `canvas migrate` reports them `already` even if they were
  edited on the old release in between. Use `canvas migrate --replace` to bring the legacy
  content over again (this overwrites what Canvas Cloud has).
