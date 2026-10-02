/**
 * In-memory stand-in for `/admin/api/*`, used when `NEXT_PUBLIC_MOCK=1`.
 * Implements every route in `docs/admin-api.md` (including mutations and
 * audit rows) against a realistic fixture so each page renders without a
 * server. State resets on reload; the signed-out flag survives in
 * sessionStorage so sign-in/out can be exercised.
 */
import type {
  AccessKey,
  AccessKeyStatus,
  Admin,
  AuditEntry,
  Canvas,
  CreatedAccessKey,
  Device,
  Overview,
  Page,
  PairingCode,
  Settings,
  User,
} from "./types";

const NOW = Date.now();
const MIN = 60_000;
const HOUR = 60 * MIN;
const DAY = 24 * HOUR;
const ago = (ms: number) => new Date(NOW - ms).toISOString();
const ahead = (ms: number) => new Date(NOW + ms).toISOString();
const fid = (prefix: string, n: number) =>
  `0192f1${prefix}-7c3a-7000-8000-${n.toString(16).padStart(12, "0")}`;

const HOST = "cloud.hollis.studio:443";
const FINGERPRINT = "9f86d081884c7d659a2feaa0c55ad015a3bf4f1b2b0b822cd15d6c15b0f00a08";
const INSTANCE_KEY = "ik_Zt4cQ1mJp8wVb2nRk7sXyE0fLh3aGd6uTo9iWq5vB1M";
const BOOTED_AT = NOW - (9 * DAY + 4 * HOUR + 17 * MIN);

const admin: Admin = {
  id: fid("c4", 1),
  email: "admin@hollis.studio",
  created_at: ago(62 * DAY),
  last_login_at: ago(14 * MIN),
};

interface UserRow extends Omit<User, "device_count" | "canvas_count"> {
  password: string;
}

const people: [string | null, string, number, number | null, boolean?][] = [
  ["Ana Pereira", "ana@hollis.studio", 41 * DAY, 3 * MIN],
  ["Marcus Webb", "marcus@hollis.studio", 40 * DAY, 58 * MIN],
  ["Priya Raman", "priya@hollis.studio", 38 * DAY, 12 * MIN],
  ["Jonas Lindqvist", "jonas.lindqvist@fastmail.com", 35 * DAY, 2 * DAY],
  ["Mei Tanaka", "mei@hollis.studio", 33 * DAY, 5 * HOUR],
  ["Tomás Ibarra", "tomas@hollis.studio", 30 * DAY, 26 * MIN],
  ["Sofia Rossi", "sofia@hollis.studio", 27 * DAY, 3 * DAY],
  ["Kwame Mensah", "kwame@hollis.studio", 22 * DAY, 8 * HOUR],
  ["Léa Martin", "lea@hollis.studio", 19 * DAY, 26 * HOUR],
  ["Daniel Kim", "daniel@hollis.studio", 15 * DAY, 41 * MIN],
  ["Hannah Okafor", "hannah@hollis.studio", 9 * DAY, 2 * HOUR],
  ["Ruth Abernathy", "ruth@hollis.studio", 60 * DAY, 24 * DAY, true],
  ["Grace Liu", "grace@hollis.studio", 3 * DAY, 6 * HOUR],
  [null, "ilya.petrov@proton.me", 1 * DAY, null],
];

const users: UserRow[] = people.map(([name, email, created, seen, disabled], i) => ({
  id: fid("d0", i + 1),
  email,
  display_name: name,
  created_at: ago(created),
  last_seen_at: seen === null ? null : ago(seen),
  disabled: disabled ?? false,
  password: "unused",
}));
const userByEmail = (email: string) => users.find((u) => u.email === email)!;

