"use client";

import { BracesIcon } from "lucide-react";
import { useId, useState } from "react";

import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import { Switch } from "@/components/ui/switch";
import type { AddonSettingSpec } from "@/lib/types";
import { cn } from "@/lib/utils";

type Values = Record<string, unknown>;
/** Form drafts: text for string/number/object/array, boolean|undefined for booleans. */
type Draft = Record<string, string | boolean | undefined>;

const TEXTAREA =
  "min-h-16 w-full min-w-0 resize-y rounded-md border border-input bg-transparent px-2.5 py-1.5 font-mono text-xs/5 transition-colors outline-none placeholder:text-muted-foreground focus-visible:border-ring focus-visible:ring-3 focus-visible:ring-ring/50 disabled:opacity-50 aria-invalid:border-destructive aria-invalid:ring-3 aria-invalid:ring-destructive/20 dark:bg-input/30";

function draftFrom(specs: [string, AddonSettingSpec][], values: Values): Draft {
  const draft: Draft = {};
  for (const [key, spec] of specs) {
    const v = values[key];
    if (spec.type === "boolean") draft[key] = typeof v === "boolean" ? v : undefined;
    else if (spec.type === "string") draft[key] = typeof v === "string" ? v : "";
    else if (spec.type === "number") draft[key] = typeof v === "number" ? String(v) : "";
    else draft[key] = v === undefined ? "" : JSON.stringify(v, null, 2);
  }
  return draft;
}

/** Draft → settings object; blank fields fall back to the manifest default. */
function valuesFrom(
  specs: [string, AddonSettingSpec][],
  draft: Draft,
): { values: Values; errors: Record<string, string> } {
  const values: Values = {};
  const errors: Record<string, string> = {};
  for (const [key, spec] of specs) {
    const d = draft[key];
    if (spec.type === "boolean") {
      if (typeof d === "boolean") values[key] = d;
      continue;
    }
    const s = typeof d === "string" ? d : "";
    if (spec.type === "string") {
      if (s !== "") values[key] = s;
      continue;
    }
    if (!s.trim()) continue;
    if (spec.type === "number") {
      const n = Number(s.trim());
      if (Number.isFinite(n)) values[key] = n;
      else errors[key] = "Enter a number.";
      continue;
    }
    try {
      const parsed: unknown = JSON.parse(s);
      const isArray = Array.isArray(parsed);
      const isObject = typeof parsed === "object" && parsed !== null && !isArray;
      if (spec.type === "array" ? isArray : isObject) values[key] = parsed;
      else
        errors[key] =
          spec.type === "array" ? "Enter a JSON array." : "Enter a JSON object.";
    } catch {
      errors[key] = "That isn't valid JSON.";
    }
  }
  return { values, errors };
}

function parseJsonObject(text: string): { values?: Values; error?: string } {
  if (!text.trim()) return { values: {} };
  try {
    const parsed: unknown = JSON.parse(text);
    if (typeof parsed !== "object" || parsed === null || Array.isArray(parsed)) {
      return { error: 'Settings must be a JSON object, like {"key": "value"}.' };
    }
    return { values: parsed as Values };
  } catch (error) {
    return {
      error:
        `That isn't valid JSON. ${error instanceof Error ? error.message : ""}`.trim(),
    };
  }
}

const preview = (v: unknown) =>
  v === undefined || v === "" ? undefined : typeof v === "string" ? v : JSON.stringify(v);

interface SettingsFormProps {
  /** Id for the "Settings" heading. */
  headingId: string;
  specs: [string, AddonSettingSpec][];
  /** Admin-set values (manifest defaults fill the rest). */
  values: Values;
  /** Resolve to an error message to show inline, or null when saved. */
  onSave: (values: Values) => Promise<string | null>;
}

/**
 * Settings generated from `manifest.settings`: text and number inputs,
 * switches for booleans, JSON boxes for objects and arrays. "Edit as JSON"
 * swaps the whole form for one JSON object.
 */
