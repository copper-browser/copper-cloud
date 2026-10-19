import type { Addon, AddonSettingSpec } from "@/lib/types";

export const EMAIL_RE = /^[^@\s]+@[^@\s]+\.[^@\s]+$/;

const kb = new Intl.NumberFormat(undefined, { maximumFractionDigits: 1 });

/** 812 B, 14.2 KB, 3.1 MB (1024-based, like the server's limits). */
export function formatBytes(n: number): string {
  if (n < 1024) return `${n} B`;
  if (n < 1024 * 1024) return `${kb.format(n / 1024)} KB`;
  return `${kb.format(n / (1024 * 1024))} MB`;
}

/** Copper picks nobody: on, but audience is "people" with none listed. */
export const reachesNobody = (a: Addon) =>
  a.audience === "people" && a.people.length === 0;

export const settingSpecs = (a: Addon): [string, AddonSettingSpec][] =>
  Object.entries(a.manifest?.settings ?? {});

/** Short form of a hash for display: first 12 hex characters. */
export const shortSha = (sha: string) => sha.slice(0, 12);
