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
  Integration,
  IntegrationDevice,
  IntelligenceSettings,
  Overview,
  Page,
  PairingCode,
  Settings,
  SsoProvider,
  SsoSettings,
  SsoTestCheck,
  User,
} from "./types";
import type {
  Addon,
  AddonAudience,
  AddonManifest,
  AddonSlot,
  AddonVersion,
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

const INTEL_DEFAULTS = {
  jev_endpoint: "https://api.typesafe.ai/v1/systemone",
  jev_model: "jev-latest",
  agent_max_turns: 60,
};
const AGENT_MAX_TURNS = { min: 1, max: 500 };
const intelligence: IntelligenceSettings = {
  enabled: true,
  jev: { key_last4: "x7Qa", endpoint: INTEL_DEFAULTS.jev_endpoint, model: "jev-latest" },
  router: null,
  agent: null,
  updated_at: ago(2 * DAY),
  updated_by: "cli",
  defaults: INTEL_DEFAULTS,
};
const last4 = (k: string) => (k.length < 12 ? "" : k.slice(-4));

// Single sign-on + agent access (0.9.0). SSO starts unconfigured so the
// walkthrough shows; a client secret containing "wrong" fails the test.
interface SsoRow extends Omit<SsoProvider, "has_secret"> {
  secret: string | null;
}
const sso: { password_signin: boolean; providers: SsoRow[] } = {
  password_signin: true,
  providers: [],
};
const GOOGLE_SUFFIX = ".apps.googleusercontent.com";

interface IntegrationRow extends Omit<Integration, "device_count"> {
  devices: IntegrationDevice[];
}
const agentDevice = (email: string, name: string, n: number, seen: number | null) => ({
  user_email: email,
  device_id: `7d3f0c1e-9a2b-5c4d-8e6f-${n.toString(16).padStart(12, "0")}`,
  name,
  last_seen_at: seen === null ? null : ago(seen),
});
let integrations: IntegrationRow[] = [
  {
    id: fid("f1", 1),
    name: "Team bots",
    domains: [],
    create_users: true,
    created_at: ago(6 * DAY),
    last_used_at: ago(22 * MIN),
    revoked_at: null,
    devices: [
      agentDevice("ana@hollis.studio", "Bot · Vera", 1, 22 * MIN),
      agentDevice("marcus@hollis.studio", "Bot · Atlas", 2, 3 * HOUR),
      agentDevice("priya@hollis.studio", "Bot · Juniper", 3, 2 * DAY),
    ],
  },
  {
    id: fid("f1", 2),
    name: "Staging bots (old)",
    domains: ["hollis.studio"],
    create_users: false,
    created_at: ago(30 * DAY),
    last_used_at: ago(12 * DAY),
    revoked_at: ago(9 * DAY),
    devices: [agentDevice("tomas@hollis.studio", "Bot · Test", 4, 12 * DAY)],
  },
];
const toIntegration = ({ devices: d, ...row }: IntegrationRow): Integration => ({
  ...row,
  device_count: d.length,
});

function ssoView(): SsoSettings {
  const first = sso.providers.find((p) => p.enabled);
  return {
    enabled: !!first,
    password_signin: sso.password_signin,
    providers: sso.providers.map(({ secret, ...p }) => ({ ...p, has_secret: !!secret })),
    join_link: first ? `copper-cloud://${HOST}/#sso=${first.id}&fp=${FINGERPRINT}` : null,
    start_url: first ? `https://${HOST.replace(/:443$/, "")}/start` : null,
  };
}

function ssoTest(p: SsoRow): SsoTestCheck[] {
  const checks: SsoTestCheck[] = [];
  if (p.kind === "oidc") {
    const ok = /^https:\/\/[^\s/]+/.test(p.issuer ?? "");
    checks.push({
      name: "Discovery",
      ok,
      detail: ok
        ? `Read ${p.issuer}/.well-known/openid-configuration.`
        : "The issuer URL isn't https.",
    });
  } else {
    const ok = p.client_id.endsWith(GOOGLE_SUFFIX);
    checks.push({
      name: "Client ID format",
      ok,
      detail: ok
        ? "Looks like a Google OAuth client ID."
        : `Doesn't end in ${GOOGLE_SUFFIX}.`,
    });
  }
  const wrong = !p.secret || p.secret.includes("wrong");
  checks.push({
    name: "Token endpoint",
    ok: !wrong,
    detail: wrong
      ? "The provider answered invalid_client: the OAuth client was not found or the secret doesn't match."
      : "The provider answered invalid_grant for a dummy code, so the ID and secret are accepted.",
  });
  checks.push({
    name: "Allowed domains",
    ok: p.domains.length > 0,
    detail: p.domains.length ? p.domains.join(", ") : "No domains: nobody can sign in.",
  });
  return checks;
}

// Addons ------------------------------------------------------------------

// `?mock=empty` starts with no addons; `?mock=nobody` has one that is on with
// nobody selected (to see the warning).
const MOCK_VARIANT =
  typeof window === "undefined"
    ? null
    : new URLSearchParams(window.location.search).get("mock");

const ADDON_SLOTS: AddonSlot[] = [
  {
    id: "agent-sidebar",
    name: "Agent sidebar",
    description: "Replaces the ⌘E pane in Copper.",
  },
];
const MAX_ADDON_BYTES = 16 * 1024 * 1024;

interface AddonVersionRow extends AddonVersion {
  manifest: AddonManifest;
}
interface AddonRow {
  id: string;
  slot: string;
  name: string;
  enabled: boolean;
  audience: AddonAudience;
  people: string[];
  settings: Record<string, unknown>;
  priority: number;
  revision: number;
  current: string;
  versions: AddonVersionRow[];
  created_at: string;
  updated_at: string;
}

const researchManifest = (version: string): AddonManifest => ({
  schema: "copper-addon/1",
  id: "hollis.research-sidebar",
  name: "Research sidebar",
  version,
  slot: "agent-sidebar",
  entry: "index.html",
  description:
    "Ask about the page you're on and save what you find to the studio's notes.",
  author: "Hollis Studio",
  bridge: 1,
  permissions: {
    hosts: ["research.example.com"],
    capabilities: ["identity", "storage", "fetch", "page"],
  },
  settings: {
    apiUrl: {
      type: "string",
      default: "https://research.example.com/api",
      title: "Research API URL",
    },
    notebook: {
      type: "string",
      default: "",
      title: "Notebook for saved findings",
      description: "Leave blank to ask each person the first time.",
    },
    maxSources: { type: "number", default: 8, title: "Sources per answer" },
    showCitations: {
      type: "boolean",
      default: true,
      title: "Show citations",
      description: "Link each claim to the page it came from.",
    },
    blockedSites: {
      type: "array",
      default: [],
      title: "Sites it never reads",
      description: "A JSON list of hostnames.",
    },
  },
  platforms: ["mac", "linux", "windows"],
});

const fakeSha = (n: number) =>
  Array.from({ length: 64 }, (_, i) => ((n * 7 + i * 13) % 16).toString(16)).join("");

let addonRows: AddonRow[] =
  MOCK_VARIANT === "empty"
    ? []
    : [
        {
          id: "hollis.research-sidebar",
          slot: "agent-sidebar",
          name: "Research sidebar",
          enabled: true,
          audience: "people",
          people:
            MOCK_VARIANT === "nobody"
              ? []
              : ["ana@hollis.studio", "priya@hollis.studio", "noor@hollis.studio"],
          settings: { notebook: "Studio research", maxSources: 5 },
          priority: 0,
          revision: 7,
          current: "1.2.0",
          versions: [
            ["1.2.0", 2 * DAY, 48_213, admin.email],
            ["1.1.0", 9 * DAY, 46_870, admin.email],
            ["1.0.0", 16 * DAY, 41_092, null],
          ].map(([version, when, size, by], i) => ({
            version: version as string,
            sha256: fakeSha(i + 3),
            size_bytes: size as number,
            created_at: ago(when as number),
            uploaded_by: by as string | null,
            manifest: researchManifest(version as string),
          })),
          created_at: ago(16 * DAY),
          updated_at: ago(2 * DAY - 20 * MIN),
        },
      ];

const addonSeed: [string, string | null, Record<string, unknown> | null, number][] =
  MOCK_VARIANT === "empty"
    ? []
    : [
        [
          "addon.create",
          "hollis.research-sidebar",
          { id: "hollis.research-sidebar", name: "Research sidebar", version: "1.0.0" },
          16 * DAY,
        ],
        [
          "addon.upload",
          "hollis.research-sidebar",
          { id: "hollis.research-sidebar", name: "Research sidebar", version: "1.1.0" },
          9 * DAY,
        ],
        [
          "addon.upload",
          "hollis.research-sidebar",
          { id: "hollis.research-sidebar", name: "Research sidebar", version: "1.2.0" },
          2 * DAY,
        ],
        [
          "addon.update",
          "hollis.research-sidebar",
          { id: "hollis.research-sidebar", name: "Research sidebar", enabled: true },
          2 * DAY - 20 * MIN,
        ],
      ];

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
seed.push(...addonSeed);
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

// Addons ------------------------------------------------------------------

function toAddon(row: AddonRow): Addon {
  const versions = [...row.versions].sort(
    (a, b) => Date.parse(b.created_at) - Date.parse(a.created_at),
  );
  const current = versions.find((v) => v.version === row.current) ?? null;
  const strip = ({ manifest: _m, ...v }: AddonVersionRow): AddonVersion => v;
  return {
    id: row.id,
    slot: row.slot,
    name: row.name,
    enabled: row.enabled,
    audience: row.audience,
    people: row.people.map((email) => {
      const user = users.find((u) => u.email === email);
      return { email, name: user?.display_name ?? null, user_id: user?.id ?? null };
    }),
    settings: { ...row.settings },
    priority: row.priority,
    revision: row.revision,
    current: current ? strip(current) : null,
    versions: versions.map(strip),
    manifest: current?.manifest ?? null,
    created_at: row.created_at,
    updated_at: row.updated_at,
  };
}

const SETTING_TYPES = ["string", "number", "boolean", "object", "array"];

function settingTypeOk(type: string, v: unknown): boolean {
  switch (type) {
    case "string":
      return typeof v === "string";
    case "number":
      return typeof v === "number" && Number.isFinite(v);
    case "boolean":
      return typeof v === "boolean";
    case "array":
      return Array.isArray(v);
    case "object":
      return typeof v === "object" && v !== null && !Array.isArray(v);
    default:
      return false;
  }
}

/** The §2 manifest checks the server runs, enough to exercise the error paths. */
function validateManifest(raw: unknown, files: string[], slot: string): AddonManifest {
  if (typeof raw !== "object" || raw === null || Array.isArray(raw))
    throw bad("manifest.json must be a JSON object");
  const m = raw as Record<string, unknown>;
  if (m.schema !== "copper-addon/1")
    throw bad('manifest schema must be "copper-addon/1"');
  const id = typeof m.id === "string" ? m.id : "";
  if (!/^[a-z0-9][a-z0-9.-]{2,63}$/.test(id) || !id.includes(".") || id.includes(".."))
    throw bad(
      "manifest id must look like vendor.name: lowercase letters, digits, dots and dashes",
    );
  const name = typeof m.name === "string" ? m.name.trim() : "";
  if (!name || name.length > 60) throw bad("manifest name must be 1-60 characters");
  const version = typeof m.version === "string" ? m.version : "";
  if (!/^\d+\.\d+\.\d+(-[0-9A-Za-z.-]+)?$/.test(version))
    throw bad(`manifest version "${version}" is not MAJOR.MINOR.PATCH`);
  if (!ADDON_SLOTS.some((s) => s.id === m.slot))
    throw bad(`manifest slot "${String(m.slot)}" is not a slot this server knows`);
  if (m.slot !== slot)
    throw bad(`manifest slot is "${String(m.slot)}", but you chose "${slot}"`);
  const entry = typeof m.entry === "string" ? m.entry : "";
  if (!entry.endsWith(".html")) throw bad("manifest entry must be an .html file");
  if (!files.includes(entry)) throw bad(`entry "${entry}" is not in the bundle`);
  if (typeof m.bridge !== "number" || !Number.isInteger(m.bridge) || m.bridge < 1)
    throw bad("manifest bridge must be a whole number of 1 or more");
  if (typeof m.description === "string" && m.description.length > 280)
    throw bad("manifest description must be at most 280 characters");
  if (m.settings !== undefined) {
    if (
      typeof m.settings !== "object" ||
      m.settings === null ||
      Array.isArray(m.settings)
    )
      throw bad("manifest settings must be an object");
    for (const [key, spec] of Object.entries(m.settings as Record<string, unknown>)) {
      const t = (spec as { type?: unknown } | null)?.type;
      if (typeof t !== "string" || !SETTING_TYPES.includes(t))
        throw bad(`setting "${key}" needs a type: ${SETTING_TYPES.join(", ")}`);
    }
  }
  return { ...(m as AddonManifest), name };
}

// A tiny zip reader (central directory + stored/deflate entries), mock only.
interface ZipEntry {
  name: string;
  method: number;
  compressed: number;
  size: number;
  offset: number;
}

function zipEntries(bytes: Uint8Array): ZipEntry[] {
  const view = new DataView(bytes.buffer, bytes.byteOffset, bytes.byteLength);
  let eocd = -1;
  for (let i = bytes.length - 22; i >= Math.max(0, bytes.length - 22 - 0xffff); i--) {
    if (view.getUint32(i, true) === 0x06054b50) {
      eocd = i;
      break;
    }
  }
  if (eocd < 0) throw bad("the file is not a zip archive");
  const count = view.getUint16(eocd + 10, true);
  let p = view.getUint32(eocd + 16, true);
  const decoder = new TextDecoder();
  const entries: ZipEntry[] = [];
  for (let n = 0; n < count; n++) {
    if (p + 46 > bytes.length || view.getUint32(p, true) !== 0x02014b50)
      throw bad("the zip archive is damaged");
    const nameLength = view.getUint16(p + 28, true);
    const extra = view.getUint16(p + 30, true);
    const comment = view.getUint16(p + 32, true);
    entries.push({
      name: decoder.decode(bytes.subarray(p + 46, p + 46 + nameLength)),
      method: view.getUint16(p + 10, true),
      compressed: view.getUint32(p + 20, true),
      size: view.getUint32(p + 24, true),
      offset: view.getUint32(p + 42, true),
    });
    p += 46 + nameLength + extra + comment;
  }
  return entries;
}

async function zipRead(bytes: Uint8Array, entry: ZipEntry): Promise<Uint8Array> {
  const view = new DataView(bytes.buffer, bytes.byteOffset, bytes.byteLength);
  if (view.getUint32(entry.offset, true) !== 0x04034b50)
    throw bad("the zip archive is damaged");
  const start =
    entry.offset +
    30 +
    view.getUint16(entry.offset + 26, true) +
    view.getUint16(entry.offset + 28, true);
  const data = bytes.slice(start, start + entry.compressed);
  if (entry.method === 0) return data;
  if (entry.method !== 8)
    throw bad(`zip entry "${entry.name}" uses an unsupported method`);
  const stream = new Blob([data])
    .stream()
    .pipeThrough(new DecompressionStream("deflate-raw"));
  return new Uint8Array(await new Response(stream).arrayBuffer());
}

async function sha256Hex(bytes: Uint8Array<ArrayBuffer>): Promise<string> {
  const hex = (buf: ArrayBuffer | Uint8Array) =>
    Array.from(new Uint8Array(buf), (b) => b.toString(16).padStart(2, "0")).join("");
  if (typeof crypto !== "undefined" && crypto.subtle) {
    return hex(await crypto.subtle.digest("SHA-256", bytes));
  }
  return hex(crypto.getRandomValues(new Uint8Array(32)));
}

/** `POST addons?slot=…` with a raw `.copperaddon` body. */
async function uploadAddon(params: URLSearchParams, body: unknown): Promise<unknown> {
  if (!signedIn()) throw new HttpError(401, "admin_session", "not signed in");
  const slot = params.get("slot") ?? "";
  if (!slot) throw bad("the slot query parameter is required");
  if (!ADDON_SLOTS.some((s) => s.id === slot)) throw bad(`unknown slot "${slot}"`);
  if (!(body instanceof Blob)) throw bad("send the addon as a zip body");
  if (body.size > MAX_ADDON_BYTES)
    throw new HttpError(413, "payload_too_large", "the addon is larger than 16 MiB");
  const bytes = new Uint8Array(await body.arrayBuffer());

  const all = zipEntries(bytes).filter(
    (e) =>
      !e.name.endsWith("/") &&
      !e.name.startsWith("__MACOSX/") &&
      !e.name.split("/").includes(".DS_Store"),
  );
  // manifest.json at the root, or inside one top-level folder (stripped).
  const tops = new Set(
    all.map((e) => (e.name.includes("/") ? e.name.split("/")[0] : "")),
  );
  const prefix =
    !all.some((e) => e.name === "manifest.json") && tops.size === 1 && !tops.has("")
      ? `${[...tops][0]}/`
      : "";
  const files = all.map((e) => e.name.slice(prefix.length));
  const manifestEntry = all.find((e) => e.name === `${prefix}manifest.json`);
  if (!manifestEntry) throw bad("manifest.json is missing from the root of the zip");
  if (files.length > 1000) throw bad("the bundle has more than 1000 files");
  let parsed: unknown;
  try {
    parsed = JSON.parse(new TextDecoder().decode(await zipRead(bytes, manifestEntry)));
  } catch (error) {
    if (error instanceof HttpError) throw error;
    throw bad("manifest.json is not valid JSON");
  }
  const manifest = validateManifest(parsed, files, slot);
  const sha256 = await sha256Hex(bytes);
  const size = Math.round(all.reduce((n, e) => n + e.size, 0) * 1.37 + 120);
  const nowIso = new Date().toISOString();
  const version: AddonVersionRow = {
    version: manifest.version,
    sha256,
    size_bytes: size,
    created_at: nowIso,
    uploaded_by: admin.email,
    manifest,
  };

  const row = addonRows.find((r) => r.id === manifest.id);
  if (!row) {
    const created: AddonRow = {
      id: manifest.id,
      slot,
      name: manifest.name,
      enabled: false,
      audience: "people",
      people: [],
      settings: {},
      priority: 0,
      revision: 1,
      current: manifest.version,
      versions: [version],
      created_at: nowIso,
      updated_at: nowIso,
    };
    addonRows = [...addonRows, created];
    record(
      "addon.create",
      created.id,
      { id: created.id, name: created.name, version: manifest.version, slot },
      nowIso,
    );
    return { outcome: "created", addon: toAddon(created) };
  }
  if (row.slot !== slot) throw bad(`"${row.id}" is an addon for the ${row.slot} slot`);
  const same = row.versions.find((v) => v.version === manifest.version);
  if (same) {
    if (same.sha256 === sha256) return { outcome: "unchanged", addon: toAddon(row) };
    throw new HttpError(
      409,
      "version_exists",
      `version ${manifest.version} of ${row.id} already exists with different contents. Raise the version number and upload again`,
    );
  }
  row.versions.push(version);
  row.current = manifest.version;
  row.name = manifest.name;
  // Keep only settings the new manifest still declares.
  const keys = Object.keys(manifest.settings ?? {});
  row.settings = Object.fromEntries(
    Object.entries(row.settings).filter(([k]) => keys.includes(k)),
  );
  row.revision += 1;
  row.updated_at = nowIso;
  record(
    "addon.upload",
    row.id,
    { id: row.id, name: row.name, version: manifest.version },
    nowIso,
  );
  return { outcome: "new_version", addon: toAddon(row) };
}

function addonRoute(method: string, seg: string[], body: Body, nowIso: string): unknown {
  if (method === "GET" && seg.length === 2 && seg[1] === "slots") return ADDON_SLOTS;
  if (method === "GET" && seg.length === 1) return addonRows.map(toAddon);
  const row = addonRows.find((r) => r.id === seg[1]);
  if (!row) throw notFound();
  if (method === "GET" && seg.length === 2) return toAddon(row);
  if (method === "GET" && seg[2] === "versions" && seg[4] === "bundle") {
    const v = row.versions.find((x) => x.version === seg[3]);
    if (!v) throw notFound();
    return { format: "copper-addon-bundle/1", manifest: v.manifest, files: [] };
  }
  if (method === "DELETE" && seg.length === 2) {
    addonRows = addonRows.filter((r) => r !== row);
    record("addon.delete", row.id, { id: row.id, name: row.name }, nowIso);
    return { ok: true };
  }
  if (method !== "PATCH" || seg.length !== 2) throw notFound();

  const changed: Record<string, unknown> = { id: row.id, name: row.name };
  const next = { ...row };
  if (body.enabled !== undefined) {
    if (typeof body.enabled !== "boolean") throw bad("enabled must be true or false");
    next.enabled = body.enabled;
    changed.enabled = body.enabled;
  }
  if (body.audience !== undefined) {
    if (body.audience !== "everyone" && body.audience !== "people")
      throw bad('audience must be "everyone" or "people"');
    next.audience = body.audience;
    changed.audience = body.audience;
  }
  if (body.people !== undefined) {
    if (!Array.isArray(body.people)) throw bad("people must be a list of emails");
    const emails = body.people.map((e) => String(e).trim().toLowerCase());
    const badEmail = emails.find((e) => !/^[^@\s]+@[^@\s]+\.[^@\s]+$/.test(e));
    if (badEmail !== undefined) throw bad(`"${badEmail}" is not an email address`);
    next.people = [...new Set(emails)];
    changed.people = next.people.length;
  }
  if (body.settings !== undefined) {
    const s = body.settings;
    if (typeof s !== "object" || s === null || Array.isArray(s))
      throw bad("settings must be an object");
    if (JSON.stringify(s).length > 16 * 1024) throw bad("settings must be under 16 KiB");
    const manifest = row.versions.find((v) => v.version === row.current)?.manifest;
    const specs = manifest?.settings ?? {};
    for (const [key, value] of Object.entries(s)) {
      const spec = specs[key];
      if (!spec) throw bad(`unknown setting "${key}"`);
      if (!settingTypeOk(spec.type, value))
        throw bad(
          `setting "${key}" must be ${spec.type === "array" || spec.type === "object" ? "an" : "a"} ${spec.type}`,
        );
    }
    next.settings = { ...(s as Record<string, unknown>) };
    changed.settings = Object.keys(s);
  }
  if (body.priority !== undefined) {
    if (typeof body.priority !== "number" || !Number.isInteger(body.priority))
      throw bad("priority must be a whole number");
    next.priority = body.priority;
    changed.priority = body.priority;
  }
  if (body.current_version !== undefined) {
    const v = row.versions.find((x) => x.version === body.current_version);
    if (!v) throw bad(`there is no version "${String(body.current_version)}"`);
    next.current = v.version;
    next.name = v.manifest.name;
    changed.current_version = v.version;
  }
  next.revision = row.revision + 1;
  next.updated_at = nowIso;
  addonRows = addonRows.map((r) => (r === row ? next : r));
  record("addon.update", row.id, changed, nowIso);
  return toAddon(next);
}

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

  if (path === "intelligence") {
    if (method === "DELETE") {
      intelligence.jev = null;
      intelligence.router = null;
      intelligence.enabled = true;
      // The agent budget is not a key: DELETE leaves it (and the row it lives on).
      intelligence.updated_at = intelligence.agent ? nowIso : null;
      intelligence.updated_by = intelligence.agent ? admin.email : null;
      record("intelligence.clear", "intelligence", { via: "admin_api" }, nowIso);
    } else if (method === "PUT") {
      const changed: Record<string, unknown> = { via: "admin_api" };
      if (body.jev === null) {
        intelligence.jev = null;
        changed.jev = null;
      } else if (body.jev) {
        const j = body.jev as { key?: string; endpoint?: string; model?: string };
        if (!j.key && !intelligence.jev) throw bad("jev key is required");
        intelligence.jev = {
          key_last4: j.key ? last4(j.key) : intelligence.jev!.key_last4,
          endpoint:
            j.endpoint || intelligence.jev?.endpoint || INTEL_DEFAULTS.jev_endpoint,
          model: j.model || intelligence.jev?.model || INTEL_DEFAULTS.jev_model,
        };
        changed.jev = { ...intelligence.jev };
      }
      if (body.router === null) {
        intelligence.router = null;
        changed.router = null;
      } else if (body.router) {
        const r = body.router as { key?: string; url?: string };
        if (!r.key && !intelligence.router) throw bad("router key is required");
        const url = r.url?.trim() || intelligence.router?.url;
        if (!url) throw bad("router url is required");
        intelligence.router = {
          key_last4: r.key ? last4(r.key) : intelligence.router!.key_last4,
          url,
        };
        changed.router = { ...intelligence.router };
      }
      if (body.agent === null) {
        intelligence.agent = null;
        changed.agent = null;
      } else if (body.agent !== undefined) {
        const n = (body.agent as { max_turns?: unknown }).max_turns;
        if (
          typeof n !== "number" ||
          !Number.isInteger(n) ||
          n < AGENT_MAX_TURNS.min ||
          n > AGENT_MAX_TURNS.max
        ) {
          throw bad(
            `agent max_turns must be an integer from ${AGENT_MAX_TURNS.min} to ${AGENT_MAX_TURNS.max}`,
          );
        }
        intelligence.agent = { max_turns: n };
        changed.agent = { max_turns: n };
      }
      if (typeof body.enabled === "boolean") {
        intelligence.enabled = body.enabled;
        changed.enabled = body.enabled;
      }
      intelligence.updated_at = nowIso;
      intelligence.updated_by = admin.email;
      record("intelligence.update", "intelligence", changed, nowIso);
    }
    return { ...intelligence };
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

  if (seg[0] === "sso") {
    if (method === "GET" && seg.length === 1) return ssoView();
    if (method === "PATCH" && seg.length === 1) {
      if (typeof body.password_signin !== "boolean")
        throw bad("password_signin is required");
      sso.password_signin = body.password_signin;
      record(
        "sso.password_signin",
        "sso",
        { password_signin: sso.password_signin },
        nowIso,
      );
      return ssoView();
    }
    if (seg[1] === "providers" && seg[2]) {
      const pid = seg[2];
      const row = sso.providers.find((p) => p.id === pid);
      if (method === "PUT" && seg.length === 3) {
        const kind = body.kind === "oidc" ? "oidc" : "google";
        const clientId = String(body.client_id ?? "").trim();
        if (!clientId) throw bad("client_id is required");
        const domains = Array.isArray(body.domains) ? body.domains.map(String) : [];
        if (!domains.length) throw bad("at least one domain is required");
        const secret = body.client_secret
          ? String(body.client_secret)
          : (row?.secret ?? null);
        if (!secret) throw bad("client_secret is required");
        const issuer = kind === "oidc" ? String(body.issuer ?? "").trim() : null;
        if (kind === "oidc" && !issuer) throw bad("issuer is required");
        const next: SsoRow = {
          id: pid,
          kind,
          name: String(body.name ?? "").trim() || (kind === "google" ? "Google" : pid),
          client_id: clientId,
          secret,
          issuer,
          domains,
          auto_create: body.auto_create !== false,
          enabled: body.enabled !== false,
          updated_at: nowIso,
        };
        sso.providers = row
          ? sso.providers.map((p) => (p.id === pid ? next : p))
          : [...sso.providers, next];
        record(
          "sso.provider.put",
          pid,
          { kind, name: next.name, domains, enabled: next.enabled, via: "admin_api" },
          nowIso,
        );
        return ssoView();
      }
      if (!row) throw notFound();
      if (method === "DELETE" && seg.length === 3) {
        sso.providers = sso.providers.filter((p) => p !== row);
        record("sso.provider.delete", pid, { via: "admin_api" }, nowIso);
        return { ok: true };
      }
      if (method === "POST" && seg[3] === "test") {
        const checks = ssoTest(row);
        return { ok: checks.every((c) => c.ok), checks };
      }
    }
  }

  if (seg[0] === "integrations") {
    if (method === "GET" && seg.length === 1) {
      return newestFirst(integrations).map(toIntegration);
    }
    if (method === "POST" && seg.length === 1) {
      const name = String(body.name ?? "").trim();
      if (!name || name.length > 200) throw bad("name must be 1-200 characters");
      const row: IntegrationRow = {
        id: crypto.randomUUID(),
        name,
        domains: Array.isArray(body.domains) ? body.domains.map(String) : [],
        create_users: body.create_users === true,
        created_at: nowIso,
        last_used_at: null,
        revoked_at: null,
        devices: [],
      };
      integrations = [row, ...integrations];
      record("integration.create", row.id, { name }, nowIso);
      const key = `cai_${base64url(crypto.getRandomValues(new Uint8Array(32)))}`;
      return { integration: toIntegration(row), key };
    }
    const row = integrations.find((i) => i.id === seg[1]);
    if (!row) throw notFound();
    if (method === "POST" && seg[2] === "rotate") {
      if (row.revoked_at) throw bad("this integration is revoked; create a new one");
      record("integration.rotate", row.id, { name: row.name }, nowIso);
      return { key: `cai_${base64url(crypto.getRandomValues(new Uint8Array(32)))}` };
    }
    if (method === "DELETE" && seg.length === 2) {
      if (!row.revoked_at) {
        // Like the server: revoking signs out every Copper it paired (devices stay listed).
        row.revoked_at = nowIso;
        record(
          "integration.revoke",
          row.id,
          { name: row.name, revoked_sessions: row.devices.length },
          nowIso,
        );
      }
      return toIntegration(row);
    }
    if (method === "GET" && seg[2] === "devices") return row.devices;
  }

  if (seg[0] === "addons") return addonRoute(method, seg, body, nowIso);

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
  if (method === "POST" && path === "addons") {
    try {
      return json(201, await uploadAddon(parsed.searchParams, init.body));
    } catch (error) {
      if (error instanceof HttpError) {
        return json(error.status, { error: error.code, message: error.message });
      }
      return json(500, { error: "internal", message: String(error) });
    }
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