export function SettingsForm({ headingId, specs, values, onSave }: SettingsFormProps) {
  const [mode, setMode] = useState<"form" | "json">("form");
  const [draft, setDraft] = useState<Draft>(() => draftFrom(specs, values));
  const [json, setJson] = useState(() => JSON.stringify(values, null, 2));
  const [errors, setErrors] = useState<Record<string, string>>({});
  const [formError, setFormError] = useState<string | null>(null);
  const [dirty, setDirty] = useState(false);
  const [pending, setPending] = useState(false);
  const jsonId = useId();

  function edit(key: string, value: string | boolean | undefined) {
    setDraft((d) => ({ ...d, [key]: value }));
    setDirty(true);
    if (errors[key]) setErrors(({ [key]: _gone, ...rest }) => rest);
  }

  function switchMode() {
    setFormError(null);
    if (mode === "form") {
      const built = valuesFrom(specs, draft);
      setErrors(built.errors);
      if (Object.keys(built.errors).length) return;
      setJson(JSON.stringify(built.values, null, 2));
      setMode("json");
    } else {
      const parsed = parseJsonObject(json);
      if (parsed.error) {
        setFormError(parsed.error);
        return;
      }
      setDraft(draftFrom(specs, parsed.values ?? {}));
      setMode("form");
    }
  }

  function reset() {
    setDraft(draftFrom(specs, values));
    setJson(JSON.stringify(values, null, 2));
    setErrors({});
    setFormError(null);
    setDirty(false);
  }

  async function submit(event: React.FormEvent) {
    event.preventDefault();
    if (pending) return;
    setFormError(null);
    let next: Values;
    if (mode === "json") {
      const parsed = parseJsonObject(json);
      if (parsed.error || !parsed.values) {
        setFormError(parsed.error ?? "That isn't valid JSON.");
        return;
      }
      next = parsed.values;
    } else {
      const built = valuesFrom(specs, draft);
      setErrors(built.errors);
      if (Object.keys(built.errors).length) return;
      next = built.values;
    }
    setPending(true);
    const error = await onSave(next);
    setPending(false);
    if (error) setFormError(error);
    else setDirty(false);
  }

  const heading = (
    <div className="flex min-h-7 items-center justify-between gap-3">
      <h4 id={headingId} className="text-sm font-medium">
        Settings
      </h4>
      {specs.length > 0 && (
        <Button
          type="button"
          variant="ghost"
          size="sm"
          className="-mr-2 text-muted-foreground"
          aria-pressed={mode === "json"}
          onClick={switchMode}
        >
          <BracesIcon aria-hidden="true" />
          Edit as JSON
        </Button>
      )}
    </div>
  );

  if (specs.length === 0) {
    return (
      <div className="grid gap-1">
        {heading}
        <p className="text-muted-foreground">This addon has no settings.</p>
      </div>
    );
  }

  return (
    <form onSubmit={submit} className="grid gap-4" noValidate aria-labelledby={headingId}>
      <div className="grid gap-1">
        {heading}
        <p className="text-pretty text-muted-foreground">
          Blank fields use the addon&rsquo;s default. Coppers get changes on their next
          check.
        </p>
      </div>

      {mode === "json" ? (
        <div className="grid gap-1.5">
          <Label htmlFor={jsonId}>Settings JSON</Label>
          <textarea
            id={jsonId}
            value={json}
            onChange={(e) => {
              setJson(e.target.value);
              setDirty(true);
            }}
            rows={Math.min(14, Math.max(4, json.split("\n").length + 1))}
            spellCheck={false}
            autoComplete="off"
            className={TEXTAREA}
            aria-invalid={formError ? true : undefined}
          />
          <p className="text-xs text-pretty text-muted-foreground">
            Only keys from the addon&rsquo;s manifest:{" "}
            {specs.map(([key], i) => (
              <span key={key}>
                {i > 0 && ", "}
                <code className="font-mono">{key}</code>
              </span>
            ))}
            .
          </p>
        </div>
      ) : (
        specs.map(([key, spec]) => (
          <SettingField
            key={key}
            name={key}
            spec={spec}
            value={draft[key]}
            error={errors[key]}
            onChange={(v) => edit(key, v)}
          />
        ))
      )}

      {formError && (
        <p role="alert" className="text-sm text-pretty text-destructive">
          {formError}
        </p>
      )}

      <div className="flex items-center gap-2">
        <Button type="submit" disabled={pending || !dirty}>
          {pending ? "Saving…" : "Save settings"}
        </Button>
        {dirty && !pending && (
          <Button type="button" variant="outline" onClick={reset}>
            Discard changes
          </Button>
        )}
      </div>
    </form>
  );
}

function SettingField({
  name,
  spec,
  value,
  error,
  onChange,
}: {
  name: string;
  spec: AddonSettingSpec;
  value: string | boolean | undefined;
  error?: string;
  onChange: (value: string | boolean | undefined) => void;
}) {
  const id = useId();
  const hintId = `${id}-hint`;
  const label = (
    <span className="flex min-w-0 flex-wrap items-baseline gap-x-2 gap-y-0.5">
      <span>{spec.title?.trim() || name}</span>
      {spec.title?.trim() && (
        <code className="font-mono text-xs font-normal text-faint">{name}</code>
      )}
    </span>
  );
  const hint = error ?? spec.description;

  if (spec.type === "boolean") {
    const fallback = spec.default === true;
    const checked = typeof value === "boolean" ? value : fallback;
    return (
      <div className="flex items-start justify-between gap-6">
        <div className="grid min-w-0 gap-1">
          <Label htmlFor={id} className="leading-5">
            {label}
          </Label>
          <p id={hintId} className="text-xs text-pretty text-muted-foreground">
            {spec.description ? `${spec.description} ` : ""}
            Default: {fallback ? "on" : "off"}.
          </p>
        </div>
        <Switch
          id={id}
          checked={checked}
          onCheckedChange={(next) => onChange(next)}
          aria-describedby={hintId}
          className="mt-0.5"
        />
      </div>
    );
  }

  const text = typeof value === "string" ? value : "";
  const placeholder = preview(spec.default) ?? "Not set";
  const common = {
    id,
    value: text,
    "aria-invalid": error ? true : undefined,
    "aria-describedby": hint ? hintId : undefined,
    spellCheck: false,
    autoComplete: "off",
  } as const;

  return (
    <div className="grid content-start gap-1.5">
      <Label htmlFor={id}>{label}</Label>
      {spec.type === "object" || spec.type === "array" ? (
        <textarea
          {...common}
          rows={3}
          placeholder={placeholder}
          onChange={(e) => onChange(e.target.value)}
          className={TEXTAREA}
        />
      ) : (
        <Input
          {...common}
          type={spec.type === "number" ? "number" : "text"}
          inputMode={spec.type === "number" ? "decimal" : undefined}
          placeholder={placeholder}
          onChange={(e) => onChange(e.target.value)}
          className={cn(spec.type === "number" && "max-w-[200px]")}
        />
      )}
      {hint && (
        <p
          id={hintId}
          className={cn(
            "text-xs text-pretty",
            error ? "text-destructive" : "text-muted-foreground",
          )}
        >
          {hint}
        </p>
      )}
    </div>
  );
}
