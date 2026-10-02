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
  | "pairing_code.revoke";

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