const deviceSpecs: [string, string, number, number | null][] = [
  ["ana@hollis.studio", "Ana's MacBook Pro", 41 * DAY, 3 * MIN],
  ["ana@hollis.studio", "Ana's Mac mini", 6 * DAY, 2 * HOUR],
  ["marcus@hollis.studio", "Marcus's MacBook Air", 40 * DAY, 58 * MIN],
  ["priya@hollis.studio", "Priya's MacBook Pro", 38 * DAY, 12 * MIN],
  ["priya@hollis.studio", "Studio iMac", 21 * DAY, 4 * DAY],
  ["jonas.lindqvist@fastmail.com", "Jonas's MacBook Pro", 35 * DAY, 2 * DAY],
  ["mei@hollis.studio", "Mei's MacBook Air", 33 * DAY, 5 * HOUR],
  ["tomas@hollis.studio", "Tomás's MacBook Pro", 30 * DAY, 26 * MIN],
  ["tomas@hollis.studio", "Tomás's Mac Studio", 12 * DAY, 20 * HOUR],
  ["sofia@hollis.studio", "Sofia's MacBook Air", 27 * DAY, 3 * DAY],
  ["kwame@hollis.studio", "Kwame's MacBook Pro", 22 * DAY, 8 * HOUR],
  ["lea@hollis.studio", "Léa's MacBook Pro", 19 * DAY, 26 * HOUR],
  ["daniel@hollis.studio", "Daniel's MacBook Pro", 15 * DAY, 41 * MIN],
  ["daniel@hollis.studio", "Daniel's Mac mini", 4 * DAY, 9 * HOUR],
  ["hannah@hollis.studio", "Hannah's MacBook Air", 9 * DAY, 2 * HOUR],
  ["ruth@hollis.studio", "Ruth's MacBook Pro", 60 * DAY, 24 * DAY],
  ["grace@hollis.studio", "Grace's MacBook Pro", 3 * DAY, 6 * HOUR],
  ["ilya.petrov@proton.me", "MacBook Air", 1 * DAY, null],
];

let devices: Device[] = deviceSpecs.map(([email, name, created, seen], i) => {
  const u = userByEmail(email);
  return {
    id: `6b1e${(0x3a00 + i * 37).toString(16)}-${(0x9c21 + i * 113).toString(16)}-4f0a-9d2e-${(0x5ab3c0d1e2f + i * 7919).toString(16).slice(-12)}`,
    user_id: u.id,
    user_email: u.email,
    name,
    created_at: ago(created),
    last_seen_at: seen === null ? null : ago(seen),
  };
});

interface CanvasRow extends Omit<Canvas, "member_count" | "share_link_count"> {
  members: string[];
  share_link_count: number;
}

const personalOwners = [
  "ana@hollis.studio",
  "marcus@hollis.studio",
  "priya@hollis.studio",
  "mei@hollis.studio",
  "tomas@hollis.studio",
  "kwame@hollis.studio",
  "daniel@hollis.studio",
  "hannah@hollis.studio",
  "grace@hollis.studio",
];
const shared: [string, string, string[], number, number][] = [
  [
    "Q4 roadmap",
    "ana@hollis.studio",
    [
      "marcus@hollis.studio",
      "priya@hollis.studio",
      "mei@hollis.studio",
      "daniel@hollis.studio",
    ],
    34 * DAY,
    3 * MIN,
  ],
  [
    "Onboarding flow v2",
    "priya@hollis.studio",
    ["tomas@hollis.studio", "hannah@hollis.studio"],
    18 * DAY,
    12 * MIN,
  ],
  [
    "Brand refresh moodboard",
    "mei@hollis.studio",
    ["sofia@hollis.studio", "lea@hollis.studio"],
    25 * DAY,
    5 * HOUR,
  ],
  [
    "Incident 0923 timeline",
    "marcus@hollis.studio",
    ["kwame@hollis.studio", "daniel@hollis.studio"],
    9 * DAY,
    8 * DAY,
  ],
  [
    "Hiring loop: product design",
    "ana@hollis.studio",
    ["mei@hollis.studio"],
    12 * DAY,
    2 * DAY,
  ],
  [
    "Offsite planning",
    "tomas@hollis.studio",
    [
      "ana@hollis.studio",
      "sofia@hollis.studio",
      "kwame@hollis.studio",
      "lea@hollis.studio",
      "grace@hollis.studio",
    ],
    7 * DAY,
    26 * MIN,
  ],
  [
    "Pricing page sketches",
    "sofia@hollis.studio",
    ["jonas.lindqvist@fastmail.com"],
    20 * DAY,
    3 * DAY,
  ],
];

