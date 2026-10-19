import type { AuditEntry } from "./types";

export type AuditIcon =
  | "session"
  | "password"
  | "settings"
  | "key"
  | "key-off"
  | "user"
  | "user-off"
  | "user-x"
  | "device"
  | "canvas"
  | "pairing"
  | "ai"
  | "sso"
  | "agent"
  | "addon"
  | "other";

export interface AuditLine {
  icon: AuditIcon;
  /** Verb phrase after the actor: "created access key". */
  verb: string;
  /** The object, rendered with emphasis. */
  object?: string;
  /** Trailing plain text. */
  suffix?: string;
  /** Who did it, for rows without an admin (server CLI, a person's Copper). */
  actor?: string;
}

const str = (v: unknown) => (typeof v === "string" && v ? v : undefined);

/** Turn an `admin_audit` row into a readable sentence fragment. */
export function describeAudit(entry: AuditEntry, self: boolean): AuditLine {
  const d = entry.detail ?? {};
  switch (entry.action) {
    case "login":
      return { icon: "session", verb: "signed in" };
    case "logout":
      return { icon: "session", verb: "signed out" };
    case "password.change":
      return {
        icon: "password",
        verb: self ? "changed your password" : "changed their password",
      };
    case "settings.update": {
      const parts: string[] = [];
      if (d.access_mode === "open" || d.access_mode === "directory") {
        parts.push(
          `set access mode to ${d.access_mode === "open" ? "Open" : "Directory"}`,
        );
      }
      if (typeof d.allow_signup === "boolean") {
        parts.push(d.allow_signup ? "turned sign-up on" : "turned sign-up off");
      }
      return {
        icon: "settings",
        verb: parts.length ? parts.join(" and ") : "updated settings",
      };
    }
    case "access_key.create":
      return {
        icon: "key",
        verb: "created access key",
        object: str(d.label),
        suffix: str(d.email) ? `for ${str(d.email)}` : undefined,
      };
    case "access_key.revoke":
      return { icon: "key-off", verb: "revoked access key", object: str(d.label) };
    case "user.update": {
      const email = str(d.email);
      if (d.disabled === true)
        return { icon: "user-off", verb: "disabled", object: email };
      if (d.disabled === false) return { icon: "user", verb: "enabled", object: email };
      if (str(d.display_name)) {
        return {
          icon: "user",
          verb: "renamed",
          object: email,
          suffix: `to “${str(d.display_name)}”`,
        };
      }
      return { icon: "user", verb: "updated", object: email ?? "a person" };
    }
    case "user.delete":
      return { icon: "user-x", verb: "deleted", object: str(d.email) ?? "a person" };
    case "user.reset_password":
      return {
        icon: "password",
        verb: "reset the password for",
        object: str(d.email) ?? "a person",
      };
    case "device.delete":
      return { icon: "device", verb: "removed a device" };
    case "canvas.delete":
      return { icon: "canvas", verb: "deleted canvas", object: str(d.name) };
    case "intelligence.update": {
      const parts: string[] = [];
      if (d.jev === null) parts.push("removed the Jev key");
      else if (d.jev) parts.push("updated the Jev settings");
      if (d.router === null) parts.push("removed the router key");
      else if (d.router) parts.push("updated the router settings");
      if (d.agent === null) parts.push("removed the agent round limit");
      else if (d.agent && typeof d.agent === "object") {
        const n = (d.agent as { max_turns?: unknown }).max_turns;
        parts.push(
          typeof n === "number"
            ? `set the agent round limit to ${n}`
            : "set the agent round limit",
        );
      }
      if (d.enabled === true) parts.push("turned AI key sharing on");
      if (d.enabled === false) parts.push("turned AI key sharing off");
      return {
        icon: "ai",
        verb: parts.length ? parts.join(" and ") : "updated the AI keys",
        actor: d.via === "cli" ? "The server CLI" : undefined,
      };
    }
    case "intelligence.clear":
      return {
        icon: "ai",
        verb: "removed all AI keys",
        actor: d.via === "cli" ? "The server CLI" : undefined,
      };
    case "intelligence.read":
      return {
        icon: "ai",
        verb: "fetched the AI keys",
        actor: str(d.email) ?? "A person",
      };
    case "pairing_code.revoke":
      return { icon: "pairing", verb: "revoked a pairing code" };
    case "addon.create":
    case "addon.upload":
    case "addon.update":
    case "addon.delete":
      return describeAddonAudit(entry, d);
    default:
      return (
        describeAccess(entry.action, d, entry.target) ?? {
          icon: "other",
          verb: entry.action.replace(/[._]/g, " "),
        }
      );
  }
}

