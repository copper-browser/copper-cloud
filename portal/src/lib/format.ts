const MINUTE = 60_000;
const HOUR = 60 * MINUTE;
const DAY = 24 * HOUR;

const shortDate = new Intl.DateTimeFormat(undefined, { month: "short", day: "numeric" });
const shortDateYear = new Intl.DateTimeFormat(undefined, {
  month: "short",
  day: "numeric",
  year: "numeric",
});
const fullStamp = new Intl.DateTimeFormat(undefined, {
  weekday: "short",
  year: "numeric",
  month: "short",
  day: "numeric",
  hour: "numeric",
  minute: "2-digit",
  second: "2-digit",
  timeZoneName: "short",
});

/** Compact relative time: "just now", "4m ago", "in 3d", "Mar 4". */
export function relativeTime(iso: string, now: number = Date.now()): string {
  const t = Date.parse(iso);
  if (Number.isNaN(t)) return "—";
  const diff = t - now;
  const abs = Math.abs(diff);
  const future = diff > 0;
  const wrap = (s: string) => (future ? `in ${s}` : `${s} ago`);

  if (abs < 45_000) return future ? "in a moment" : "just now";
  if (abs < HOUR) return wrap(`${Math.max(1, Math.round(abs / MINUTE))}m`);
  if (abs < DAY) return wrap(`${Math.round(abs / HOUR)}h`);
  if (abs < 30 * DAY) return wrap(`${Math.round(abs / DAY)}d`);
  const d = new Date(t);
  return d.getFullYear() === new Date(now).getFullYear()
    ? shortDate.format(d)
    : shortDateYear.format(d);
}

/** Full local timestamp for tooltips, e.g. "Wed, Oct 1, 2026, 6:04:05 PM PDT". */
export function fullTimestamp(iso: string): string {
  const t = Date.parse(iso);
  return Number.isNaN(t) ? iso : fullStamp.format(new Date(t));
}

/** "3d 4h", "5h 12m", "48s". */
export function formatUptime(seconds: number): string {
  const s = Math.max(0, Math.floor(seconds));
  const d = Math.floor(s / 86_400);
  const h = Math.floor((s % 86_400) / 3_600);
  const m = Math.floor((s % 3_600) / 60);
  if (d > 0) return h ? `${d}d ${h}h` : `${d}d`;
  if (h > 0) return m ? `${h}h ${m}m` : `${h}h`;
  if (m > 0) return `${m}m`;
  return `${s}s`;
}

const integer = new Intl.NumberFormat(undefined, { maximumFractionDigits: 0 });
export const formatCount = (n: number) => integer.format(n);

/** Plural helper: plural(2, "device") → "2 devices". */
export function plural(n: number, one: string, many = `${one}s`): string {
  return `${formatCount(n)} ${n === 1 ? one : many}`;
}

/** SHA-256 hex → "9F:86:D0:81:…" pairs, the way TLS tools print fingerprints. */
export function fingerprintPairs(hex: string): string[] {
  const clean = hex.replace(/[^0-9a-f]/gi, "").toUpperCase();
  const pairs: string[] = [];
  for (let i = 0; i < clean.length; i += 2) pairs.push(clean.slice(i, i + 2));
  return pairs;
}

/** Two-letter initials for avatars. */
export function initials(name: string | null | undefined, email: string): string {
  const source = (name ?? "").trim();
  if (source) {
    const parts = source.split(/\s+/).filter(Boolean);
    const first = parts[0]?.[0] ?? "";
    const last = parts.length > 1 ? (parts[parts.length - 1]?.[0] ?? "") : "";
    return (first + last).toUpperCase() || email.slice(0, 2).toUpperCase();
  }
  return email.slice(0, 2).toUpperCase();
}

/** `host:443` → `host`; any other port stays. */
export function displayHost(publicUrl: string): string {
  return publicUrl.replace(/^https?:\/\//, "").replace(/:443$/, "");
}

/** Case-insensitive "contains" across a row's searchable fields. */
export function matches(
  query: string,
  ...fields: (string | null | undefined)[]
): boolean {
  const q = query.trim().toLowerCase();
  if (!q) return true;
  return fields.some((f) => f?.toLowerCase().includes(q));
}
