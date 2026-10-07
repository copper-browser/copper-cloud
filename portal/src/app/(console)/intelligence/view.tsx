"use client";

import { CircleAlertIcon } from "lucide-react";
import { useId, useState } from "react";
import { toast } from "sonner";

import { ConfirmDialog } from "@/components/confirm-dialog";
import { PageHeader } from "@/components/page-header";
import { RelativeTime } from "@/components/relative-time";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import { Skeleton } from "@/components/ui/skeleton";
import { Switch } from "@/components/ui/switch";
import { type Resource, useResource } from "@/hooks/use-resource";
import { api, errorMessage } from "@/lib/api";
import type { IntelligenceSettings, IntelligenceUpdate } from "@/lib/types";
import { cn } from "@/lib/utils";

type Intel = Resource<IntelligenceSettings>;

export function IntelligenceView() {
  const intel = useResource("intelligence", (signal) => api.intelligence.get(signal));
  const data = intel.data;

  return (
    <>
      <PageHeader
        title="AI keys"
        description="Set the Jev and LLM router keys once. Every signed-in Copper on this instance picks them up, so nobody pastes keys by hand. The agent round limit below applies org-wide too."
      />
      {intel.error && !data ? (
        <LoadError error={intel.error} onRetry={intel.reload} />
      ) : !data ? (
        <div className="grid gap-3 py-2" aria-busy="true">
          <Skeleton className="h-[74px] w-full rounded-lg" />
          <Skeleton className="h-40 w-full rounded-lg" />
          <Skeleton className="h-40 w-full rounded-lg" />
          <Skeleton className="h-24 w-full rounded-lg" />
        </div>
      ) : (
        <>
          <SharingSection intel={intel} data={data} />
          <JevSection intel={intel} data={data} />
          <RouterSection intel={intel} data={data} />
          <AgentSection intel={intel} data={data} />
        </>
      )}
    </>
  );
}

function Section({
  id,
  title,
  description,
  children,
}: {
  id: string;
  title: string;
  description: React.ReactNode;
  children: React.ReactNode;
}) {
  return (
    <section
      aria-labelledby={id}
      className="grid gap-x-10 gap-y-4 border-t py-8 first-of-type:border-t-0 first-of-type:pt-2 @3xl:grid-cols-[220px_minmax(0,1fr)]"
    >
      <div>
        <h2 id={id} className="text-sm font-medium">
          {title}
        </h2>
        <div className="mt-1 text-sm text-pretty text-muted-foreground">
          {description}
        </div>
      </div>
      <div className="min-w-0 @3xl:max-w-[560px]">{children}</div>
    </section>
  );
}

async function save(intel: Intel, input: IntelligenceUpdate, message: string) {
  try {
    const next = await api.intelligence.update(input);
    intel.mutate(() => next);
    toast.success(message);
    return true;
  } catch (error) {
    toast.error(`Couldn't save. ${errorMessage(error)}`);
    return false;
  }
}

function updatedBy(by: string | null) {
  if (!by) return null;
  return by === "cli" ? "the server CLI" : by;
}

function SharingSection({ intel, data }: { intel: Intel; data: IntelligenceSettings }) {
  const [pending, setPending] = useState(false);
  const switchId = useId();
  const anySet = !!(data.jev || data.router);

  async function toggle(enabled: boolean) {
    setPending(true);
    await save(
      intel,
      { enabled },
      enabled ? "Coppers now receive the AI keys" : "AI key sharing turned off",
    );
    setPending(false);
  }

  return (
    <Section
      id="sharing-title"
      title="Sharing"
      description="Signed-in Coppers fetch these keys from the server. Turn sharing off to stop handing them out without deleting them."
    >
      <div className="grid gap-3">
        <div className="flex items-start justify-between gap-6 rounded-lg border px-3.5 py-3">
          <div className="grid gap-0.5">
            <Label htmlFor={switchId} className="leading-5">
              Share keys with signed-in Coppers
            </Label>
            <p className="text-pretty text-muted-foreground">
              {!anySet
                ? "No keys are set yet. Add one below."
                : data.enabled
                  ? "On. Anyone signed in to this instance can use the keys below."
                  : "Off. Coppers see no keys until you turn this back on."}
            </p>
          </div>
          <Switch
            id={switchId}
            checked={data.enabled}
            disabled={pending}
            onCheckedChange={(checked) => void toggle(checked)}
            className="mt-0.5"
          />
        </div>
        {data.updated_at && (
          <p className="text-xs text-muted-foreground">
            Last changed <RelativeTime value={data.updated_at} />
            {updatedBy(data.updated_by) && <> by {updatedBy(data.updated_by)}</>}.
          </p>
        )}
      </div>
    </Section>
  );
}

function StoredKey({ last4 }: { last4: string | undefined }) {
  return (
    <div className="flex h-9 items-center justify-between gap-3 rounded-md border bg-well px-3 text-sm">
      <span className="text-muted-foreground">Stored key</span>
      {last4 === undefined ? (
        <span className="text-muted-foreground">Not set</span>
      ) : (
        <span className="font-mono text-xs text-foreground">
          {last4 ? `••••••••${last4}` : "••••••••"}
        </span>
      )}
    </div>
  );
}

