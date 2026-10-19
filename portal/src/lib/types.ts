/**
 * Wire types for `/admin/api/*`. Source of truth: `docs/admin-api.md`.
 * Timestamps are RFC 3339 UTC strings; ids are UUID strings unless noted.
 */

export type AccessMode = "open" | "directory";
export type TlsMode = "self-signed" | "acme" | "off";

export interface Page<T> {
  items: T[];
  total: number;
  limit: number;
  offset: number;
}

export interface PageQuery {
  limit?: number;
  offset?: number;
}

export interface ApiErrorBody {
  error: string;
  message?: string;
}

// Session -----------------------------------------------------------------

export interface Admin {
  id: string;
  email: string;
  created_at: string;
  last_login_at: string | null;
}

export interface LoginResponse {
  admin: Admin;
}

export interface Me {
  admin: Admin;
  session: { created_at: string; expires_at: string };
}

export interface Ok {
  ok: true;
}

export interface PasswordChanged extends Ok {
  revoked_sessions: number;
}

// Instance ----------------------------------------------------------------

export interface Counts {
  users: number;
  devices: number;
  canvases: number;
  /** Active keys only (not revoked, expired or exhausted). */
  access_keys: number;
  live_rooms: number;
  live_peers: number;
}

export interface Overview {
  version: string;
  uptime_s: number;
  access_mode: AccessMode;
  allow_signup: boolean;
  counts: Counts;
  /** `host:port`, no scheme. */
  public_url: string;
  tls: { mode: TlsMode; fingerprint: string | null };
}

export interface Settings {
  access_mode: AccessMode;
  allow_signup: boolean;
  /** Secret. Works only in `open` mode. `null` if the certificate is unreadable. */
  instance_link_code: string | null;
}

export interface SettingsPatch {
  access_mode?: AccessMode;
  allow_signup?: boolean;
}

// Intelligence (AI) keys ---------------------------------------------------

/** Cloud-wide Jev key as the admin sees it: never the key, only its last 4 characters. */
export interface JevKeyView {
  /** `""` when the key is too short to reveal any characters. */
  key_last4: string;
  endpoint: string;
  model: string;
}

export interface RouterKeyView {
  key_last4: string;
  url: string;
}

/** Org-wide agent round budget: tool-call rounds per question, 1–500. Not a key. */
export interface AgentSettings {
  max_turns: number;
}

export interface IntelligenceSettings {
  /** Signed-in Coppers receive the keys (GET /v1/intelligence) while true. */
  enabled: boolean;
  jev: JevKeyView | null;
  router: RouterKeyView | null;
  /** `null` = each person's own Copper setting. Served regardless of `enabled`. */
  agent: AgentSettings | null;
  updated_at: string | null;
  /** Admin email, or `"cli"` for `copper-cloud intelligence …`. */
  updated_by: string | null;
  defaults: {
    jev_endpoint: string;
    jev_model: string;
    /** Copper's own default when the org sets no budget. */
    agent_max_turns: number;
  };
}

/** `PUT intelligence`: omit a block to keep it, `null` clears it; omit `key` to keep the stored one. */
export interface IntelligenceUpdate {
  jev?: { key?: string; endpoint?: string; model?: string } | null;
  router?: { key?: string; url?: string } | null;
  agent?: AgentSettings | null;
  enabled?: boolean;
}

// Access keys -------------------------------------------------------------

export type AccessKeyStatus = "active" | "revoked" | "expired" | "exhausted";

export interface AccessKey {
  id: string;
  label: string;
  email: string | null;
  created_at: string;
  /** Admin email; `null` for keys minted by pairing or whose admin was deleted. */
  created_by: string | null;
  expires_at: string | null;
  revoked_at: string | null;
  last_used_at: string | null;
  uses: number;
  max_uses: number | null;
  status: AccessKeyStatus;
}

/** `POST access-keys` response: the only time `key` and `link_code` are visible. */
export interface CreatedAccessKey extends AccessKey {
  key: string;
  link_code: string;
}

export interface CreateAccessKey {
  label: string;
  email?: string | null;
  expires_in_days?: number | null;
  max_uses?: number | null;
}

