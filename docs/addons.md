# Addons

An **addon** is a small web bundle (HTML, JavaScript, CSS and a `manifest.json`) written for
one **slot** of the Copper window. The admin uploads it to Copper Cloud, picks who gets it
(everyone, or listed people) and fills in its settings. Every signed-in Copper asks the
cloud which addons apply to its person, downloads them, verifies them and shows them in
their slot. Nothing is installed on anyone's machine by hand.

Addons are off by default: an upload reaches nobody until an admin turns it on.

- [Slots](#slots)
- [Manifest](#manifest-manifestjson-schema-copper-addon1)
- [Bundles](#bundles)
- [Who gets what](#who-gets-what-resolution)
- [Admin API](#admin-api) · [Client API](#client-api) · [CLI](#cli)
- [Security](#security)

## Slots

A slot is a region of the Copper window that an addon replaces. The list is closed: it is
compiled into every Copper and into the cloud, and uploads for any other slot are refused.

| Slot | Where | Replaces |
|---|---|---|
| `agent-sidebar` | The pane on the right of the page, full height, toggled by ⌘E (Mac) or Ctrl+E (Linux, Windows) | The built-in agent pane |

At most one addon shows in a slot. A Copper that does not know a slot (an older version)
ignores addons for it.

## Manifest (`manifest.json`, schema `copper-addon/1`)

```json
{
  "schema": "copper-addon/1",
  "id": "com.example.notes-sidebar",
  "name": "Notes sidebar",
  "version": "1.0.0",
  "slot": "agent-sidebar",
  "entry": "index.html",
  "description": "Team notes beside the page.",
  "author": "Example Inc.",
  "bridge": 1,
  "permissions": {
    "hosts": ["api.example.com", "*.example.org"],
    "capabilities": ["identity", "storage", "fetch"]
  },
  "settings": {
    "apiUrl": { "type": "string", "default": "https://api.example.com", "title": "API URL" },
    "pageSize": { "type": "number", "default": 20 }
  },
  "platforms": ["mac", "linux", "windows"]
}
```

| Field | Rule |
|---|---|
| `schema` | `"copper-addon/1"` |
| `id` | `^[a-z0-9][a-z0-9.-]{2,63}$`, no `..`, contains a `.` (vendor prefix, e.g. `com.example.x`). The addon's identity across versions. |
| `name` | 1–60 characters |
| `version` | `MAJOR.MINOR.PATCH` with an optional `-prerelease` (no `+build`) |
| `slot` | a known slot id |
| `entry` | a bundle path ending in `.html` that exists in the bundle |
| `description` | optional, ≤ 280 characters. `author`: optional, ≤ 200. |
| `bridge` | integer ≥ 1: the `window.copper` bridge version the addon needs (Coppers refuse a higher one) |
| `permissions.hosts` | ≤ 32 entries, each a lowercase hostname or `*.` + hostname (two labels at least), optional `:port`, optional `https://`. `http://` only for `localhost` / `127.0.0.1`. |
| `permissions.capabilities` | subset of `identity storage fetch oauth open-url page agent` |
| `settings` | optional; key → `{type, default?, title?, description?}`, `type` one of `string number boolean object array`; at most 64 settings |
| `platforms` | optional, non-empty subset of `mac linux windows`; absent = all |

Unknown top-level keys are kept (and served) but ignored.

## Bundles

**Upload format.** A zip file, extension `.copperaddon`, with `manifest.json` at its root or
inside a single top-level folder (stripped). Zip it from inside the folder or zip the folder
itself; both work.

- Paths: `^[A-Za-z0-9._@-]+(/[A-Za-z0-9._@-]+)*$`, no `.` or `..` segment, ≤ 200 characters.
- Refused: absolute paths, backslashes, drive letters, `..`, symlinks, encrypted entries, and
  compression other than Stored / Deflate.
- Dropped silently: directories (implied), `__MACOSX/`, `.DS_Store`.
- Limits: ≤ 1000 files, ≤ 10 MB decoded, ≤ 14 MB served (below), ≤ `limits.max_addon_bytes`
  (default 16 MiB) uploaded; an entry that expands more than 100× (above 256 KiB) is refused
  as a likely zip bomb.

**Served format.** The cloud turns an upload into one canonical JSON document, so clients
need no zip code:

```json
{
  "format": "copper-addon-bundle/1",
  "manifest": { "…": "the manifest exactly as uploaded" },
  "files": [
    { "path": "index.html", "size": 1234, "sha256": "<hex>", "data": "<base64>" }
  ]
}
```

Files are sorted by path (bytewise), `data` is standard padded base64, and the document is
compact JSON. The **lowercase hex SHA-256 of the exact served bytes is the version's
identity** (`sha256` everywhere below). `manifest.json` is one of the files.

**Versions.** Uploading a new `id` creates the addon, **off**, for nobody. Uploading a new
`version` of an existing addon stores it and makes it current. Uploading a version that is
already stored with the same contents changes nothing; with different contents it is refused
with `409 version_exists` (bump `version`). Older versions stay stored; an admin can make any
of them current again (`current_version`). An addon keeps its slot for life.

## Who gets what (resolution)

For a person (matched by their account email, case-insensitively):

1. Take the **enabled** addons whose audience is `everyone`, or `people` with that email in
   the list. Emails can be listed before the person signs up; they get the addon once they do.
2. Per slot, pick the highest `priority` (−1000…1000, default 0), then the most recently
   changed.

Disabled addons are never listed. The person gets the **current** version, with settings =
manifest defaults overlaid by the admin's values (values for settings the current manifest no
longer declares, or of the wrong type, are ignored).

Copper downloads a new addon or version, verifies it and shows **Update available**; the
person installs it from Copper's update UI. Removals and settings-only changes apply at once.
A person can switch an addon off locally in Settings › Addons.

## Admin API

Under `/admin/api/addons` (same session cookie, CSRF header and `Cache-Control: no-store` as
every admin route). Full shapes in [admin-api.md](admin-api.md#addons).

| Route | |
|---|---|
| `GET addons/slots` | the slot list |
| `GET addons` | every addon (plain array) |
| `POST addons?slot=agent-sidebar` | upload a `.copperaddon` (raw body, `Content-Type: application/zip`) |
| `GET addons/{id}` | one addon |
| `PATCH addons/{id}` | `enabled`, `audience`, `people`, `settings`, `priority`, `current_version` |
| `DELETE addons/{id}` | the addon and every version |
| `GET addons/{id}/versions/{version}/bundle` | a stored version's served bundle |

Every change bumps the addon's `revision`, writes an audit row (`addon.create`,
`addon.upload`, `addon.update`, `addon.delete`) and sends the `addons` event to every
connected Copper.

## Client API

Device session auth, like `/v1/intelligence`. Full shapes in [api.md](api.md#addons).

- `GET /v1/addons` → `{revision, addons:[…]}` for the caller; `ETag: "<revision>"`, and a
  matching `If-None-Match` → `304`. A server without addons answers `404`: treat it as "no
  addons". `GET /v1/info` lists `"addons"` in `features`.
- `GET /v1/addons/{id}/bundle/{sha256}` → the served bundle, only while that version is what
  the caller resolves to (else `404`); immutable.
- SSE `event: addons` on `/v1/sync/events` → refetch `GET /v1/addons` with your last revision.

## CLI

Straight against the database, sharing the code path (and audit trail, `via: "cli"`) with the
admin API. `copper-cloud admin addons …` is the same command.

```sh
copper-cloud addons slots
copper-cloud addons upload notes-sidebar.copperaddon --slot agent-sidebar
copper-cloud addons enable com.example.notes-sidebar --people ana@example.com,bob@example.com
copper-cloud addons enable com.example.notes-sidebar --everyone --priority 10
copper-cloud addons settings com.example.notes-sidebar --json '{"apiUrl":"https://api.example.com"}'
copper-cloud addons list            # --json for the admin API's shape
copper-cloud addons disable com.example.notes-sidebar
copper-cloud addons delete com.example.notes-sidebar --yes
```

Every id / file argument also takes the flag form (`--id ID`, `--file FILE`). `settings
--file FILE` (or `-` for stdin) replaces `--json`. The CLI runs outside the server process, so
it cannot push the `addons` event: Coppers see CLI changes at their next check (launch, update
check, or the next portal change).

## Security

- **Admin-only writes.** Only admins (portal / admin API, or root on the host via the CLI)
  upload, enable, target or configure addons. Every change is audited; settings *values* are
  never written to the audit log (only the keys), since they may hold credentials.
- **No code runs on the server.** Uploads are parsed (zip directory, `manifest.json`) and
  re-encoded; nothing in them is executed or served as a web page by the cloud.
- **Hostile zips.** Paths are validated before anything else, symlinks / traversal /
  absolute paths are refused, the decoded size is capped while inflating (not trusted from
  the zip header) and high expansion ratios are refused.
- **Integrity.** Clients download a bundle by its SHA-256 and check it, then check every
  file's own SHA-256, and refuse a manifest that disagrees with what `/v1/addons` announced.
- **Least exposure.** `GET /v1/addons` is per person and `no-store` (settings may be secrets);
  a bundle is served only to people it currently resolves to.
- **In Copper,** an addon runs in an isolated web view for its slot with only the hosts and
  bridge capabilities its manifest declares.