interface FieldProps {
  label: string;
  value: string;
  onChange: (v: string) => void;
  placeholder?: string;
  hint?: string;
  secret?: boolean;
  error?: string;
}

function Field({ label, value, onChange, placeholder, hint, secret, error }: FieldProps) {
  const id = useId();
  return (
    <div className="grid content-start gap-1.5">
      <Label htmlFor={id}>{label}</Label>
      <Input
        id={id}
        type={secret ? "password" : "text"}
        value={value}
        onChange={(e) => onChange(e.target.value)}
        placeholder={placeholder}
        autoComplete="off"
        spellCheck={false}
        className={cn(secret && "font-mono placeholder:font-sans")}
        aria-invalid={error ? true : undefined}
        aria-describedby={error || hint ? `${id}-hint` : undefined}
      />
      {(error || hint) && (
        <p
          id={`${id}-hint`}
          className={cn("text-xs", error ? "text-destructive" : "text-muted-foreground")}
        >
          {error ?? hint}
        </p>
      )}
    </div>
  );
}

const isUrl = (v: string) => /^https?:\/\/[^\s/]/.test(v.trim());

function KeyForm({
  stored,
  fields,
  onSave,
  onRemove,
  removeTitle,
  removeDescription,
}: {
  stored: string | undefined;
  fields: React.ReactNode;
  onSave: (key: string | undefined) => Promise<boolean>;
  onRemove: () => Promise<void>;
  removeTitle: string;
  removeDescription: string;
}) {
  const [key, setKey] = useState("");
  const [keyError, setKeyError] = useState<string>();
  const [pending, setPending] = useState(false);
  const [confirmOpen, setConfirmOpen] = useState(false);

  async function submit(event: React.FormEvent) {
    event.preventDefault();
    if (pending) return;
    const trimmed = key.trim();
    if (stored === undefined && !trimmed) {
      setKeyError("Paste the key to turn this on.");
      return;
    }
    if (trimmed && (trimmed.length < 8 || /\s/.test(trimmed))) {
      setKeyError("Keys are at least 8 characters, without spaces.");
      return;
    }
    setKeyError(undefined);
    setPending(true);
    const ok = await onSave(trimmed || undefined);
    setPending(false);
    if (ok) setKey("");
  }

  return (
    <form onSubmit={submit} className="grid gap-4" noValidate>
      <StoredKey last4={stored} />
      <Field
        label={stored === undefined ? "Key" : "Replace key"}
        value={key}
        onChange={setKey}
        secret
        placeholder={
          stored === undefined ? "Paste the key" : "Leave blank to keep the stored key"
        }
        hint="Write-only: the server never shows a stored key again."
        error={keyError}
      />
      {fields}
      <div className="flex items-center gap-2">
        <Button type="submit" disabled={pending}>
          {pending ? "Saving…" : "Save"}
        </Button>
        {stored !== undefined && (
          <Button type="button" variant="outline" onClick={() => setConfirmOpen(true)}>
            Remove
          </Button>
        )}
      </div>
      <ConfirmDialog
        open={confirmOpen}
        onOpenChange={setConfirmOpen}
        title={removeTitle}
        description={removeDescription}
        confirmLabel="Remove"
        pendingLabel="Removing…"
        onConfirm={onRemove}
      />
    </form>
  );
}

function JevSection({ intel, data }: { intel: Intel; data: IntelligenceSettings }) {
  const [endpoint, setEndpoint] = useState(data.jev?.endpoint ?? "");
  const [model, setModel] = useState(data.jev?.model ?? "");
  const [error, setError] = useState<string>();

  return (
    <Section
      id="jev-title"
      title="Jev"
      description="TypeSafe's Jev powers Copper's fast in-page agent. One key for the whole instance."
    >
      <KeyForm
        stored={data.jev ? data.jev.key_last4 : undefined}
        removeTitle="Remove the Jev key?"
        removeDescription="Coppers stop receiving it on their next refresh. Anyone who wants Jev will need their own key."
        onRemove={async () => {
          const next = await api.intelligence.update({ jev: null });
          intel.mutate(() => next);
          toast.success("Jev key removed");
        }}
        onSave={async (key) => {
          if (endpoint.trim() && !isUrl(endpoint)) {
            setError("Use an http(s):// URL.");
            return false;
          }
          setError(undefined);
          return save(
            intel,
            {
              jev: {
                key,
                endpoint: endpoint.trim() || undefined,
                model: model.trim() || undefined,
              },
            },
            "Jev settings saved",
          );
        }}
        fields={
          <div className="grid gap-4 @xl:grid-cols-[minmax(0,1fr)_180px]">
            <Field
              label="Endpoint"
              value={endpoint}
              onChange={setEndpoint}
              placeholder={data.defaults.jev_endpoint}
              error={error}
            />
            <Field
              label="Model"
              value={model}
              onChange={setModel}
              placeholder={data.defaults.jev_model}
            />
          </div>
        }
      />
    </Section>
  );
}