export interface AccessKeyQuery extends PageQuery {
  status?: AccessKeyStatus;
}

// People ------------------------------------------------------------------

export interface User {
  id: string;
  email: string;
  display_name: string | null;
  created_at: string;
  last_seen_at: string | null;
  disabled: boolean;
  device_count: number;
  canvas_count: number;
}

export interface UserPatch {
  disabled?: boolean;
  display_name?: string;
}

export interface UserQuery extends PageQuery {
  q?: string;
}

// Devices -----------------------------------------------------------------

export interface Device {
  id: string;
  user_id: string;
  user_email: string;
  name: string;
  created_at: string;
  last_seen_at: string | null;
}

export interface DeviceQuery extends PageQuery {
  user_id?: string;
}

export interface DevicesDeleted extends Ok {
  deleted: number;
}

// Canvases ----------------------------------------------------------------

export type CanvasKind = "personal" | "shared";

export interface Canvas {
  id: string;
  name: string;
  kind: CanvasKind;
  owner_id: string;
  owner_email: string;
  member_count: number;
  share_link_count: number;
  created_at: string;
  updated_at: string;
}

export interface CanvasQuery extends PageQuery {
  kind?: CanvasKind;
}

// Pairing codes -----------------------------------------------------------

export interface PairingCode {
  id: string;
  user_id: string;
  user_email: string;
  device_name: string | null;
  created_by_device: string | null;
  created_at: string;
  expires_at: string;
}

export interface PairingCodeQuery extends PageQuery {
  user_id?: string;
}

// Audit -------------------------------------------------------------------

export type AuditAction =
  | "login"
  | "logout"
  | "password.change"
  | "settings.update"
  | "access_key.create"
  | "access_key.revoke"
  | "user.update"
  | "user.delete"
  | "user.reset_password"
  | "device.delete"
  | "canvas.delete"
  | "pairing_code.revoke"
  | "intelligence.update"
  | "intelligence.clear"
  | "intelligence.read"
  | "addon.create"
  | "addon.upload"
  | "addon.update"
  | "addon.delete";

export interface AuditEntry {
  /** Sequence number, not a UUID. */
  id: number;
  admin_id: string | null;
  admin_email: string | null;
  /** One of {@link AuditAction}; kept open so new server actions still render. */
  action: AuditAction | (string & {});
  target: string | null;
  detail: Record<string, unknown> | null;
  at: string;
}

// Single sign-on (copper-cloud ≥ 0.9.0; older servers answer 404) ----------

export type SsoProviderKind = "google" | "oidc";

export interface SsoProvider {
  /** Slug used in URLs and link codes: `"google"`, `"oidc"`. */
  id: string;
  /** Kept open so a newer server's kinds still render. */
  kind: SsoProviderKind | (string & {});
  /** Button label in Copper: "Continue with <name>". */
  name: string;
  client_id: string;
  /** The secret itself is write-only. */
  has_secret: boolean;
  /** OIDC issuer URL; `null` for Google. */
  issuer: string | null;
  /** Lower-case email domains allowed to sign in, e.g. `["example.com"]`. */
  domains: string[];
  /** Create an account on first sign-in for an allowed email. */
  auto_create: boolean;
  enabled: boolean;
  updated_at: string | null;
}

export interface SsoSettings {
  /** True while at least one provider is enabled (read-only). */
  enabled: boolean;
  /** Password sign-up/sign-in for people (admins are unaffected). */
  password_signin: boolean;
  providers: SsoProvider[];
  /** `copper-cloud://HOST:PORT/#sso=<id>&fp=HEX`; `null` while no provider is on. */
  join_link: string | null;
  /** `https://HOST/start` landing page to share; `null` while no provider is on. */
  start_url: string | null;
}

/** `PUT sso/providers/{id}`: omit `client_secret` to keep the stored one. */
export interface SsoProviderInput {
  kind: SsoProviderKind;
  name: string;
  client_id: string;
  client_secret?: string;
  issuer?: string;
  domains: string[];
  auto_create: boolean;
  enabled: boolean;
}

export interface SsoPatch {
  password_signin: boolean;
}

