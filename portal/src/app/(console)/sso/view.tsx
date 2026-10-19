"use client";

import {
  ArrowUpRightIcon,
  CheckIcon,
  ChevronRightIcon,
  CircleAlertIcon,
  CircleCheckIcon,
  CircleXIcon,
  LoaderCircleIcon,
  TriangleAlertIcon,
  XIcon,
} from "lucide-react";
import { useCallback, useEffect, useId, useRef, useState } from "react";
import { toast } from "sonner";

import { ConfirmDialog } from "@/components/confirm-dialog";
import { CopyButton } from "@/components/copy-button";
import { DomainChip, DomainInput, domainOf } from "@/components/domain-input";
import { PageHeader } from "@/components/page-header";
import { RelativeTime } from "@/components/relative-time";
import { SecretWell } from "@/components/secret-well";
import { useSession } from "@/components/session-provider";
import { Button, buttonVariants } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import { Skeleton } from "@/components/ui/skeleton";
import { Switch } from "@/components/ui/switch";
import { UnsupportedNotice } from "@/components/unsupported-notice";
import { type Resource, useResource } from "@/hooks/use-resource";
import { api, errorMessage, isUnsupported } from "@/lib/api";
import { plural } from "@/lib/format";
import type {
  SsoProvider,
  SsoProviderInput,
  SsoProviderKind,
  SsoSettings,
  SsoTestCheck,
  SsoTestResult,
} from "@/lib/types";
import { cn } from "@/lib/utils";

const GOOGLE = "google";
const OIDC = "oidc";
const GOOGLE_SUFFIX = ".apps.googleusercontent.com";
const GOOGLE_AUTH_OVERVIEW = "https://console.cloud.google.com/auth/overview";
const GOOGLE_CREATE_CLIENT = "https://console.cloud.google.com/auth/clients/create";
const LOOPBACK_REDIRECT = "http://127.0.0.1/";

type TestState =
  | { status: "running" }
  | { status: "done"; result: SsoTestResult }
  | { status: "error"; message: string };

/** Everything the sections below share: the loaded settings plus save/test. */
interface Ctl {
  data: SsoSettings;
  resource: Resource<SsoSettings>;
  tests: Record<string, TestState | undefined>;
  /** PUT the provider, reload the settings, then run its test. Throws if the save fails. */
  save: (providerId: string, input: SsoProviderInput) => Promise<void>;
  test: (providerId: string) => Promise<void>;
  /** The signed-in admin's email domain, to prefill allowed domains. */
  adminDomain: string | undefined;
}

export function SsoView() {
  const sso = useResource("sso", (signal) => api.sso.get(signal));
  const { me } = useSession();
  const [tests, setTests] = useState<Record<string, TestState | undefined>>({});
  // Keeps the walkthrough on screen after its first save, through test and share.
  const [walkthrough, setWalkthrough] = useState(false);
  const { reload } = sso;

  const test = useCallback(async (providerId: string) => {
    setTests((t) => ({ ...t, [providerId]: { status: "running" } }));
    try {
      const result = await api.sso.testProvider(providerId);
      setTests((t) => ({ ...t, [providerId]: { status: "done", result } }));
    } catch (error) {
      setTests((t) => ({
        ...t,
        [providerId]: { status: "error", message: errorMessage(error) },
      }));
    }
  }, []);

  const save = useCallback(
    async (providerId: string, input: SsoProviderInput) => {
      await api.sso.saveProvider(providerId, input);
      await reload();
      void test(providerId);
    },
    [reload, test],
  );

  const data = sso.data;
  const ctl: Ctl | undefined = data && {
    data,
    resource: sso,
    tests,
    save,
    test,
    adminDomain: domainOf(me?.admin.email),
  };

  return (
    <>
      <PageHeader
        title="Single sign-on"
        description="Let people join with their company Google account, with no access keys or passwords to hand out. Set it up once, then share one link."
      />
      {!data && isUnsupported(sso.error) ? (
        <UnsupportedNotice feature="single sign-on" />
      ) : sso.error && !data ? (
        <LoadError error={sso.error} onRetry={sso.reload} />
      ) : !ctl ? (
        <div className="grid gap-3 py-2" aria-busy="true">
          <Skeleton className="h-[74px] w-full rounded-lg" />
          <Skeleton className="h-48 w-full rounded-lg" />
          <Skeleton className="h-32 w-full rounded-lg" />
        </div>
      ) : ctl.data.providers.length === 0 || walkthrough ? (
        <>
          <GoogleWalkthrough
            ctl={ctl}
            onStart={() => setWalkthrough(true)}
            onFinish={() => setWalkthrough(false)}
          />
          {!ctl.data.providers.some((p) => p.id === OIDC) && (
            <Section
              divided
              id="oidc-title"
              title="Not on Google Workspace?"
              description="Any OpenID Connect provider works the same way."
            >
              <OidcDisclosure ctl={ctl} />
            </Section>
          )}
        </>
      ) : (
        <Configured ctl={ctl} />
      )}
    </>
  );
}

// Layout ---------------------------------------------------------------------

