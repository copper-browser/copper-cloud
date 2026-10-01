/**
 * Typed client for the copper-cloud admin API (`/admin/api/*`, see
 * `docs/admin-api.md`). Every request:
 *  - is same-origin with the `cc_admin` cookie (`credentials: 'same-origin'`),
 *  - carries `X-Requested-With: copper-cloud-portal` (the CSRF header),
 *  - turns a session 401 into a redirect to `/login/`.
 *
 * `NEXT_PUBLIC_API_BASE` prefixes every URL (default '' = same origin).
 * `NEXT_PUBLIC_MOCK=1` answers from the in-memory fixture in `./mock`.
 */
import type {
  AccessKey,
  AccessKeyQuery,
  ApiErrorBody,
  AuditEntry,
  Canvas,
  CanvasQuery,
  CreateAccessKey,
  CreatedAccessKey,
  Device,
  DeviceQuery,
  DevicesDeleted,
  LoginResponse,
  Me,
  Ok,
  Overview,
  Page,
  PageQuery,
  PairingCode,
  PairingCodeQuery,
  PasswordChanged,
  Settings,
  SettingsPatch,
  User,
  UserPatch,
  UserQuery,
} from "./types";

export const API_BASE = (process.env.NEXT_PUBLIC_API_BASE ?? "").replace(/\/+$/, "");
export const MOCK_MODE = process.env.NEXT_PUBLIC_MOCK === "1";
export const CSRF_HEADER = "X-Requested-With";
export const CSRF_VALUE = "copper-cloud-portal";
export const LOGIN_PATH = "/login/";

/** Largest page the server accepts; lists here are small enough to load whole. */
export const MAX_PAGE = 500;

export class ApiError extends Error {
  readonly status: number;
  readonly code: string;
  readonly retryAfter: number | null;

  constructor(
    status: number,
    code: string,
    message: string,
    retryAfter: number | null = null,
  ) {
    super(message);
    this.name = "ApiError";
    this.status = status;
    this.code = code;
    this.retryAfter = retryAfter;
  }
}

/** Human message for any thrown value, for toasts and inline errors. */
export function errorMessage(error: unknown): string {
  if (error instanceof ApiError) return error.message;
  if (error instanceof Error) return error.message;
  return "Something went wrong.";
}

type Query = Record<string, string | number | boolean | null | undefined>;

interface RequestOptions {
  method?: "GET" | "POST" | "PATCH" | "DELETE";
  body?: unknown;
  query?: Query;
  signal?: AbortSignal;
  /** Default true. `login` handles its own 401s. */
  redirectOn401?: boolean;
}

const FALLBACK_MESSAGES: Record<string, string> = {
  bad_request: "The server rejected that request.",
  admin_session: "Your session ended. Sign in again.",
  credentials: "Wrong email or password.",
  csrf: "The server refused the request (missing portal header).",
  not_found: "That item no longer exists.",
  conflict: "That conflicts with the current state. Reload and try again.",
  payload_too_large: "That request is too large.",
  rate_limited: "Too many attempts. Wait a minute and try again.",
  internal: "The server hit an error. Check its logs and try again.",
};

function buildQuery(query?: Query): string {
  if (!query) return "";
  const params = new URLSearchParams();
  for (const [key, value] of Object.entries(query)) {
    if (value === undefined || value === null || value === "") continue;
    params.set(key, String(value));
  }
  const s = params.toString();
  return s ? `?${s}` : "";
}

let redirecting = false;

/** Send the browser to `/login/`, remembering where it was. Never resolves. */
export function redirectToLogin(): Promise<never> {
  if (typeof window !== "undefined" && !redirecting) {
    const here = window.location.pathname + window.location.search;
    if (!window.location.pathname.startsWith(LOGIN_PATH)) {
      redirecting = true;
      const next = here && here !== "/" ? `?next=${encodeURIComponent(here)}` : "";
      window.location.replace(`${LOGIN_PATH}${next}`);
    }
  }
  return new Promise<never>(() => {});
}

async function transport(url: string, init: RequestInit): Promise<Response> {
  // Inline env check (not MOCK_MODE) so the bundler drops this branch and the
  // mock chunk from production builds.
  if (process.env.NEXT_PUBLIC_MOCK === "1") {
    const { mockFetch } = await import("./mock");
    return mockFetch(url, init);
  }
  return fetch(url, init);
}