export interface SsoTestCheck {
  /** Short label, e.g. "Token endpoint". */
  name: string;
  ok: boolean;
  /** What happened, may include the provider's error code (`invalid_client`). */
  detail: string | null;
}

export interface SsoTestResult {
  ok: boolean;
  checks: SsoTestCheck[];
}

// Agent access (integration keys) -------------------------------------------

export interface Integration {
  id: string;
  name: string;
  /** Allowed email domains; empty = the single sign-on domains. */
  domains: string[];
  /** May create an account for an allowed email that has none. */
  create_users: boolean;
  created_at: string;
  last_used_at: string | null;
  revoked_at: string | null;
  device_count: number;
}

export interface CreateIntegration {
  name: string;
  /** Omit to use the single sign-on domains. */
  domains?: string[];
  create_users: boolean;
}

/** `POST integrations`: the only time `key` (`cai_…`) is visible. */
export interface CreatedIntegration {
  integration: Integration;
  key: string;
}

/** `POST integrations/{id}/rotate`: the new key, shown once. */
export interface RotatedIntegrationKey {
  key: string;
}

export interface IntegrationDevice {
  user_email: string;
  device_id: string;
  name: string;
  last_seen_at: string | null;
}

// Addons ------------------------------------------------------------------
//
// Every addon field the portal reads is declared here, and `normalizeAddon`
// is the only code that touches the raw JSON. If the server's row shape moves,
// this section is the one place to change.

export type AddonAudience = "everyone" | "people";
export type AddonPlatform = "mac" | "linux" | "windows";
export type AddonSettingType = "string" | "number" | "boolean" | "object" | "array";

/** One entry of `manifest.settings`. */
export interface AddonSettingSpec {
  type: AddonSettingType;
  default?: unknown;
  title?: string;
  description?: string;
}

/** `manifest.json`, schema `copper-addon/1`. Unknown keys are kept. */
export interface AddonManifest {
  schema: string;
  id: string;
  name: string;
  version: string;
  slot: string;
  entry: string;
  description?: string;
  author?: string;
  bridge: number;
  permissions?: { hosts?: string[]; capabilities?: string[] };
  settings?: Record<string, AddonSettingSpec>;
  platforms?: AddonPlatform[];
  [key: string]: unknown;
}

/** `GET addons/slots`: the closed list of slots this server accepts. */
export interface AddonSlot {
  id: string;
  name: string;
  description: string;
}

/** Someone in an addon's audience. Emails work before the person signs up. */
export interface AddonPerson {
  email: string;
  name: string | null;
  /** `null` until someone with this email has an account. */
  user_id: string | null;
}

export interface AddonVersion {
  version: string;
  /** Lowercase hex SHA-256 of the served bundle. */
  sha256: string;
  size_bytes: number;
  created_at: string;
  /** Admin email; `null` for the server CLI or a deleted admin. */
  uploaded_by: string | null;
}

/** A row of `GET addons` (also `GET`/`PATCH addons/:id` and the upload response). */
export interface Addon {
  /** The manifest id, e.g. `example.notes-sidebar`. */
  id: string;
  slot: string;
  name: string;
  /** Off by default. */
  enabled: boolean;
  audience: AddonAudience;
  people: AddonPerson[];
  /** Admin-set values only; manifest defaults fill the rest. */
  settings: Record<string, unknown>;
  priority: number;
  revision: number;
  current: AddonVersion | null;
  /** Newest first. */
  versions: AddonVersion[];
  /** The current version's manifest. */
  manifest: AddonManifest | null;
  created_at: string;
  updated_at: string;
}

/** `PATCH addons/:id`. `people` replaces the whole set. */
export interface AddonPatch {
  enabled?: boolean;
  audience?: AddonAudience;
  people?: string[];
  settings?: Record<string, unknown>;
  priority?: number;
  current_version?: string;
}

type Raw = Record<string, unknown>;
const isRaw = (v: unknown): v is Raw => typeof v === "object" && v !== null;
const text = (v: unknown, fallback = ""): string =>
  typeof v === "string" ? v : fallback;
const textOrNull = (v: unknown): string | null => (typeof v === "string" && v ? v : null);
const num = (v: unknown, fallback = 0): number =>
  typeof v === "number" && Number.isFinite(v) ? v : fallback;