function Section({
  id,
  title,
  description,
  divided,
  children,
}: {
  id: string;
  title: string;
  description: React.ReactNode;
  /** Keep the top rule even as the first section (after the walkthrough). */
  divided?: boolean;
  children: React.ReactNode;
}) {
  return (
    <section
      aria-labelledby={id}
      className={cn(
        "grid gap-x-10 gap-y-4 border-t py-8 @3xl:grid-cols-[220px_minmax(0,1fr)]",
        !divided && "first-of-type:border-t-0 first-of-type:pt-2",
      )}
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

/** One numbered step of the walkthrough: same grid as a settings section. */
function Step({
  n,
  title,
  description,
  children,
}: {
  n: number;
  title: string;
  description: React.ReactNode;
  children: React.ReactNode;
}) {
  return (
    <li className="grid gap-x-10 gap-y-4 border-t py-8 first:border-t-0 first:pt-2 @3xl:grid-cols-[220px_minmax(0,1fr)]">
      <div className="flex items-start gap-2.5">
        <span
          aria-hidden="true"
          className="flex size-5 shrink-0 items-center justify-center rounded-full border text-[11px] font-semibold text-muted-foreground tabular-nums"
        >
          {n}
        </span>
        <div className="min-w-0">
          <h2 className="text-sm font-medium">
            <span className="sr-only">Step {n}: </span>
            {title}
          </h2>
          <div className="mt-1 text-sm text-pretty text-muted-foreground">
            {description}
          </div>
        </div>
      </div>
      <div className="min-w-0 pl-7.5 @3xl:max-w-[560px] @3xl:pl-0">{children}</div>
    </li>
  );
}

function SwitchRow({
  label,
  description,
  checked,
  disabled,
  onCheckedChange,
}: {
  label: string;
  description: React.ReactNode;
  checked: boolean;
  disabled?: boolean;
  onCheckedChange: (checked: boolean) => void;
}) {
  const id = useId();
  return (
    <div className="flex items-start justify-between gap-6 rounded-lg border px-3.5 py-3">
      <div className="grid gap-0.5">
        <Label htmlFor={id} className="leading-5">
          {label}
        </Label>
        <p id={`${id}-desc`} className="text-pretty text-muted-foreground">
          {description}
        </p>
      </div>
      <Switch
        id={id}
        checked={checked}
        disabled={disabled}
        onCheckedChange={onCheckedChange}
        aria-describedby={`${id}-desc`}
        className="mt-0.5"
      />
    </div>
  );
}

/** Button-styled external link that opens in a new tab. */
function ExternalButton({ href, children }: { href: string; children: React.ReactNode }) {
  return (
    <a
      href={href}
      target="_blank"
      rel="noopener noreferrer"
      className={buttonVariants({ variant: "outline" })}
    >
      {children}
      <ArrowUpRightIcon className="text-muted-foreground" aria-hidden="true" />
      <span className="sr-only"> (opens in a new tab)</span>
    </a>
  );
}

function WarningNote({ children }: { children: React.ReactNode }) {
  return (
    <div
      role="note"
      className="flex items-start gap-2.5 rounded-md border border-warning/40 bg-warning-surface px-3 py-2.5 text-sm text-warning-foreground"
    >
      <TriangleAlertIcon
        className="mt-0.5 size-4 shrink-0 text-warning"
        aria-hidden="true"
      />
      <p className="text-pretty">{children}</p>
    </div>
  );
}

// Provider form state ----------------------------------------------------------

interface DraftErrors {
  name?: string;
  issuer?: string;
  clientId?: string;
  secret?: string;
  domains?: string;
  form?: string;
}

type Draft = ReturnType<typeof useProviderDraft>;

/** Field state + validation for one provider, shared by the walkthrough and the editors. */
function useProviderDraft(
  kind: SsoProviderKind,
  provider: SsoProvider | undefined,
  adminDomain: string | undefined,
) {
  const [name, setName] = useState(provider?.name ?? (kind === GOOGLE ? "Google" : ""));
  const [issuer, setIssuer] = useState(provider?.issuer ?? "");
  const [clientId, setClientId] = useState(provider?.client_id ?? "");
  const [secret, setSecret] = useState("");
  const [domains, setDomainsState] = useState<string[]>(
    provider?.domains ?? (adminDomain ? [adminDomain] : []),
  );
  const [autoCreate, setAutoCreate] = useState(provider?.auto_create ?? true);
  const [errors, setErrors] = useState<DraftErrors>({});
  const [typedDomainError, setTypedDomainError] = useState<string>();
  const touchedDomains = useRef(!!provider);
  const ids = {
    name: useId(),
    issuer: useId(),
    clientId: useId(),
    secret: useId(),
    domains: useId(),
  };

  // The admin's own domain is a good first guess once `me` has loaded.
  useEffect(() => {
    if (!touchedDomains.current && adminDomain) {
      setDomainsState((d) => (d.length ? d : [adminDomain]));
    }
  }, [adminDomain]);

  function setDomains(next: string[]) {
    touchedDomains.current = true;
    setDomainsState(next);
    setErrors((e) => ({ ...e, domains: undefined }));
  }

  /** The PUT body, or `null` (with errors shown and the first bad field focused). */
  function validate(hasSecret: boolean): SsoProviderInput | null {
    const e: DraftErrors = {};
    const id = clientId.trim();
    const sec = secret.trim();
    if (kind === OIDC) {
      if (!name.trim()) e.name = "Name it. People see “Continue with” and this name.";
      if (!/^https:\/\/[^\s/]+/.test(issuer.trim()))
        e.issuer = "Use the issuer’s https:// URL, like https://example.okta.com.";
    }
    if (!id) e.clientId = "Paste the client ID.";
    else if (kind === GOOGLE && !id.endsWith(GOOGLE_SUFFIX))
      e.clientId = `Google client IDs end in ${GOOGLE_SUFFIX}. Copy the Client ID field.`;
    if (!sec && !hasSecret && kind === GOOGLE) e.secret = "Paste the client secret.";
    else if (/\s/.test(sec)) e.secret = "Secrets have no spaces. Copy it again.";
    if (typedDomainError) e.domains = typedDomainError;
    else if (domains.length === 0)
      e.domains = "Add at least one domain, like example.com.";
    setErrors(e);

    const first = (["name", "issuer", "clientId", "secret", "domains"] as const).find(
      (k) => e[k],
    );
    if (first) {
      document.getElementById(ids[first])?.focus();
      return null;
    }
    return {
      kind,
      name: kind === GOOGLE ? (provider?.name ?? "Google") : name.trim(),
      client_id: id,
      client_secret: sec || undefined,
      issuer: kind === OIDC ? issuer.trim().replace(/\/+$/, "") : undefined,
      domains,
      auto_create: autoCreate,
      enabled: provider?.enabled ?? true,
    };
  }

  return {
    kind,
    ids,
    name,
    setName,
    issuer,
    setIssuer,
    clientId,
    setClientId,
    secret,
    setSecret,
    domains,
    setDomains,
    autoCreate,
    setAutoCreate,
    errors,
    setErrors,
    setTypedDomainError,
    validate,
  };
}

function TextField({
  id,
  label,
  value,
  onChange,
  placeholder,
  hint,
  error,
  secret,
  mono,
}: {
  id: string;
  label: string;
  value: string;
  onChange: (value: string) => void;
  placeholder?: string;
  hint?: React.ReactNode;
  error?: string;
  secret?: boolean;
  mono?: boolean;
}) {
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
        autoCapitalize="none"
        spellCheck={false}
        className={cn((secret || mono) && "font-mono placeholder:font-sans")}
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

/** Name + issuer (OIDC only), client ID and secret. */
function CredentialFields({ draft, hasSecret }: { draft: Draft; hasSecret: boolean }) {
  const clear = (key: keyof DraftErrors) =>
    draft.errors[key] && draft.setErrors((e) => ({ ...e, [key]: undefined }));
  return (
    <div className="grid gap-4">
      {draft.kind === OIDC && (
        <div className="grid gap-4 @xl:grid-cols-[180px_minmax(0,1fr)]">
          <TextField
            id={draft.ids.name}
            label="Name"
            value={draft.name}
            onChange={(v) => {
              draft.setName(v);
              clear("name");
            }}
            placeholder="Okta"
            hint="Shown on the button."
            error={draft.errors.name}
          />
          <TextField
            id={draft.ids.issuer}
            label="Issuer URL"
            value={draft.issuer}
            onChange={(v) => {
              draft.setIssuer(v);
              clear("issuer");
            }}
            placeholder="https://example.okta.com"
            hint="Copper Cloud reads its /.well-known/openid-configuration."
            error={draft.errors.issuer}
            mono
          />
        </div>
      )}
      <TextField
        id={draft.ids.clientId}
        label="Client ID"
        value={draft.clientId}
        onChange={(v) => {
          draft.setClientId(v);
          clear("clientId");
        }}
        placeholder={
          draft.kind === GOOGLE
            ? `1234567890-abc123${GOOGLE_SUFFIX}`
            : "0oa1b2c3d4e5f6g7h8"
        }
        error={draft.errors.clientId}
        mono
      />
      <TextField
        id={draft.ids.secret}
        label="Client secret"
        value={draft.secret}
        onChange={(v) => {
          draft.setSecret(v);
          clear("secret");
        }}
        placeholder={
          hasSecret
            ? "Leave blank to keep the stored secret"
            : draft.kind === GOOGLE
              ? "GOCSPX-…"
              : "None for a public client"
        }
        hint={
          hasSecret
            ? "Stored. Paste a new one only to replace it."
            : draft.kind === OIDC
              ? "Optional: leave blank for a public client. Write-only once saved."
              : "Write-only: this server never shows it again."
        }
        error={draft.errors.secret}
        secret
      />
    </div>
  );
}

/** Allowed domains + auto-create. */
function AccessFields({ draft }: { draft: Draft }) {
  return (
    <div className="grid gap-4">
      <div className="grid content-start gap-1.5">
        <Label htmlFor={draft.ids.domains}>Allowed email domains</Label>
        <DomainInput
          id={draft.ids.domains}
          label="Allowed email domains"
          value={draft.domains}
          onChange={draft.setDomains}
          onInvalid={(message) => {
            draft.setTypedDomainError(message);
            draft.setErrors((e) => ({ ...e, domains: message }));
          }}
          invalid={!!draft.errors.domains}
          describedBy={`${draft.ids.domains}-hint`}
        />
        <p
          id={`${draft.ids.domains}-hint`}
          className={cn(
            "text-xs",
            draft.errors.domains ? "text-destructive" : "text-muted-foreground",
          )}
        >
          {draft.errors.domains ??
            "Press Enter after each domain. Only verified emails at these domains get in."}
        </p>
      </div>
      <SwitchRow
        label="Create accounts automatically"
        description={
          draft.autoCreate
            ? "Anyone at these domains can join with the onboarding link."
            : "Off. Only people who already have an account can sign in this way."
        }
        checked={draft.autoCreate}
        onCheckedChange={draft.setAutoCreate}
      />
    </div>
  );
}

function FormError({ message }: { message: string | undefined }) {
  if (!message) return null;
  return (
    <p role="alert" className="flex items-start gap-1.5 text-sm text-destructive">
      <CircleAlertIcon className="mt-0.5 size-3.5 shrink-0" aria-hidden="true" />
      {message}
    </p>
  );
}

// Walkthrough ----------------------------------------------------------------

function GoogleWalkthrough({
  ctl,
  onStart,
  onFinish,
}: {
  ctl: Ctl;
  onStart: () => void;
  onFinish: () => void;
}) {
  const existing = ctl.data.providers.find((p) => p.id === GOOGLE);
  const draft = useProviderDraft(GOOGLE, existing, ctl.adminDomain);
  const [pending, setPending] = useState(false);
  const testState = ctl.tests[GOOGLE];
  const saved = !!existing;

  async function submit(event: React.FormEvent) {
    event.preventDefault();
    if (pending) return;
    const input = draft.validate(existing?.has_secret ?? false);
    if (!input) return;
    setPending(true);
    onStart();
    try {
      await ctl.save(GOOGLE, input);
      draft.setSecret("");
      toast.success("Google sign-in saved");
    } catch (error) {
      draft.setErrors((e) => ({ ...e, form: `Couldn't save. ${errorMessage(error)}` }));
    } finally {
      setPending(false);
    }
  }

  return (
    <form onSubmit={submit} noValidate>
      <ol aria-label="Set up sign-in with Google">
        <Step
          n={1}
          title="Open Google Auth Platform"
          description="In the Google Cloud project your company already uses. You need to be a Workspace admin or the project’s owner."
        >
          <div className="grid gap-3">
            <div>
              <ExternalButton href={GOOGLE_AUTH_OVERVIEW}>
                Open Google Auth Platform
              </ExternalButton>
            </div>
            <ul className="grid gap-2 text-sm">
              <Bullet>
                If Google asks, click <Em>Get started</Em>, name the app{" "}
                <Em>Copper Cloud</Em> and use your own email for support.
              </Bullet>
              <Bullet>
                For <Em>Audience</Em>, choose <Em>Internal</Em>. Only accounts in your
                Workspace can sign in, and Google skips app review.
              </Bullet>
            </ul>
          </div>
        </Step>

        <Step
          n={2}
          title="Create an OAuth client"
          description="One client for the whole company. Nothing to install."
        >
          <div className="grid gap-3">
            <div>
              <ExternalButton href={GOOGLE_CREATE_CLIENT}>Create a client</ExternalButton>
            </div>
            <dl className="rounded-lg border py-1 text-sm">
              <SpecRow label="Application type">
                <span className="font-medium">Desktop app</span>
              </SpecRow>
              <SpecRow label="Name">
                <span className="flex items-center gap-1">
                  <span className="font-medium">Copper Cloud</span>
                  <CopyButton
                    value="Copper Cloud"
                    label="client name"
                    className="-my-1"
                  />
                </span>
              </SpecRow>
              <SpecRow label="Redirect URIs">
                <span className="text-muted-foreground">None needed</span>
              </SpecRow>
            </dl>
            <p className="text-pretty text-muted-foreground">
              <span className="text-foreground">Why Desktop app:</span> Copper signs
              people in through their own browser and a callback on their own computer, so
              it works at any server address and the client secret never leaves this
              server.
            </p>
          </div>
        </Step>

        <Step
          n={3}
          title="Paste the client ID and secret"
          description="Google shows both right after you click Create. Copy the secret while it’s on screen."
        >
          <CredentialFields draft={draft} hasSecret={existing?.has_secret ?? false} />
        </Step>

        <Step
          n={4}
          title="Choose who can join"
          description="Only Google accounts at these domains get in. That check is the gate, so the onboarding link is safe to share."
        >
          <AccessFields draft={draft} />
        </Step>

        <Step
          n={5}
          title="Save and test"
          description="Copper Cloud stores the settings, then asks Google whether the client ID and secret work."
        >
          <div className="grid gap-4">
            <div className="flex items-center gap-2">
              <Button type="submit" disabled={pending || testState?.status === "running"}>
                {pending ? "Saving…" : saved ? "Save and test again" : "Save and test"}
              </Button>
            </div>
            <FormError message={draft.errors.form} />
            <TestPanel state={testState} kind={GOOGLE} />
          </div>
        </Step>

        <Step
          n={6}
          title="Share the onboarding link"
          description="One link for everyone. Post it wherever your team will see it."
        >
          {saved ? (
            <div className="grid gap-4">
              <OnboardingLinks data={ctl.data} />
              <div>
                <Button type="button" variant="outline" onClick={onFinish}>
                  <CheckIcon aria-hidden="true" />
                  Finish setup
                </Button>
              </div>
            </div>
          ) : (
            <p className="rounded-md border border-dashed px-3 py-2.5 text-sm text-muted-foreground">
              Your team&rsquo;s link appears here once you save.
            </p>
          )}
        </Step>
      </ol>
    </form>
  );
}

function Bullet({ children }: { children: React.ReactNode }) {
  return (
    <li className="flex gap-2.5 text-pretty text-muted-foreground">
      <span
        aria-hidden="true"
        className="mt-[7px] size-1.5 shrink-0 rounded-full bg-foreground/25"
      />
      <span>{children}</span>
    </li>
  );
}

const Em = ({ children }: { children: React.ReactNode }) => (
  <span className="font-medium text-foreground">{children}</span>
);

function SpecRow({ label, children }: { label: string; children: React.ReactNode }) {
  return (
    <div className="grid grid-cols-[128px_minmax(0,1fr)] items-center gap-3 px-3.5 py-1.5">
      <dt className="text-muted-foreground">{label}</dt>
      <dd className="min-w-0">{children}</dd>
    </div>
  );
}

// Test results -----------------------------------------------------------------

/** Turns `token_endpoint` into "Token endpoint"; leaves human labels alone. */
function checkLabel(name: string): string {
  if (/\s/.test(name) || /[A-Z]/.test(name)) return name;
  const s = name
    .replace(/[._-]+/g, " ")
    .trim()
    .replace(/\b(id|url|uri|oidc|dns|tls)\b/g, (w) => w.toUpperCase());
  return s.charAt(0).toUpperCase() + s.slice(1);
}

/** Plain-English next step for a failed check, from the provider's error code. */
function fixFor(check: SsoTestCheck, kind: string): string | undefined {
  const text = `${check.name} ${check.detail ?? ""}`.toLowerCase();
  const where =
    kind === GOOGLE
      ? "Copy both again from Google Cloud › Clients › Copper Cloud."
      : "Copy both again from the app in your identity provider.";
  if (/invalid_client|unauthorized_client/.test(text))
    return `The client ID or secret is wrong. ${where}`;
  if (/deleted_client|disabled_client/.test(text))
    return "That client was deleted or disabled. Create a new one and paste its ID and secret.";
  if (check.name === "enabled")
    return "Sign-in with this provider is off. Turn it on above so Coppers offer it.";
  if (/redirect_uri_mismatch/.test(text))
    return kind === GOOGLE
      ? "The client isn’t a Desktop app. Create a new client with Application type: Desktop app."
      : `The app doesn’t accept loopback redirects. Add ${LOOPBACK_REDIRECT} as a redirect URI.`;
  if (check.name === "discovery" || /well-known|issuer/.test(text))
    return kind === GOOGLE
      ? "This server couldn’t reach Google. Check its outbound internet access and DNS."
      : "Copper Cloud couldn’t read the issuer’s discovery document. Check the issuer URL and that this server can reach it.";
  if (/timed? ?out|timeout|connect|dns|resolve|unreachable|network|tls/.test(text))
    return "This server couldn’t reach the provider. Check its outbound internet access.";
  if (/secret/.test(text)) return "Paste the client secret again and save.";
  return undefined;
}

function TestPanel({ state, kind }: { state: TestState | undefined; kind: string }) {
  if (!state) return null;
  if (state.status === "running") {
    return (
      <div
        role="status"
        className="flex items-center gap-2.5 rounded-lg border px-3.5 py-3 text-sm text-muted-foreground"
      >
        <LoaderCircleIcon
          className="size-4 shrink-0 animate-spin motion-reduce:animate-none"
          aria-hidden="true"
        />
        Testing the settings…
      </div>
    );
  }
  if (state.status === "error") {
    return (
      <div
        role="alert"
        className="flex items-start gap-2.5 rounded-lg border px-3.5 py-3 text-sm"
      >
        <CircleAlertIcon
          className="mt-0.5 size-4 shrink-0 text-destructive"
          aria-hidden="true"
        />
        <p>
          <span className="font-medium">Couldn&rsquo;t run the test.</span>{" "}
          <span className="text-muted-foreground">{state.message}</span>
        </p>
      </div>
    );
  }
  const { result } = state;
  const failed = result.checks.filter((c) => !c.ok).length;
  return (
    <div role="status" className="rounded-lg border text-sm">
      <div className="flex items-center gap-2.5 border-b px-3.5 py-2.5">
        {result.ok ? (
          <CircleCheckIcon className="size-4 shrink-0 text-success" aria-hidden="true" />
        ) : (
          <CircleXIcon className="size-4 shrink-0 text-destructive" aria-hidden="true" />
        )}
        <p className="font-medium">
          {result.ok
            ? "Everything checks out"
            : failed
              ? `${plural(failed, "check")} failed`
              : "The test failed"}
        </p>
        <span className="ml-auto text-xs text-muted-foreground tabular-nums">
          {result.checks.length - failed} of {result.checks.length} passed
        </span>
      </div>
      <ul>
        {result.checks.map((check, i) => {
          const fix = check.ok ? undefined : fixFor(check, kind);
          return (
            <li
              key={`${check.name}-${i}`}
              className="flex items-start gap-2.5 border-b border-border/70 px-3.5 py-2.5 last:border-0"
            >
              {check.ok ? (
                <CheckIcon
                  className="mt-0.5 size-4 shrink-0 text-success"
                  aria-hidden="true"
                />
              ) : (
                <XIcon
                  className="mt-0.5 size-4 shrink-0 text-destructive"
                  aria-hidden="true"
                />
              )}
              <div className="min-w-0 flex-1">
                <p className="flex items-baseline justify-between gap-3">
                  <span className="font-medium">{checkLabel(check.name)}</span>
                  <span className="shrink-0 text-xs text-muted-foreground">
                    {check.ok ? "Passed" : "Failed"}
                  </span>
                </p>
                {check.detail && (
                  <p className="mt-0.5 text-pretty wrap-break-word text-muted-foreground">
                    {check.detail}
                  </p>
                )}
                {fix && <p className="mt-1 text-pretty">{fix}</p>}
              </div>
            </li>
          );
        })}
      </ul>
    </div>
  );
}

// Onboarding link ------------------------------------------------------------

function OnboardingLinks({ data }: { data: SsoSettings }) {
  const provider = data.providers.find((p) => p.enabled) ?? data.providers[0];
  const name = provider?.name ?? "Google";
  if (!data.start_url && !data.join_link) {
    return (
      <p className="rounded-md border bg-well px-3 py-2.5 text-sm text-muted-foreground">
        Turn on a sign-in provider above to get your team&rsquo;s link.
      </p>
    );
  }
  return (
    <div className="grid gap-3">
      {data.start_url && (
        <SecretWell
          label="Onboarding link"
          value={data.start_url}
          emphasis
          hint={
            <>
              Send this to your team: they install Copper, open the link, and click{" "}
              <span className="text-foreground">Continue with {name}</span>.{" "}
              <a
                href={data.start_url}
                target="_blank"
                rel="noopener noreferrer"
                className="inline-flex items-center gap-0.5 whitespace-nowrap text-foreground underline-offset-4 hover:underline"
              >
                Preview the page
                <ArrowUpRightIcon className="size-3" aria-hidden="true" />
                <span className="sr-only"> (opens in a new tab)</span>
              </a>
            </>
          }
        />
      )}
      {data.join_link && (
        <SecretWell
          label="Copper link code"
          value={data.join_link}
          hint="For people who already run Copper: paste it into Settings › Cloud."
        />
      )}
    </div>
  );
}

// Configured -----------------------------------------------------------------

function Configured({ ctl }: { ctl: Ctl }) {
  const { data } = ctl;
  const hasGoogle = data.providers.some((p) => p.id === GOOGLE);
  const hasOidc = data.providers.some((p) => p.id === OIDC);
  return (
    <>
      {data.providers.map((p) => (
        <ProviderSection key={p.id} ctl={ctl} provider={p} />
      ))}
      <Section
        id="link-title"
        title="Onboarding link"
        description="One link for everyone. It isn’t a secret: only accounts at your allowed domains get in."
      >
        <OnboardingLinks data={data} />
      </Section>
      <PasswordSection ctl={ctl} />
      {(!hasGoogle || !hasOidc) && (
        <Section
          id="more-title"
          title="Other providers"
          description="Offer another way in next to the one you have."
        >
          <div className="grid gap-2">
            {!hasGoogle && (
              <Disclosure
                title="Google Workspace"
                description="Desktop app client from Google Auth Platform."
              >
                <div className="grid gap-4">
                  <p className="text-pretty text-muted-foreground">
                    In{" "}
                    <a
                      href={GOOGLE_CREATE_CLIENT}
                      target="_blank"
                      rel="noopener noreferrer"
                      className="text-foreground underline underline-offset-4"
                    >
                      Google Auth Platform › Clients
                    </a>
                    , create a client with Application type{" "}
                    <span className="text-foreground">Desktop app</span> named{" "}
                    <span className="text-foreground">Copper Cloud</span>, then paste its
                    details here.
                  </p>
                  <ProviderEditor ctl={ctl} kind={GOOGLE} />
                </div>
              </Disclosure>
            )}
            {!hasOidc && <OidcDisclosure ctl={ctl} />}
          </div>
        </Section>
      )}
    </>
  );
}

function maskClientId(clientId: string) {
  const suffix = clientId.endsWith(GOOGLE_SUFFIX) ? GOOGLE_SUFFIX : "";
  const core = suffix ? clientId.slice(0, -suffix.length) : clientId;
  return { tail: `••••${core.slice(-6)}`, suffix };
}

const hostOf = (url: string | null) => {
  try {
    return url ? new URL(url).host : "";
  } catch {
    return url ?? "";
  }
};

function inputOf(
  p: SsoProvider,
  patch: Partial<SsoProviderInput> = {},
): SsoProviderInput {
  return {
    kind: (p.kind === OIDC ? OIDC : GOOGLE) as SsoProviderKind,
    name: p.name,
    client_id: p.client_id,
    issuer: p.issuer ?? undefined,
    domains: p.domains,
    auto_create: p.auto_create,
    enabled: p.enabled,
    ...patch,
  };
}

function ProviderSection({ ctl, provider }: { ctl: Ctl; provider: SsoProvider }) {
  const [editing, setEditing] = useState(false);
  const [removing, setRemoving] = useState(false);
  const [toggling, setToggling] = useState(false);
  const testState = ctl.tests[provider.id];
  const known = provider.kind === GOOGLE || provider.kind === OIDC;
  const { tail, suffix } = maskClientId(provider.client_id);
  const titleId = `provider-${provider.id}-title`;

  async function toggle(enabled: boolean) {
    const { mutate } = ctl.resource;
    const flip = (on: boolean) =>
      mutate((s) =>
        s
          ? {
              ...s,
              providers: s.providers.map((p) =>
                p.id === provider.id ? { ...p, enabled: on } : p,
              ),
            }
          : s,
      );
    setToggling(true);
    flip(enabled);
    try {
      await api.sso.saveProvider(provider.id, inputOf(provider, { enabled }));
      await ctl.resource.reload();
      toast.success(
        enabled
          ? `Sign-in with ${provider.name} turned on`
          : `Sign-in with ${provider.name} turned off`,
      );
    } catch (error) {
      flip(!enabled);
      toast.error(`Couldn't save. ${errorMessage(error)}`);
    } finally {
      setToggling(false);
    }
  }

  return (
    <Section
      id={titleId}
      title={provider.name}
      description={
        provider.kind === GOOGLE
          ? "People sign in with their Google Workspace account."
          : `People sign in through ${hostOf(provider.issuer) || "your identity provider"}.`
      }
    >
      <div className="grid gap-4">
        <SwitchRow
          label={`Allow sign-in with ${provider.name}`}
          description={
            provider.enabled
              ? `On. Copper shows “Continue with ${provider.name}”.`
              : `Off. Nobody can sign in with ${provider.name} until you turn this back on.`
          }
          checked={provider.enabled}
          disabled={toggling}
          onCheckedChange={(on) => void toggle(on)}
        />

        {editing ? (
          <ProviderEditor
            ctl={ctl}
            kind={provider.kind === OIDC ? OIDC : GOOGLE}
            provider={provider}
            onClose={() => setEditing(false)}
          />
        ) : (
          <>
            <dl className="rounded-lg border py-1.5 text-sm">
              <SpecRow label="Client ID">
                <code
                  className="block truncate font-mono text-[12.5px]"
                  aria-label={`Client ID ending in ${tail.replace(/•/g, "")}`}
                >
                  {tail}
                  <span className="text-faint">{suffix}</span>
                </code>
              </SpecRow>
              <SpecRow label="Client secret">
                {provider.has_secret ? (
                  "Stored"
                ) : (
                  <span className="text-warning-foreground">
                    Missing. Edit to add it.
                  </span>
                )}
              </SpecRow>
              {provider.issuer && (
                <SpecRow label="Issuer">
                  <code
                    className="block truncate font-mono text-[12.5px]"
                    title={provider.issuer}
                  >
                    {provider.issuer}
                  </code>
                </SpecRow>
              )}
              <SpecRow label="Domains">
                {provider.domains.length ? (
                  <span className="flex flex-wrap gap-1 py-0.5">
                    {provider.domains.map((d) => (
                      <DomainChip key={d}>{d}</DomainChip>
                    ))}
                  </span>
                ) : (
                  <span className="text-warning-foreground">
                    None. Nobody can get in.
                  </span>
                )}
              </SpecRow>
              <SpecRow label="New people">
                {provider.auto_create
                  ? "Get an account on first sign-in"
                  : "Need an existing account"}
              </SpecRow>
              {provider.updated_at && (
                <SpecRow label="Last changed">
                  <RelativeTime value={provider.updated_at} />
                </SpecRow>
              )}
            </dl>
            <div className="flex flex-wrap items-center gap-2">
              <Button
                variant="outline"
                onClick={() => void ctl.test(provider.id)}
                disabled={testState?.status === "running"}
              >
                {testState?.status === "running" ? "Testing…" : "Test"}
              </Button>
              {known && (
                <Button variant="outline" onClick={() => setEditing(true)}>
                  Edit
                </Button>
              )}
              <Button variant="outline" onClick={() => setRemoving(true)}>
                Remove
              </Button>
            </div>
          </>
        )}
        <TestPanel state={testState} kind={provider.kind} />
      </div>

      <ConfirmDialog
        open={removing}
        onOpenChange={setRemoving}
        title={`Remove sign-in with ${provider.name}?`}
        description={`Nobody can sign in with ${provider.name} and the onboarding link stops working. Coppers already signed in stay signed in; people without a password can't sign in on a new Copper until you set it up again.`}
        confirmLabel="Remove"
        pendingLabel="Removing…"
        onConfirm={async () => {
          await api.sso.removeProvider(provider.id);
          await ctl.resource.reload();
          toast.success(`Removed sign-in with ${provider.name}`);
        }}
      />
    </Section>
  );
}

/** Inline form to add a provider or edit one; saving runs the test. */
function ProviderEditor({
  ctl,
  kind,
  provider,
  onClose,
}: {
  ctl: Ctl;
  kind: SsoProviderKind;
  provider?: SsoProvider;
  onClose?: () => void;
}) {
  const draft = useProviderDraft(kind, provider, ctl.adminDomain);
  const [pending, setPending] = useState(false);
  const providerId = provider?.id ?? kind;

  async function submit(event: React.FormEvent) {
    event.preventDefault();
    if (pending) return;
    const input = draft.validate(provider?.has_secret ?? false);
    if (!input) return;
    setPending(true);
    try {
      await ctl.save(providerId, input);
      toast.success(`${input.name} sign-in saved`);
      draft.setSecret("");
      onClose?.();
    } catch (error) {
      draft.setErrors((e) => ({ ...e, form: `Couldn't save. ${errorMessage(error)}` }));
    } finally {
      setPending(false);
    }
  }

  return (
    <form onSubmit={submit} className="grid gap-4" noValidate>
      <CredentialFields draft={draft} hasSecret={provider?.has_secret ?? false} />
      <AccessFields draft={draft} />
      <FormError message={draft.errors.form} />
      <div className="flex items-center gap-2">
        <Button type="submit" disabled={pending}>
          {pending ? "Saving…" : "Save and test"}
        </Button>
        {onClose && (
          <Button type="button" variant="outline" onClick={onClose} disabled={pending}>
            Cancel
          </Button>
        )}
      </div>
    </form>
  );
}

function Disclosure({
  title,
  description,
  children,
}: {
  title: string;
  description: string;
  children: React.ReactNode;
}) {
  const [open, setOpen] = useState(false);
  const panelId = useId();
  return (
    <div className="rounded-lg border">
      <button
        type="button"
        aria-expanded={open}
        aria-controls={panelId}
        onClick={() => setOpen((o) => !o)}
        className={cn(
          "flex w-full items-start gap-2.5 rounded-lg px-3.5 py-3 text-left text-sm transition-colors outline-none hover:bg-muted/40 focus-visible:ring-2 focus-visible:ring-ring",
          open && "rounded-b-none",
        )}
      >
        <ChevronRightIcon
          className={cn(
            "mt-0.5 size-4 shrink-0 text-faint transition-transform duration-150 motion-reduce:transition-none",
            open && "rotate-90",
          )}
          aria-hidden="true"
        />
        <span className="grid gap-0.5">
          <span className="font-medium">{title}</span>
          <span className="text-pretty text-muted-foreground">{description}</span>
        </span>
      </button>
      <div id={panelId} hidden={!open} className="border-t px-3.5 py-4">
        {children}
      </div>
    </div>
  );
}

function OidcDisclosure({ ctl }: { ctl: Ctl }) {
  return (
    <Disclosure
      title="Generic OIDC (Okta, Entra ID…)"
      description="Any provider with OpenID Connect discovery."
    >
      <div className="grid gap-4">
        <div className="grid gap-2">
          <p className="text-pretty text-muted-foreground">
            In your identity provider, create a native or desktop app (a public client
            with PKCE) and add this redirect URI. Copper listens on a free local port each
            time, so the provider must accept any port on it.
          </p>
          <div className="flex h-8 items-center justify-between gap-2 rounded-md border bg-well pr-1 pl-3">
            <code className="truncate font-mono text-[12.5px]">{LOOPBACK_REDIRECT}</code>
            <CopyButton value={LOOPBACK_REDIRECT} label="redirect URI" />
          </div>
        </div>
        <ProviderEditor ctl={ctl} kind={OIDC} />
      </div>
    </Disclosure>
  );
}

// Password sign-in -------------------------------------------------------------

function PasswordSection({ ctl }: { ctl: Ctl }) {
  const { data, resource } = ctl;
  const [pending, setPending] = useState(false);
  const [confirmOpen, setConfirmOpen] = useState(false);
  const on = data.providers.filter((p) => p.enabled);
  const via = on.length ? on.map((p) => p.name).join(" or ") : "single sign-on";

  async function set(allowed: boolean) {
    setPending(true);
    resource.mutate((s) => (s ? { ...s, password_signin: allowed } : s));
    try {
      await api.sso.update({ password_signin: allowed });
      await resource.reload();
      toast.success(
        allowed ? "Password sign-in turned on" : "Password sign-in turned off",
      );
    } catch (error) {
      resource.mutate((s) => (s ? { ...s, password_signin: !allowed } : s));
      toast.error(`Couldn't save. ${errorMessage(error)}`);
    } finally {
      setPending(false);
    }
  }

  return (
    <Section
      id="password-signin-title"
      title="Password sign-in"
      description="Whether people can also create an account or sign in with an email and password. Your admin sign-in here isn’t affected."
    >
      <div className="grid gap-3">
        <SwitchRow
          label="Allow password sign-in"
          description={
            data.password_signin
              ? `On. People can use ${via} or a password.`
              : `Off. People sign in with ${via} only.`
          }
          checked={data.password_signin}
          disabled={pending}
          onCheckedChange={(allowed) => (allowed ? void set(true) : setConfirmOpen(true))}
        />
        {!data.password_signin && on.length === 0 && (
          <WarningNote>
            Nobody can sign in to Copper right now. Turn on a provider above, or allow
            passwords again.
          </WarningNote>
        )}
      </div>
      <ConfirmDialog
        open={confirmOpen}
        onOpenChange={setConfirmOpen}
        destructive={false}
        title="Turn off password sign-in?"
        description={`People will sign in to Copper with ${via} only.`}
        confirmLabel="Turn off passwords"
        pendingLabel="Turning off…"
        onConfirm={() => set(false)}
      >
        <WarningNote>
          Accounts that use a password today can still get in with {via} if their email
          matches. Anyone outside your allowed domains can&rsquo;t sign in until you turn
          passwords back on.
        </WarningNote>
      </ConfirmDialog>
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
        <p className="font-medium">Couldn&rsquo;t load single sign-on</p>
        <p className="mt-0.5 text-muted-foreground">{errorMessage(error)}</p>
        <Button variant="outline" size="sm" className="mt-3" onClick={onRetry}>
          Try again
        </Button>
      </div>
    </div>
  );
}