let canvases: CanvasRow[] = [
  ...shared.map(([name, owner, others, created, updated], i) => {
    const u = userByEmail(owner);
    return {
      id: fid("d5", i + 1),
      name,
      kind: "shared" as const,
      owner_id: u.id,
      owner_email: u.email,
      members: [u.id, ...others.map((e) => userByEmail(e).id)],
      share_link_count: i % 3,
      created_at: ago(created),
      updated_at: ago(updated),
    };
  }),
  ...personalOwners.map((email, i) => {
    const u = userByEmail(email);
    const created = Date.parse(u.created_at);
    return {
      id: fid("d5", 100 + i),
      name: "Personal",
      kind: "personal" as const,
      owner_id: u.id,
      owner_email: u.email,
      members: [u.id],
      share_link_count: 0,
      created_at: new Date(created + HOUR).toISOString(),
      updated_at: ago((i * 7 + 2) * HOUR),
    };
  }),
];

type KeyRow = Omit<AccessKey, "status">;

const keySpecs: {
  label: string;
  email?: string;
  created: number;
  by?: string | null;
  expires?: number | null;
  revoked?: number;
  used?: number | null;
  uses: number;
  max?: number;
}[] = [
  {
    label: "Noor Haddad",
    email: "noor@hollis.studio",
    created: 5 * HOUR,
    expires: -(7 * DAY - 5 * HOUR),
    uses: 0,
    max: 1,
  },
  {
    label: "Grace Liu",
    email: "grace@hollis.studio",
    created: 3 * DAY + 2 * HOUR,
    expires: -(4 * DAY),
    used: 6 * HOUR,
    uses: 1,
    max: 1,
  },
  { label: "Studio kiosk", created: 8 * DAY, used: 26 * HOUR, uses: 2 },
  {
    label: "Daniel's Mac mini via pairing",
    email: "daniel@hollis.studio",
    created: 4 * DAY,
    by: null,
    used: 9 * HOUR,
    uses: 1,
  },
  {
    label: "Hannah Okafor",
    email: "hannah@hollis.studio",
    created: 9 * DAY + 3 * HOUR,
    used: 2 * HOUR,
    uses: 1,
    max: 1,
  },
  {
    label: "Design team",
    created: 7 * DAY,
    expires: -(23 * DAY),
    used: 2 * HOUR,
    uses: 3,
    max: 5,
  },
  {
    label: "Ana's Mac mini via pairing",
    email: "ana@hollis.studio",
    created: 6 * DAY,
    by: null,
    used: 3 * MIN,
    uses: 1,
  },
  {
    label: "Weekend hackathon",
    created: 20 * DAY,
    expires: 13 * DAY,
    used: 14 * DAY,
    uses: 4,
  },
  {
    label: "Ruth Abernathy",
    email: "ruth@hollis.studio",
    created: 60 * DAY,
    revoked: 24 * DAY,
    used: 24 * DAY,
    uses: 1,
    max: 1,
  },
  {
    label: "Jonas (contractor)",
    email: "jonas.lindqvist@fastmail.com",
    created: 35 * DAY,
    expires: -(55 * DAY),
    used: 2 * DAY,
    uses: 1,
    max: 1,
  },
  {
    label: "Ana Pereira",
    email: "ana@hollis.studio",
    created: 41 * DAY,
    used: 3 * MIN,
    uses: 1,
    max: 1,
  },
  {
    label: "Old shared key",
    created: 58 * DAY,
    revoked: 30 * DAY,
    used: 31 * DAY,
    uses: 6,
  },
];

let keys: KeyRow[] = keySpecs.map((k, i) => ({
  id: fid("c9", i + 1),
  label: k.label,
  email: k.email ?? null,
  created_at: ago(k.created),
  created_by: k.by === null ? null : admin.email,
  expires_at: k.expires === undefined || k.expires === null ? null : ago(k.expires),
  revoked_at: k.revoked === undefined ? null : ago(k.revoked),
  last_used_at: k.used === undefined || k.used === null ? null : ago(k.used),
  uses: k.uses,
  max_uses: k.max ?? null,
}));

let pairingCodes: PairingCode[] = [
  {
    id: fid("e0", 1),
    user_id: userByEmail("tomas@hollis.studio").id,
    user_email: "tomas@hollis.studio",
    device_name: "Tomás's MacBook Pro",
    created_by_device: devices.find((d) => d.name === "Tomás's MacBook Pro")!.id,
    created_at: ago(3 * MIN),
    expires_at: ahead(7 * MIN),
  },
];