const PAST: Record<string, string> = {
  create: "created",
  update: "updated",
  set: "updated",
  put: "updated",
  delete: "removed",
  remove: "removed",
  revoke: "revoked",
  rotate: "rotated the key for",
  test: "tested",
  enable: "turned on",
  disable: "turned off",
};

/**
 * Single sign-on (`sso.*`) and agent access (`integration.*`, `agent.*`) rows
 * (copper-cloud ≥ 0.9.0), phrased from the last segment so new actions still read.
 */
function describeAccess(
  action: string,
  d: Record<string, unknown>,
  target: string | null | undefined,
): AuditLine | undefined {
  const parts = action.split(".");
  const area = parts[0];
  const verb = PAST[parts[parts.length - 1] ?? ""];
  const via = d.via === "cli" ? "The server CLI" : undefined;

  if (area === "sso") {
    if (typeof d.password_signin === "boolean") {
      return {
        icon: "sso",
        verb: d.password_signin
          ? "turned password sign-in on"
          : "turned password sign-in off",
        actor: via,
      };
    }
    // A person's own first sign-in: no admin involved.
    if (parts[1] === "user_create" || parts[1] === "link") {
      return {
        icon: "sso",
        verb: parts[1] === "user_create" ? "joined with" : "linked their account to",
        object: str(d.provider) ?? "single sign-on",
        actor: str(d.email) ?? "A person",
      };
    }
    const list = Array.isArray(d.providers)
      ? d.providers.filter((p): p is string => typeof p === "string").join(", ")
      : undefined;
    const name =
      str(d.name) ??
      str(d.provider) ??
      (list || undefined) ??
      (parts[1] === "provider" ? (target ?? undefined) : undefined);
    if (!verb) return { icon: "sso", verb: "changed single sign-on", actor: via };
    if (!name) return { icon: "sso", verb: `${verb} single sign-on`, actor: via };
    return { icon: "sso", verb: `${verb} sign-in with`, object: name, actor: via };
  }
  if (area === "integration" || area === "agent_integration") {
    return {
      icon: "agent",
      verb: `${verb ?? "changed"} integration`,
      object: str(d.name),
      actor: via,
    };
  }
  if (area === "agent" && parts[1] === "pair") {
    return {
      icon: "agent",
      verb: "paired a Copper for",
      object: str(d.email) ?? "a person",
      actor: str(d.integration) ?? str(d.integration_name) ?? "An integration",
    };
  }
  return undefined;
}

/** `addon.*` rows. `target` is the addon id; updates list what changed. */
function describeAddonAudit(entry: AuditEntry, d: Record<string, unknown>): AuditLine {
  const object = str(d.name) ?? str(entry.target) ?? str(d.id) ?? "an addon";
  const version = str(d.version);
  const actor = d.via === "cli" ? "The server CLI" : undefined;
  const line = (verb: string, suffix?: string): AuditLine => ({
    icon: "addon",
    verb,
    object,
    suffix,
    actor,
  });
  switch (entry.action) {
    case "addon.create":
      return line("uploaded addon", version ? `(${version}, off)` : undefined);
    case "addon.upload":
      return line(version ? `uploaded ${version} of` : "uploaded a new version of");
    case "addon.delete":
      return line("deleted addon");
  }
  const changes: AuditLine[] = [];
  if (typeof d.enabled === "boolean")
    changes.push(line(d.enabled ? "turned on" : "turned off"));
  if (d.audience === "everyone") changes.push(line("made", "available to everyone"));
  else if (d.audience === "people" || d.people !== undefined)
    changes.push(line("changed who gets"));
  if (d.settings !== undefined) changes.push(line("changed the settings of"));
  if (typeof d.priority === "number")
    changes.push(line("set the priority of", `to ${d.priority}`));
  const current = str(d.current_version);
  if (current) changes.push(line(`made ${current} current for`));
  return changes.length === 1 ? changes[0]! : line("updated addon");
}
