"use client";

import { CircleAlertIcon, CircleCheckIcon, CloudOffIcon } from "lucide-react";
import { useId, useRef, useState } from "react";
import { toast } from "sonner";

import { ConfirmDialog } from "@/components/confirm-dialog";
import { CopyButton } from "@/components/copy-button";
import { Fingerprint } from "@/components/fingerprint";
import { PageHeader } from "@/components/page-header";
import { RelativeTime } from "@/components/relative-time";
import { useSession } from "@/components/session-provider";
import { StatusBadge, type Tone } from "@/components/status-badge";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import { Skeleton } from "@/components/ui/skeleton";
import { useNow } from "@/hooks/use-now";
import { type Resource, useResource } from "@/hooks/use-resource";
import { api, errorMessage } from "@/lib/api";
import { formatCount, plural } from "@/lib/format";
import type { CanvasLink, CanvasLinkTest } from "@/lib/types";
import { cn } from "@/lib/utils";

type Link = Resource<CanvasLink>;

const APP_KEY_COMMAND = 'canvas-cloud apps create --name "Copper Cloud" --allow-import';
const MIGRATE_COMMAND = "copper-cloud canvas migrate";

export function CanvasCloudView() {
  const link = useResource("canvas-link", (signal) => api.canvasLink.get(signal));
  const data = link.data;

  return (
    <>
      <PageHeader
        title="Canvas Cloud"
        description="Canvases live on a Canvas Cloud server. Link one and Copper users’ canvases are served from it — same app, same links."
      />
      {link.error && !data ? (
        <LoadError error={link.error} onRetry={link.reload} />
      ) : !data ? (
        <div className="grid gap-3 py-2" aria-busy="true">
          <Skeleton className="h-52 w-full rounded-lg" />
          <Skeleton className="h-[74px] w-full rounded-lg" />
          <Skeleton className="h-28 w-full rounded-lg" />
        </div>
      ) : data.linked ? (
        <>
          <ConnectionSection link={link} data={data} />
          <BridgeSection data={data} />
          <MigrationSection data={data} />
          <KeySection link={link} data={data} />
        </>
      ) : (
        <>
          <NotLinkedSection />
          <Section
            id="link-title"
            title="Link a Canvas Cloud"
            description="Point this instance at a Canvas Cloud you run. Coppers need no update: canvas links and invites keep working."
          >
            <div className="grid gap-5">
              <div className="grid gap-1.5">
                <p className="text-muted-foreground">
                  On the Canvas Cloud server, create an app key for this Copper Cloud:
                </p>
                <Command value={APP_KEY_COMMAND} />
              </div>
              <LinkForm mode="link" link={link} data={data} />
            </div>
          </Section>
          {data.migrations.legacy_canvases > 0 && <MigrationSection data={data} />}
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

/** Server-side error strings arrive lowercase and unpunctuated. */
function sentence(s: string): string {
  const t = s.trim();
  if (!t) return t;
  const first = t.charAt(0).toUpperCase() + t.slice(1);
  return /[.!?)]$/.test(first) ? first : `${first}.`;
}

/**
 * A timestamp for something that already happened. The shared relative-time clock
 * ticks every 30 s, so a fresh server timestamp can read "in a moment"; clamp it.
 */
function PastTime({
  value,
  empty,
  className,
}: {
  value: string | null;
  empty?: string;
  className?: string;
}) {
  const now = useNow() || Date.now();
  const clamped = value && Date.parse(value) > now ? new Date(now).toISOString() : value;
  return <RelativeTime value={clamped} empty={empty} className={className} />;
}

const hostOf = (url: string) => url.replace(/^https?:\/\//i, "").replace(/\/+$/, "");

function updatedBy(by: string | null) {
  if (!by) return null;
  return by === "cli" ? "the server CLI" : by;
}

function reachability(data: CanvasLink): { tone: Tone; label: string } {
  if (!data.enabled) return { tone: "muted", label: "Turned off" };
  if (data.status.reachable === true) return { tone: "success", label: "Reachable" };
  if (data.status.reachable === false) return { tone: "danger", label: "Unreachable" };
  return { tone: "muted", label: "Not checked yet" };
}

/** What `DELETE canvas-link` leaves behind, so the page flips before the reload lands. */
function unlinked(prev: CanvasLink): CanvasLink {
  return {
    ...prev,
    linked: false,
    url: null,
    enabled: false,
    tls_fingerprint: null,
    linked_at: null,
    status: { reachable: null, version: null, error: null, checked_at: null },
    bridge: { connected: false, last_event_at: null, last_error: null },
  };
}

function Row({ label, children }: { label: string; children: React.ReactNode }) {
  return (
    <div className="grid grid-cols-[88px_minmax(0,1fr)] items-center gap-3 px-4 py-2">
      <dt className="text-muted-foreground">{label}</dt>
      <dd className="min-w-0">{children}</dd>
    </div>
  );
}

/** A shell command to run on a server, with a copy button. */
function Command({ value }: { value: string }) {
  return (
    <div className="flex items-start gap-1 rounded-md border bg-well py-1 pr-1 pl-3">
      <code className="min-w-0 flex-1 py-1 font-mono text-[12.5px]/5 wrap-break-word text-foreground select-all">
        {value}
      </code>
      <CopyButton value={value} label="command" />
    </div>
  );
}

// Linked --------------------------------------------------------------------

function ConnectionSection({ link, data }: { link: Link; data: CanvasLink }) {
  const { refreshOverview } = useSession();
  const [testing, setTesting] = useState(false);
  const [result, setResult] = useState<CanvasLinkTest | null>(null);
  const [confirmOpen, setConfirmOpen] = useState(false);
  const url = data.url ?? "";
  const host = hostOf(url);
  const reach = reachability(data);
  const plainHttp = /^http:\/\//i.test(url);

  async function test() {
    if (testing) return;
    setTesting(true);
    setResult(null);
    try {
      setResult(await api.canvasLink.test());
    } catch (error) {
      setResult({
        ok: false,
        version: null,
        latency_ms: null,
        error: errorMessage(error),
      });
    }
    // The test refreshes the server's own health check; show it.
    await link.reload();
    setTesting(false);
  }

  return (
    <Section
      id="connection-title"
      title="Connection"
      description="Copper Cloud signs in to the Canvas Cloud with its app key and acts for whoever is signed in to Copper."
    >
      <div className="rounded-lg border">
        <div className="flex flex-wrap items-center justify-between gap-x-3 gap-y-1.5 border-b px-4 py-2.5">
          <p className="min-w-0 truncate font-medium" title={url}>
            {host}
          </p>
          <StatusBadge tone={reach.tone}>{reach.label}</StatusBadge>
        </div>

        <dl className="py-1.5 text-sm">
          <Row label="Address">
            <div className="flex items-center gap-1">
              <code className="truncate font-mono text-[12.5px]" title={url}>
                {url}
              </code>
              <CopyButton value={url} label="Canvas Cloud address" className="-my-1" />
            </div>
          </Row>
          <Row label="Version">
            {data.status.version ? (
              <span className="font-mono text-[12.5px]">{data.status.version}</span>
            ) : (
              <span className="text-faint">Unknown</span>
            )}
          </Row>
          <Row label="Checked">
            <PastTime value={data.status.checked_at} empty="Not yet" />
          </Row>
          <Row label="Linked">
            <PastTime value={data.linked_at} />
          </Row>
          <Row label="Certificate">
            {data.tls_fingerprint ? (
              "Self-signed, pinned"
            ) : plainHttp ? (
              <span className="text-muted-foreground">
                None, plain http on this machine
              </span>
            ) : (
              "Publicly trusted"
            )}
          </Row>
          {data.tls_fingerprint && (
            <div className="px-4 pt-1.5 pb-2.5">
              <p className="mb-1.5 text-muted-foreground">Pinned fingerprint (SHA-256)</p>
              <div className="rounded-md border bg-well px-3 py-2">
                <Fingerprint value={data.tls_fingerprint} />
              </div>
            </div>
          )}
        </dl>

        {data.status.reachable === false && data.status.error && (
          <div className="mx-4 mb-3 flex items-start gap-2 rounded-md border border-destructive/25 bg-destructive/5 px-3 py-2 text-sm">
            <CircleAlertIcon
              className="mt-0.5 size-3.5 shrink-0 text-destructive"
              aria-hidden="true"
            />
            <p className="min-w-0 text-pretty">
              <span className="font-medium">Copper users can&rsquo;t open canvases.</span>{" "}
              <span className="wrap-break-word text-muted-foreground">
                {sentence(data.status.error)}
              </span>
            </p>
          </div>
        )}

        <div className="flex flex-wrap items-center gap-x-3 gap-y-2 border-t px-4 py-3">
          <Button type="button" variant="outline" onClick={test} disabled={testing}>
            {testing ? "Testing…" : "Test connection"}
          </Button>
          <div aria-live="polite" className="min-w-0 flex-1 text-sm">
            {result && (
              <TestResult
                result={result}
                shownAbove={
                  data.status.reachable === false && result.error === data.status.error
                }
              />
            )}
          </div>
          <Button type="button" variant="outline" onClick={() => setConfirmOpen(true)}>
            Unlink…
          </Button>
        </div>
      </div>

      {!data.enabled && (
        <p className="mt-3 text-sm text-pretty text-muted-foreground">
          This link is turned off, so Copper users can&rsquo;t open canvases. Saving the
          app key below turns it back on.
        </p>
      )}
      {data.updated_at && (
        <p className="mt-3 text-xs text-muted-foreground">
          Last changed <PastTime value={data.updated_at} />
          {updatedBy(data.updated_by) && <> by {updatedBy(data.updated_by)}</>}.
        </p>
      )}

      <ConfirmDialog
        open={confirmOpen}
        onOpenChange={setConfirmOpen}
        title="Unlink this Canvas Cloud?"
        description="Copper users lose canvases until a Canvas Cloud is linked again; nothing is deleted."
        confirmLabel="Unlink"
        pendingLabel="Unlinking…"
        onConfirm={async () => {
          await api.canvasLink.remove();
          link.mutate((prev) => (prev ? unlinked(prev) : prev));
          setResult(null);
          toast.success(`Unlinked ${host}`);
          void link.reload();
          void refreshOverview();
        }}
      >
        <p className="text-sm text-pretty text-muted-foreground">
          The canvases stay on <span className="text-foreground">{host}</span>. Linking it
          again takes an app key from that server.
        </p>
      </ConfirmDialog>
    </Section>
  );
}

function TestResult({
  result,
  shownAbove,
}: {
  result: CanvasLinkTest;
  /** The same error is already in the callout above; don't print it twice. */
  shownAbove: boolean;
}) {
  if (!result.ok) {
    return (
      <p className="flex items-start gap-1.5 text-destructive">
        <CircleAlertIcon className="mt-0.5 size-3.5 shrink-0" aria-hidden="true" />
        <span className="min-w-0 wrap-break-word">
          {shownAbove ? (
            <>
              Test failed.
              <span className="sr-only"> {sentence(result.error ?? "")}</span>
            </>
          ) : result.error ? (
            sentence(result.error)
          ) : (
            "The Canvas Cloud didn’t answer."
          )}
        </span>
      </p>
    );
  }
  return (
    <p className="flex flex-wrap items-center gap-x-1.5 text-foreground/85">
      <CircleCheckIcon className="size-3.5 shrink-0 text-success" aria-hidden="true" />
      <span>Reachable</span>
      {result.version && (
        <>
          <span className="text-faint" aria-hidden="true">
            ·
          </span>
          <span className="font-mono text-[12.5px]">{result.version}</span>
        </>
      )}
      {result.latency_ms !== null && (
        <>
          <span className="text-faint" aria-hidden="true">
            ·
          </span>
          <span className="tabular-nums">{formatCount(result.latency_ms)} ms</span>
        </>
      )}
    </p>
  );
}

function BridgeSection({ data }: { data: CanvasLink }) {
  const { connected, last_event_at: lastEvent, last_error: lastError } = data.bridge;
  return (
    <Section
      id="bridge-title"
      title="Live updates"
      description="The Canvas Cloud streams canvas events (invites, renames, deletions) back here, so open Coppers update without a refresh."
    >
      <div className="grid gap-1 rounded-lg border px-3.5 py-3">
        <div className="flex flex-wrap items-center justify-between gap-x-3 gap-y-1.5">
          <p className="font-medium">Event stream</p>
          <StatusBadge tone={connected ? "success" : "warning"}>
            {connected ? "Connected" : "Disconnected"}
          </StatusBadge>
        </div>
        <p className="text-pretty text-muted-foreground">
          {connected
            ? lastEvent
              ? "Coppers hear about canvas changes as they happen."
              : "Connected. No canvas events yet."
            : "Copper Cloud keeps retrying. Until it reconnects, Coppers see canvas list changes on their next refresh."}
        </p>
        {(lastEvent || (!connected && lastError)) && (
          <dl className="mt-1.5 grid gap-1 text-xs text-muted-foreground">
            {lastEvent && (
              <div className="flex flex-wrap gap-x-1.5">
                <dt>Last event</dt>
                <dd>
                  <PastTime value={lastEvent} className="text-foreground/85" />
                </dd>
              </div>
            )}
            {!connected && lastError && (
              <div className="flex min-w-0 flex-wrap gap-x-1.5">
                <dt>Last error</dt>
                <dd className="min-w-0 font-mono wrap-break-word text-foreground/85">
                  {lastError}
                </dd>
              </div>
            )}
          </dl>
        )}
      </div>
    </Section>
  );
}

function MigrationSection({ data }: { data: CanvasLink }) {
  const { migrated, failed, legacy_canvases: legacy } = data.migrations;
  const done = Math.min(migrated, legacy);
  const left = Math.max(0, legacy - migrated);
  const pct = (n: number) => (legacy > 0 ? `${(n / legacy) * 100}%` : "0%");

  return (
    <Section
      id="migration-title"
      title="Migration"
      description="Canvases made before Canvas Cloud stay on this server until they’re migrated. Migrating copies them with their ids, members, invites and share links; the originals are kept for rollback."
    >
      {legacy === 0 ? (
        <p className="rounded-lg border px-3.5 py-3 text-pretty text-muted-foreground">
          This server has no canvases from before Canvas Cloud. Nothing to migrate.
        </p>
      ) : (
        <div className="grid gap-4">
          <div className="grid gap-2.5 rounded-lg border px-3.5 py-3">
            <p className="flex flex-wrap items-baseline gap-x-1.5">
              <span>
                <span className="font-medium tabular-nums">{formatCount(done)}</span> of{" "}
                {plural(legacy, "legacy canvas", "legacy canvases")} migrated
              </span>
              <span className="text-faint" aria-hidden="true">
                ·
              </span>
              <span
                className={cn(
                  "tabular-nums",
                  failed > 0 ? "font-medium text-destructive" : "text-muted-foreground",
                )}
              >
                {formatCount(failed)} failed
              </span>
            </p>
            <div
              className="flex h-1.5 overflow-hidden rounded-full bg-foreground/10"
              aria-hidden="true"
            >
              <div className="bg-success" style={{ width: pct(done) }} />
              <div
                className="bg-destructive"
                style={{ width: pct(Math.min(failed, left)) }}
              />
            </div>
          </div>

          {left === 0 ? (
            <p className="flex items-start gap-1.5 text-pretty text-muted-foreground">
              <CircleCheckIcon
                className="mt-0.5 size-3.5 shrink-0 text-success"
                aria-hidden="true"
              />
              All migrated. The originals stay in this server&rsquo;s database in case you
              need to roll back.
            </p>
          ) : (
            <div className="grid gap-1.5">
              <p className="text-pretty text-muted-foreground">
                {data.linked ? (
                  <>
                    {plural(left, "canvas is", "canvases are")} still only on this server.
                    Run this on the server to copy {left === 1 ? "it" : "them"} over.
                    It&rsquo;s safe to re-run: migrated canvases are skipped
                    {failed > 0 && " and failed ones are retried"}.
                  </>
                ) : (
                  <>
                    {plural(left, "canvas is", "canvases are")} still only on this server.
                    Link a Canvas Cloud first, then run this on the server to copy{" "}
                    {left === 1 ? "it" : "them"} over:
                  </>
                )}
              </p>
              <Command value={MIGRATE_COMMAND} />
            </div>
          )}
        </div>
      )}
    </Section>
  );
}

function KeySection({ link, data }: { link: Link; data: CanvasLink }) {
  return (
    <Section
      id="key-title"
      title="App key"
      description={
        <>
          Kept sealed on this server and never shown again. Replace it after rotating it
          on the Canvas Cloud (
          <code className="font-mono text-xs text-foreground">
            canvas-cloud apps rotate
          </code>
          ), or to move to a new address.
        </>
      }
    >
      <LinkForm mode="replace" link={link} data={data} />
    </Section>
  );
}

// Not linked ----------------------------------------------------------------

function NotLinkedSection() {
  return (
    <Section
      id="connection-title"
      title="Connection"
      description="Copper Cloud signs in to the Canvas Cloud with its app key and acts for whoever is signed in to Copper."
    >
      <div className="flex items-start gap-3 rounded-lg border px-3.5 py-3">
        <div className="flex size-8 shrink-0 items-center justify-center rounded-md border bg-well">
          <CloudOffIcon className="size-4 text-muted-foreground" aria-hidden="true" />
        </div>
        <div className="min-w-0">
          <p className="font-medium">No Canvas Cloud linked</p>
          <p className="mt-0.5 text-pretty text-muted-foreground">
            Copper users can&rsquo;t open canvases until you link one. Nothing is deleted
            in the meantime.
          </p>
        </div>
      </div>
    </Section>
  );
}

// Link / replace form ---------------------------------------------------------

const LOOPBACK = /^(localhost|127(?:\.\d{1,3}){3}|\[::1\])$/i;

type FieldKey = "url" | "key" | "fingerprint";
type Errors = Partial<Record<FieldKey, string>>;

function checkUrl(raw: string): { value?: string; error?: string } {
  const typed = raw.trim();
  if (!typed) return { error: "Enter the Canvas Cloud’s address." };
  const withScheme = /^[a-z][a-z\d+.-]*:\/\//i.test(typed) ? typed : `https://${typed}`;
  let url: URL;
  try {
    url = new URL(withScheme);
  } catch {
    return { error: "Use a full address, like https://canvas.example.com." };
  }
  if (url.protocol !== "https:" && url.protocol !== "http:") {
    return { error: "Use an https:// address." };
  }
  if (url.protocol === "http:" && !LOOPBACK.test(url.hostname)) {
    return { error: "Plain http:// only works for localhost. Use https://." };
  }
  return { value: withScheme.replace(/\/+$/, "") };
}

function checkKey(raw: string): { value?: string; error?: string } {
  const key = raw.trim();
  if (!key) return { error: "Paste the app key from the Canvas Cloud." };
  if (/\s/.test(key)) return { error: "App keys have no spaces." };
  if (key.length < 8) return { error: "That’s too short to be an app key." };
  return { value: key };
}

function checkFingerprint(raw: string): { value?: string; error?: string } {
  const hex = raw.replace(/[\s:]/g, "").toLowerCase();
  if (!hex) return { value: "" };
  if (!/^[0-9a-f]{64}$/.test(hex)) {
    return { error: "Use the SHA-256 fingerprint: 64 hex characters, colons optional." };
  }
  return { value: hex };
}

interface FieldProps {
  id: string;
  label: string;
  optional?: boolean;
  value: string;
  onChange: (v: string) => void;
  placeholder?: string;
  hint?: React.ReactNode;
  error?: string;
  secret?: boolean;
  mono?: boolean;
  type?: "text" | "url";
  inputRef: React.Ref<HTMLInputElement>;
}

function Field({
  id,
  label,
  optional,
  value,
  onChange,
  placeholder,
  hint,
  error,
  secret,
  mono,
  type = "text",
  inputRef,
}: FieldProps) {
  return (
    <div className="grid content-start gap-1.5">
      <Label htmlFor={id}>
        {label}
        {optional && (
          <>
            {" "}
            <span className="font-normal text-muted-foreground">Optional</span>
          </>
        )}
      </Label>
      <Input
        ref={inputRef}
        id={id}
        type={secret ? "password" : type}
        inputMode={type === "url" ? "url" : undefined}
        value={value}
        onChange={(e) => onChange(e.target.value)}
        placeholder={placeholder}
        autoComplete="off"
        autoCapitalize="off"
        spellCheck={false}
        className={cn((secret || mono) && "font-mono placeholder:font-sans")}
        aria-invalid={error ? true : undefined}
        aria-describedby={error || hint ? `${id}-hint` : undefined}
      />
      {(error || hint) && (
        <p
          id={`${id}-hint`}
          className={cn(
            "text-xs text-pretty",
            error ? "text-destructive" : "text-muted-foreground",
          )}
        >
          {error ?? hint}
        </p>
      )}
    </div>
  );
}

function StoredKey() {
  return (
    <div className="flex h-9 items-center justify-between gap-3 rounded-md border bg-well px-3 text-sm">
      <span className="text-muted-foreground">Stored key</span>
      <span className="font-mono text-xs text-foreground" aria-label="Stored, hidden">
        ••••••••••••
      </span>
    </div>
  );
}

function LinkForm({
  mode,
  link,
  data,
}: {
  mode: "link" | "replace";
  link: Link;
  data: CanvasLink;
}) {
  const { refreshOverview } = useSession();
  const replacing = mode === "replace";
  const [url, setUrl] = useState(replacing ? (data.url ?? "") : "");
  const [key, setKey] = useState("");
  const [fingerprint, setFingerprint] = useState(
    replacing ? (data.tls_fingerprint ?? "") : "",
  );
  const [errors, setErrors] = useState<Errors>({});
  const [formError, setFormError] = useState<string>();
  const [pending, setPending] = useState(false);
  const ids = { url: useId(), key: useId(), fingerprint: useId() };
  const refs = {
    url: useRef<HTMLInputElement>(null),
    key: useRef<HTMLInputElement>(null),
    fingerprint: useRef<HTMLInputElement>(null),
  };

  async function submit(event: React.FormEvent) {
    event.preventDefault();
    if (pending) return;
    const u = checkUrl(url);
    const k = checkKey(key);
    const f = checkFingerprint(fingerprint);
    const next: Errors = { url: u.error, key: k.error, fingerprint: f.error };
    setErrors(next);
    setFormError(undefined);
    const first = (["url", "key", "fingerprint"] as const).find((name) => next[name]);
    if (first) {
      refs[first].current?.focus();
      return;
    }

    setPending(true);
    try {
      const saved = await api.canvasLink.set({
        url: u.value!,
        app_key: k.value!,
        ...(f.value ? { tls_fingerprint: f.value } : {}),
      });
      setKey("");
      link.mutate(() => saved);
      toast.success(
        replacing ? "Canvas Cloud link saved" : `Linked ${hostOf(saved.url ?? u.value!)}`,
      );
      void refreshOverview();
    } catch (error) {
      setFormError(errorMessage(error));
    } finally {
      setPending(false);
    }
  }

  const edit = (name: FieldKey, set: (v: string) => void) => (v: string) => {
    set(v);
    if (errors[name]) setErrors((e) => ({ ...e, [name]: undefined }));
  };

  return (
    <form onSubmit={submit} className="grid gap-4" noValidate aria-busy={pending}>
      {replacing && <StoredKey />}
      <Field
        id={ids.url}
        inputRef={refs.url}
        label="Address"
        type="url"
        value={url}
        onChange={edit("url", setUrl)}
        placeholder="https://canvas.example.com"
        error={errors.url}
        mono
      />
      <Field
        id={ids.key}
        inputRef={refs.key}
        label={replacing ? "New app key" : "App key"}
        value={key}
        onChange={edit("key", setKey)}
        placeholder="cva_…"
        secret
        error={errors.key}
        hint={
          replacing
            ? "Needed for any change here. Write-only: the server never shows a stored key again."
            : "Write-only: the server never shows a stored key again."
        }
      />
      <Field
        id={ids.fingerprint}
        inputRef={refs.fingerprint}
        label="Certificate fingerprint"
        optional
        value={fingerprint}
        onChange={edit("fingerprint", setFingerprint)}
        placeholder="9F:86:D0:81:…"
        mono
        error={errors.fingerprint}
        hint="Only for a self-signed Canvas Cloud: the SHA-256 of its certificate. Leave empty for a publicly trusted one."
      />
      {formError && (
        <div
          role="alert"
          className="flex items-start gap-2 rounded-md border border-destructive/25 bg-destructive/5 px-3 py-2 text-sm"
        >
          <CircleAlertIcon
            className="mt-0.5 size-3.5 shrink-0 text-destructive"
            aria-hidden="true"
          />
          <p className="min-w-0 text-pretty">
            <span className="font-medium">
              {replacing ? "Not saved." : "Not linked."}
            </span>{" "}
            <span className="wrap-break-word text-muted-foreground">{formError}</span>
          </p>
        </div>
      )}
      <div className="flex flex-wrap items-center gap-x-3 gap-y-2">
        <Button type="submit" disabled={pending}>
          {pending
            ? replacing
              ? "Saving…"
              : "Linking…"
            : replacing
              ? "Save"
              : "Link Canvas Cloud"}
        </Button>
        <p className="text-xs text-pretty text-muted-foreground">
          {replacing
            ? "Nothing changes unless the server accepts the new address and key."
            : "Copper Cloud checks the address and key before saving."}
        </p>
      </div>
    </form>
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
        <p className="font-medium">Couldn&rsquo;t load the Canvas Cloud link</p>
        <p className="mt-0.5 text-muted-foreground">{errorMessage(error)}</p>
        <Button variant="outline" size="sm" className="mt-3" onClick={onRetry}>
          Try again
        </Button>
      </div>
    </div>
  );
}