const settings: Omit<Settings, "instance_link_code"> = {
  access_mode: "directory",
  allow_signup: false,
};

let auditSeq = 0;
const audit: AuditEntry[] = [];
function record(
  action: string,
  target: string | null,
  detail: Record<string, unknown> | null,
  at: string,
) {
  audit.unshift({
    id: ++auditSeq,
    admin_id: admin.id,
    admin_email: admin.email,
    action,
    target,
    detail,
    at,
  });
}

// Seed history, oldest first.
const seed: [string, string | null, Record<string, unknown> | null, number][] = [
  ["login", admin.id, null, 62 * DAY],
  [
    "access_key.create",
    fid("c9", 12),
    { label: "Old shared key", email: null },
    58 * DAY,
  ],
  [
    "access_key.create",
    fid("c9", 11),
    { label: "Ana Pereira", email: "ana@hollis.studio" },
    41 * DAY,
  ],
  [
    "access_key.create",
    fid("c9", 10),
    { label: "Jonas (contractor)", email: "jonas.lindqvist@fastmail.com" },
    35 * DAY,
  ],
  ["access_key.revoke", fid("c9", 12), { label: "Old shared key" }, 30 * DAY],
  [
    "user.update",
    userByEmail("ruth@hollis.studio").id,
    { disabled: true, email: "ruth@hollis.studio" },
    24 * DAY,
  ],
  ["access_key.revoke", fid("c9", 9), { label: "Ruth Abernathy" }, 24 * DAY - 2 * MIN],
  ["settings.update", "settings", { access_mode: "directory" }, 22 * DAY],
  [
    "access_key.create",
    fid("c9", 8),
    { label: "Weekend hackathon", email: null },
    20 * DAY,
  ],
  ["canvas.delete", fid("d5", 90), { name: "Scratch (old)" }, 16 * DAY],
  ["login", admin.id, null, 9 * DAY + 4 * HOUR],
  [
    "access_key.create",
    fid("c9", 5),
    { label: "Hannah Okafor", email: "hannah@hollis.studio" },
    9 * DAY + 3 * HOUR,
  ],
  ["access_key.create", fid("c9", 3), { label: "Studio kiosk", email: null }, 8 * DAY],
  ["access_key.create", fid("c9", 6), { label: "Design team", email: null }, 7 * DAY],
  [
    "device.delete",
    "5c0e1f2a-88b1-4c1d-a0f3-9e1d2c3b4a59",
    { user_id: userByEmail("sofia@hollis.studio").id },
    6 * DAY,
  ],
  [
    "user.reset_password",
    userByEmail("kwame@hollis.studio").id,
    { email: "kwame@hollis.studio" },
    5 * DAY,
  ],
  ["logout", admin.id, null, 5 * DAY - 10 * MIN],
  ["login", admin.id, null, 3 * DAY + 3 * HOUR],
  [
    "access_key.create",
    fid("c9", 2),
    { label: "Grace Liu", email: "grace@hollis.studio" },
    3 * DAY + 2 * HOUR,
  ],
  [
    "pairing_code.revoke",
    fid("e0", 7),
    { user_id: userByEmail("daniel@hollis.studio").id },
    4 * DAY,
  ],
  [
    "user.update",
    userByEmail("lea@hollis.studio").id,
    { display_name: "Léa Martin", email: "lea@hollis.studio" },
    2 * DAY,
  ],
  ["password.change", admin.id, null, 2 * DAY - 5 * MIN],
  ["login", admin.id, null, 14 * MIN],
  [
    "access_key.create",
    fid("c9", 1),
    { label: "Noor Haddad", email: "noor@hollis.studio" },
    5 * HOUR,
  ],
];
seed
  .sort((a, b) => b[3] - a[3])
  .forEach(([action, target, detail, when]) => record(action, target, detail, ago(when)));

// ---------------------------------------------------------------------------

function keyStatus(k: KeyRow): AccessKeyStatus {
  if (k.revoked_at) return "revoked";
  if (k.expires_at && Date.parse(k.expires_at) <= Date.now()) return "expired";
  if (k.max_uses !== null && k.uses >= k.max_uses) return "exhausted";
  return "active";
}
const toKey = (k: KeyRow): AccessKey => ({ ...k, status: keyStatus(k) });