async function request<T>(path: string, options: RequestOptions = {}): Promise<T> {
  const { method = "GET", body, query, signal, redirectOn401 = true } = options;
  const headers: Record<string, string> = {
    Accept: "application/json",
    [CSRF_HEADER]: CSRF_VALUE,
  };
  if (body !== undefined) headers["Content-Type"] = "application/json";

  const url = `${API_BASE}/admin/api/${path}${buildQuery(query)}`;
  let res: Response;
  try {
    res = await transport(url, {
      method,
      credentials: "same-origin",
      cache: "no-store",
      headers,
      body: body === undefined ? undefined : JSON.stringify(body),
      signal,
    });
  } catch (error) {
    if (error instanceof DOMException && error.name === "AbortError") throw error;
    throw new ApiError(
      0,
      "network",
      "Can't reach the server. Check that copper-cloud is running.",
    );
  }

  if (res.ok) {
    if (res.status === 204) return undefined as T;
    return (await res.json()) as T;
  }

  let payload: Partial<ApiErrorBody> = {};
  try {
    payload = (await res.json()) as ApiErrorBody;
  } catch {
    // non-JSON error (proxy, crash page): fall through to the status fallback
  }
  const code =
    payload.error ?? (res.status === 401 ? "admin_session" : `http_${res.status}`);

  // A 401 means "no admin session" everywhere except a wrong password on
  // `login` / `password`, which come back as `credentials`.
  if (res.status === 401 && code !== "credentials" && redirectOn401) {
    return redirectToLogin();
  }

  const retryAfter = Number(res.headers.get("Retry-After")) || null;
  const message =
    code === "credentials" || code === "rate_limited"
      ? FALLBACK_MESSAGES[code]
      : payload.message && payload.message !== "forbidden"
        ? capitalize(payload.message)
        : (FALLBACK_MESSAGES[code] ?? `Request failed (HTTP ${res.status}).`);
  throw new ApiError(res.status, code, message, retryAfter);
}

function capitalize(s: string): string {
  const t = s.trim();
  if (!t) return t;
  const sentence = t.charAt(0).toUpperCase() + t.slice(1);
  return /[.!?]$/.test(sentence) ? sentence : `${sentence}.`;
}

const page = (q?: PageQuery): Query => ({
  limit: q?.limit ?? MAX_PAGE,
  offset: q?.offset,
});
const id = (value: string) => encodeURIComponent(value);

export const api = {
  // Session
  login: (email: string, password: string) =>
    request<LoginResponse>("login", {
      method: "POST",
      body: { email, password },
      redirectOn401: false,
    }),
  logout: () => request<Ok>("logout", { method: "POST", redirectOn401: false }),
  me: (opts: { signal?: AbortSignal; redirectOn401?: boolean } = {}) =>
    request<Me>("me", opts),
  changePassword: (oldPassword: string, newPassword: string) =>
    request<PasswordChanged>("password", {
      method: "POST",
      body: { old: oldPassword, new: newPassword },
    }),

  // Instance
  overview: (signal?: AbortSignal) => request<Overview>("overview", { signal }),
  settings: (signal?: AbortSignal) => request<Settings>("settings", { signal }),
  updateSettings: (patch: SettingsPatch) =>
    request<Settings>("settings", { method: "PATCH", body: patch }),

  // Access keys
  accessKeys: {
    list: (q?: AccessKeyQuery, signal?: AbortSignal) =>
      request<Page<AccessKey>>("access-keys", {
        query: { ...page(q), status: q?.status },
        signal,
      }),
    create: (input: CreateAccessKey) =>
      request<CreatedAccessKey>("access-keys", { method: "POST", body: input }),
    revoke: (keyId: string) =>
      request<AccessKey>(`access-keys/${id(keyId)}`, { method: "DELETE" }),
  },

  // People
  users: {
    list: (q?: UserQuery, signal?: AbortSignal) =>
      request<Page<User>>("users", { query: { ...page(q), q: q?.q }, signal }),
    update: (userId: string, patch: UserPatch) =>
      request<User>(`users/${id(userId)}`, { method: "PATCH", body: patch }),
    remove: (userId: string) => request<Ok>(`users/${id(userId)}`, { method: "DELETE" }),
    resetPassword: (userId: string, password: string) =>
      request<PasswordChanged>(`users/${id(userId)}/reset-password`, {
        method: "POST",
        body: { password },
      }),
  },

  // Devices
  devices: {
    list: (q?: DeviceQuery, signal?: AbortSignal) =>
      request<Page<Device>>("devices", {
        query: { ...page(q), user_id: q?.user_id },
        signal,
      }),
    /** Pass `userId` to remove exactly that account's row for the device. */
    remove: (deviceId: string, userId?: string) =>
      request<DevicesDeleted>(`devices/${id(deviceId)}`, {
        method: "DELETE",
        query: { user_id: userId },
      }),
  },

  // Canvases (metadata only; content is never exposed)
  canvases: {
    list: (q?: CanvasQuery, signal?: AbortSignal) =>
      request<Page<Canvas>>("canvases", { query: { ...page(q), kind: q?.kind }, signal }),
    remove: (canvasId: string) =>
      request<Ok>(`canvases/${id(canvasId)}`, { method: "DELETE" }),
  },

  // Pairing codes (active only)
  pairingCodes: {
    list: (q?: PairingCodeQuery, signal?: AbortSignal) =>
      request<Page<PairingCode>>("pairing-codes", {
        query: { ...page(q), user_id: q?.user_id },
        signal,
      }),
    revoke: (codeId: string) =>
      request<Ok>(`pairing-codes/${id(codeId)}`, { method: "DELETE" }),
  },

  // Audit
  audit: (q?: PageQuery, signal?: AbortSignal) =>
    request<Page<AuditEntry>>("audit", {
      query: { limit: q?.limit ?? 20, offset: q?.offset },
      signal,
    }),
};

export type Api = typeof api;