function normalizeVersion(raw: unknown): AddonVersion | null {
  if (!isRaw(raw) || typeof raw.version !== "string") return null;
  return {
    version: raw.version,
    sha256: text(raw.sha256),
    size_bytes: num(raw.size_bytes ?? raw.size),
    created_at: text(raw.created_at ?? raw.uploaded_at),
    uploaded_by: textOrNull(raw.uploaded_by),
  };
}

function normalizePerson(raw: unknown): AddonPerson | null {
  if (typeof raw === "string") return { email: raw, name: null, user_id: null };
  if (!isRaw(raw) || typeof raw.email !== "string") return null;
  return {
    email: raw.email,
    name: textOrNull(raw.name ?? raw.display_name),
    user_id: textOrNull(raw.user_id ?? raw.id),
  };
}

const semverParts = (v: string) => v.split(/[.+-]/).map((p) => Number(p) || 0);
function newestFirst(a: AddonVersion, b: AddonVersion): number {
  const t = Date.parse(b.created_at) - Date.parse(a.created_at);
  if (t) return t;
  const [x, y] = [semverParts(a.version), semverParts(b.version)];
  for (let i = 0; i < 3; i++) if (x[i] !== y[i]) return (y[i] ?? 0) - (x[i] ?? 0);
  return 0;
}

/**
 * Raw `/admin/api/addons*` JSON → {@link Addon}. Tolerates both spellings the
 * docs have used (`size`/`size_bytes`, `uploaded_at`/`created_at`, people as
 * `{id,email,name}` or `{email,name,user_id}`) and missing optional blocks.
 */
export function normalizeAddon(raw: unknown): Addon {
  const r: Raw = isRaw(raw) ? raw : {};
  const manifest = isRaw(r.manifest) ? (r.manifest as AddonManifest) : null;
  const current = normalizeVersion(r.current);
  const versions = (Array.isArray(r.versions) ? r.versions : [])
    .map(normalizeVersion)
    .filter((v): v is AddonVersion => v !== null)
    .sort(newestFirst);
  if (current && !versions.some((v) => v.version === current.version)) {
    versions.unshift(current);
  }
  const people = (Array.isArray(r.people) ? r.people : [])
    .map(normalizePerson)
    .filter((p): p is AddonPerson => p !== null);
  return {
    id: text(r.id),
    slot: text(r.slot, manifest?.slot ?? ""),
    name: text(r.name, manifest?.name ?? text(r.id)),
    enabled: r.enabled === true,
    audience: r.audience === "everyone" ? "everyone" : "people",
    people,
    settings: isRaw(r.settings) && !Array.isArray(r.settings) ? r.settings : {},
    priority: num(r.priority),
    revision: num(r.revision, 1),
    current,
    versions,
    manifest,
    created_at: text(r.created_at),
    updated_at: text(r.updated_at),
  };
}

/** `GET addons` → rows. Accepts a bare array or a `{items}` / `{addons}` wrapper. */
export function normalizeAddonList(raw: unknown): Addon[] {
  const rows = Array.isArray(raw)
    ? raw
    : isRaw(raw) && Array.isArray(raw.items)
      ? raw.items
      : isRaw(raw) && Array.isArray(raw.addons)
        ? raw.addons
        : [];
  return rows.map(normalizeAddon);
}

/** What an upload did, when the server says (`created`, `new_version`, `unchanged`). */
export type AddonUploadOutcome = "created" | "new_version" | "unchanged";

export interface AddonUpload {
  addon: Addon;
  outcome: AddonUploadOutcome | null;
}

/**
 * `POST addons` → the row. The server sends the row with an `upload` field
 * (`created`, `new_version`, `unchanged`); a `{outcome, addon}` wrapper is
 * accepted too.
 */
export function normalizeAddonUpload(raw: unknown): AddonUpload {
  const r: Raw = isRaw(raw) ? raw : {};
  const wrapped = isRaw(r.addon) && typeof r.id !== "string";
  const said = r.upload ?? r.outcome;
  const outcome =
    said === "created" || said === "new_version" || said === "unchanged" ? said : null;
  return { addon: normalizeAddon(wrapped ? r.addon : raw), outcome };
}