function toUser(u: UserRow): User {
  const { password: _password, ...rest } = u;
  return {
    ...rest,
    device_count: devices.filter((d) => d.user_id === u.id).length,
    canvas_count: canvases.filter((c) => c.members.includes(u.id)).length,
  };
}
function toCanvas(c: CanvasRow): Canvas {
  const { members, ...rest } = c;
  return { ...rest, member_count: members.length };
}

function overview(): Overview {
  return {
    version: "0.3.0 (3f9c2ab)",
    uptime_s: Math.floor((Date.now() - BOOTED_AT) / 1000),
    access_mode: settings.access_mode,
    allow_signup: settings.allow_signup,
    counts: {
      users: users.length,
      devices: devices.length,
      canvases: canvases.length,
      access_keys: keys.filter((k) => keyStatus(k) === "active").length,
      live_rooms: 2,
      live_peers: 3,
    },
    public_url: HOST,
    tls: { mode: "self-signed", fingerprint: FINGERPRINT },
  };
}

function currentSettings(): Settings {
  return {
    ...settings,
    instance_link_code: `copper-cloud://${HOST}/#k=${INSTANCE_KEY}&fp=${FINGERPRINT}`,
  };
}

function base64url(bytes: Uint8Array): string {
  let s = "";
  for (const b of bytes) s += String.fromCharCode(b);
  return btoa(s).replace(/\+/g, "-").replace(/\//g, "_").replace(/=+$/, "");
}

// ---------------------------------------------------------------------------

const SIGNED_OUT = "copper-cloud-portal:mock-signed-out";
const signedIn = () =>
  typeof sessionStorage === "undefined" || sessionStorage.getItem(SIGNED_OUT) !== "1";
const setSignedIn = (v: boolean) => {
  if (typeof sessionStorage === "undefined") return;
  if (v) sessionStorage.removeItem(SIGNED_OUT);
  else sessionStorage.setItem(SIGNED_OUT, "1");
};
let failedLogins = 0;

class HttpError extends Error {
  constructor(
    readonly status: number,
    readonly code: string,
    message: string,
  ) {
    super(message);
  }
}
const bad = (message: string) => new HttpError(400, "bad_request", message);
const notFound = () => new HttpError(404, "not_found", "not found");

function paginate<T>(rows: T[], params: URLSearchParams): Page<T> {
  const limit = Math.min(500, Math.max(1, Number(params.get("limit") ?? 100) || 100));
  const offset = Math.max(0, Number(params.get("offset") ?? 0) || 0);
  return { items: rows.slice(offset, offset + limit), total: rows.length, limit, offset };
}
const newestFirst = <T extends { created_at: string }>(rows: T[]) =>
  [...rows].sort((a, b) => Date.parse(b.created_at) - Date.parse(a.created_at));

type Body = Record<string, unknown>;

function route(
  method: string,
  path: string,
  params: URLSearchParams,
  body: Body,
): unknown {
  const nowIso = new Date().toISOString();
  const seg = path.split("/").filter(Boolean);

  if (method === "POST" && path === "login") {
    const email = String(body.email ?? "").trim();
    const password = String(body.password ?? "");
    if (failedLogins >= 5) throw new HttpError(429, "rate_limited", "too many requests");
    if (!email || !password) throw bad("email and password are required");
    if (password === "wrong") {
      failedLogins += 1;
      throw new HttpError(401, "credentials", "invalid credentials");
    }
    failedLogins = 0;
    setSignedIn(true);
    admin.last_login_at = nowIso;
    record("login", admin.id, null, nowIso);
    return { admin };
  }
  if (method === "POST" && path === "logout") {
    if (signedIn()) record("logout", admin.id, null, nowIso);
    setSignedIn(false);
    return { ok: true };
  }
  if (!signedIn()) throw new HttpError(401, "admin_session", "not signed in");

  if (method === "GET" && path === "me") {
    return {
      admin,
      session: { created_at: admin.last_login_at, expires_at: ahead(7 * DAY - 14 * MIN) },
    };
  }
  if (method === "POST" && path === "password") {
    if (String(body.old ?? "") === "wrong")
      throw new HttpError(401, "credentials", "invalid credentials");
    if (String(body.new ?? "").length < 10)
      throw bad("new password must be at least 10 characters");
    record("password.change", admin.id, null, nowIso);
    return { ok: true, revoked_sessions: 1 };
  }
  if (method === "GET" && path === "overview") return overview();
  if (path === "settings") {
    if (method === "PATCH") {
      const changed: Record<string, unknown> = {};
      if (body.access_mode !== undefined) {
        if (body.access_mode !== "open" && body.access_mode !== "directory")
          throw bad('access_mode must be "open" or "directory"');
        settings.access_mode = body.access_mode;
        changed.access_mode = body.access_mode;
      }
      if (body.allow_signup !== undefined) {
        settings.allow_signup = Boolean(body.allow_signup);
        changed.allow_signup = settings.allow_signup;
      }
      record("settings.update", "settings", changed, nowIso);
    }
    return currentSettings();
  }

  if (seg[0] === "access-keys") {
    if (method === "GET" && seg.length === 1) {
      const status = params.get("status");
      const rows = newestFirst(keys).map(toKey);
      return paginate(status ? rows.filter((k) => k.status === status) : rows, params);
    }
    if (method === "POST" && seg.length === 1) {
      const label = String(body.label ?? "").trim();
      if (!label || label.length > 200) throw bad("label must be 1-200 characters");
      const email = body.email ? String(body.email).trim() : null;
      if (email && !/^[^@\s]+@[^@\s]+\.[^@\s]+$/.test(email))
        throw bad("email is not valid");
      const days = body.expires_in_days == null ? null : Number(body.expires_in_days);
      const max = body.max_uses == null ? null : Number(body.max_uses);
      const secret = `ck_${base64url(crypto.getRandomValues(new Uint8Array(32)))}`;
      const row: KeyRow = {
        id: crypto.randomUUID(),
        label,
        email,
        created_at: nowIso,
        created_by: admin.email,
        expires_at: days ? new Date(Date.now() + days * DAY).toISOString() : null,
        revoked_at: null,
        last_used_at: null,
        uses: 0,
        max_uses: max,
      };
      keys = [row, ...keys];
      record("access_key.create", row.id, { label, email }, nowIso);
      const created: CreatedAccessKey = {
        ...toKey(row),
        key: secret,
        link_code: `copper-cloud://${HOST}/#k=${secret}&fp=${FINGERPRINT}`,
      };
      return created;
    }
    if (method === "DELETE" && seg.length === 2) {
      const row = keys.find((k) => k.id === seg[1]);
      if (!row) throw notFound();
      if (!row.revoked_at) {
        row.revoked_at = nowIso;
        record("access_key.revoke", row.id, { label: row.label }, nowIso);
      }
      return toKey(row);
    }
  }

  if (seg[0] === "users") {
    if (method === "GET" && seg.length === 1) {
      const q = (params.get("q") ?? "").toLowerCase();
      const rows = newestFirst(users)
        .filter(
          (u) =>
            !q || u.email.includes(q) || (u.display_name ?? "").toLowerCase().includes(q),
        )
        .map(toUser);
      return paginate(rows, params);
    }
    const user = users.find((u) => u.id === seg[1]);
    if (!user) throw notFound();
    if (method === "PATCH" && seg.length === 2) {
      const changed: Record<string, unknown> = { email: user.email };
      if (body.disabled !== undefined) {
        user.disabled = Boolean(body.disabled);
        changed.disabled = user.disabled;
        if (user.disabled) {
          changed.revoked_sessions = devices.filter((d) => d.user_id === user.id).length;
        }
      }
      if (body.display_name !== undefined) {
        user.display_name = String(body.display_name);
        changed.display_name = user.display_name;
      }
      record("user.update", user.id, changed, nowIso);
      return toUser(user);
    }
    if (method === "DELETE" && seg.length === 2) {
      users.splice(users.indexOf(user), 1);
      devices = devices.filter((d) => d.user_id !== user.id);
      canvases = canvases
        .filter((c) => c.owner_id !== user.id)
        .map((c) => ({ ...c, members: c.members.filter((m) => m !== user.id) }));
      keys = keys.filter((k) => k.email !== user.email);
      pairingCodes = pairingCodes.filter((p) => p.user_id !== user.id);
      record("user.delete", user.id, { email: user.email }, nowIso);
      return { ok: true };
    }
    if (method === "POST" && seg[2] === "reset-password") {
      const password = String(body.password ?? "");
      if (password.length < 10) throw bad("password must be at least 10 characters");
      user.password = password;
      record("user.reset_password", user.id, { email: user.email }, nowIso);
      return {
        ok: true,
        revoked_sessions: devices.filter((d) => d.user_id === user.id).length,
      };
    }
  }

  if (seg[0] === "devices") {
    if (method === "GET" && seg.length === 1) {
      const userId = params.get("user_id");
      const rows = newestFirst(devices).filter((d) => !userId || d.user_id === userId);
      return paginate(rows, params);
    }
    if (method === "DELETE" && seg.length === 2) {
      const userId = params.get("user_id");
      const before = devices.length;
      devices = devices.filter(
        (d) => !(d.id === seg[1] && (!userId || d.user_id === userId)),
      );
      const deleted = before - devices.length;
      if (!deleted) throw notFound();
      record("device.delete", seg[1]!, { user_id: userId }, nowIso);
      return { ok: true, deleted };
    }
  }

  if (seg[0] === "canvases") {
    if (method === "GET" && seg.length === 1) {
      const kind = params.get("kind");
      const rows = [...canvases]
        .sort((a, b) => Date.parse(b.updated_at) - Date.parse(a.updated_at))
        .filter((c) => !kind || c.kind === kind)
        .map(toCanvas);
      return paginate(rows, params);
    }
    if (method === "DELETE" && seg.length === 2) {
      const row = canvases.find((c) => c.id === seg[1]);
      if (!row) throw notFound();
      canvases = canvases.filter((c) => c !== row);
      record("canvas.delete", row.id, { name: row.name }, nowIso);
      return { ok: true };
    }
  }

  if (seg[0] === "pairing-codes") {
    if (method === "GET" && seg.length === 1) {
      const userId = params.get("user_id");
      const live = pairingCodes.filter(
        (p) => Date.parse(p.expires_at) > Date.now() && (!userId || p.user_id === userId),
      );
      return paginate(newestFirst(live), params);
    }
    if (method === "DELETE" && seg.length === 2) {
      const row = pairingCodes.find((p) => p.id === seg[1]);
      if (!row) throw notFound();
      pairingCodes = pairingCodes.filter((p) => p !== row);
      record("pairing_code.revoke", row.id, { user_id: row.user_id }, nowIso);
      return { ok: true };
    }
  }

  if (method === "GET" && path === "audit") return paginate(audit, params);

  throw notFound();
}

const json = (status: number, body: unknown, headers: Record<string, string> = {}) =>
  new Response(JSON.stringify(body), {
    status,
    headers: { "Content-Type": "application/json", ...headers },
  });

/** `fetch`-compatible handler for `/admin/api/*` URLs. */
export async function mockFetch(url: string, init: RequestInit = {}): Promise<Response> {
  const parsed = new URL(url, "http://mock.local");
  const path = parsed.pathname.replace(/^.*\/admin\/api\//, "").replace(/\/+$/, "");
  const method = (init.method ?? "GET").toUpperCase();
  await new Promise((resolve) => setTimeout(resolve, 160 + Math.random() * 260));

  const headers = new Headers(init.headers);
  if (method !== "GET" && headers.get("X-Requested-With") !== "copper-cloud-portal") {
    return json(403, { error: "csrf", message: "forbidden" });
  }
  let body: Body = {};
  if (typeof init.body === "string" && init.body) {
    try {
      body = JSON.parse(init.body) as Body;
    } catch {
      return json(400, { error: "bad_request", message: "invalid JSON" });
    }
  }
  try {
    const result = route(method, path, parsed.searchParams, body);
    return json(method === "POST" && path === "access-keys" ? 201 : 200, result);
  } catch (error) {
    if (error instanceof HttpError) {
      return json(
        error.status,
        { error: error.code, message: error.message },
        error.status === 429 ? { "Retry-After": "60" } : {},
      );
    }
    return json(500, { error: "internal", message: String(error) });
  }
}
