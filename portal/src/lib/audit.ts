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
    default:
      return { icon: "other", verb: entry.action.replace(/[._]/g, " ") };
  }
}