function RouterSection({ intel, data }: { intel: Intel; data: IntelligenceSettings }) {
  const [url, setUrl] = useState(data.router?.url ?? "");
  const [error, setError] = useState<string>();

  return (
    <Section
      id="router-title"
      title="LLM router"
      description="An OpenAI-compatible LiteLLM gateway key. Copper uses it for chat, summaries and canvas agents."
    >
      <KeyForm
        stored={data.router ? data.router.key_last4 : undefined}
        removeTitle="Remove the router key?"
        removeDescription="Coppers stop receiving it on their next refresh. Anyone who wants AI features will need their own key."
        onRemove={async () => {
          const next = await api.intelligence.update({ router: null });
          intel.mutate(() => next);
          toast.success("Router key removed");
        }}
        onSave={async (key) => {
          if (!url.trim() && !data.router) {
            setError("Enter your gateway's base URL.");
            return false;
          }
          if (url.trim() && !isUrl(url)) {
            setError("Use an http(s):// URL.");
            return false;
          }
          setError(undefined);
          return save(
            intel,
            { router: { key, url: url.trim() || undefined } },
            "Router settings saved",
          );
        }}
        fields={
          <Field
            label="Router URL"
            value={url}
            onChange={setUrl}
            placeholder="https://llm.example.com"
            error={error}
          />
        }
      />
    </Section>
  );
}

const AGENT_MIN_TURNS = 1;
const AGENT_MAX_TURNS = 500;

function AgentSection({ intel, data }: { intel: Intel; data: IntelligenceSettings }) {
  const stored = data.agent?.max_turns;
  const [value, setValue] = useState(stored === undefined ? "" : String(stored));
  const [error, setError] = useState<string>();
  const [pending, setPending] = useState(false);
  const id = useId();
  const fallback = data.defaults.agent_max_turns;

  async function put(maxTurns: number | null) {
    setPending(true);
    const ok = await save(
      intel,
      { agent: maxTurns === null ? null : { max_turns: maxTurns } },
      maxTurns === null
        ? "Agent round limit cleared"
        : `Agent round limit set to ${maxTurns}`,
    );
    setPending(false);
    if (ok) setValue(maxTurns === null ? "" : String(maxTurns));
  }

  async function submit(event: React.FormEvent) {
    event.preventDefault();
    if (pending) return;
    const trimmed = value.trim();
    if (!trimmed) {
      setError(undefined);
      if (stored !== undefined) await put(null);
      return;
    }
    const n = Number(trimmed);
    if (!/^\d+$/.test(trimmed) || n < AGENT_MIN_TURNS || n > AGENT_MAX_TURNS) {
      setError(`Use a whole number from ${AGENT_MIN_TURNS} to ${AGENT_MAX_TURNS}.`);
      return;
    }
    setError(undefined);
    await put(n);
  }

  return (
    <Section
      id="agent-title"
      title="Agent"
      description="How many rounds of tool calls Copper's agent may run for one question before it stops and asks you to continue. Applies to everyone, whether or not keys are shared."
    >
      <form onSubmit={submit} className="grid gap-4" noValidate>
        <div className="grid content-start gap-1.5">
          <Label htmlFor={id}>Tool-call rounds per question (whole org)</Label>
          <Input
            id={id}
            type="number"
            inputMode="numeric"
            min={AGENT_MIN_TURNS}
            max={AGENT_MAX_TURNS}
            step={1}
            value={value}
            onChange={(e) => setValue(e.target.value)}
            placeholder={String(fallback)}
            className="@xl:max-w-[180px]"
            aria-invalid={error ? true : undefined}
            aria-describedby={`${id}-hint`}
          />
          <p
            id={`${id}-hint`}
            className={cn(
              "text-xs",
              error ? "text-destructive" : "text-muted-foreground",
            )}
          >
            {error ?? `Blank = each person's own Copper setting (default ${fallback}).`}
          </p>
        </div>
        <div className="flex items-center gap-2">
          <Button type="submit" disabled={pending}>
            {pending ? "Saving…" : "Save"}
          </Button>
          {stored !== undefined && (
            <Button
              type="button"
              variant="outline"
              disabled={pending}
              onClick={() => void put(null)}
            >
              Clear
            </Button>
          )}
        </div>
      </form>
    </Section>
  );
}

function LoadError({ error, onRetry }: { error: unknown; onRetry: () => void }) {
  return (
    <div className="flex items-start gap-3 rounded-lg border px-3.5 py-3 text-sm">
      <CircleAlertIcon
        className="mt-0.5 size-4 shrink-0 text-destructive"
        aria-hidden="true"
      />
      <div>
        <p className="font-medium">Couldn&rsquo;t load the AI keys</p>
        <p className="mt-0.5 text-muted-foreground">{errorMessage(error)}</p>
        <Button variant="outline" size="sm" className="mt-3" onClick={onRetry}>
          Try again
        </Button>
      </div>
    </div>
  );
}
