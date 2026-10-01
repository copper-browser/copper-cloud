"use client";

import { CircleAlertIcon } from "lucide-react";
import { useId, useState } from "react";
import { toast } from "sonner";

import { ConfirmDialog } from "@/components/confirm-dialog";
import { InstanceDetails } from "@/components/instance-details";
import { PageHeader } from "@/components/page-header";
import { RelativeTime } from "@/components/relative-time";
import { SecretWell } from "@/components/secret-well";
import { useSession } from "@/components/session-provider";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import { RadioGroup, RadioGroupItem } from "@/components/ui/radio-group";
import { Skeleton } from "@/components/ui/skeleton";
import { Switch } from "@/components/ui/switch";
import { useResource } from "@/hooks/use-resource";
import { api, ApiError, errorMessage } from "@/lib/api";
import { plural } from "@/lib/format";
import { MIN_PASSWORD_LENGTH } from "@/lib/password";
import type { AccessMode, Settings, SettingsPatch } from "@/lib/types";
import { cn } from "@/lib/utils";

export function SettingsView() {
  return (
    <>
      <PageHeader
        title="Settings"
        description="Who can get in, your admin password, and this instance."
      />
      <AccessSection />
      <PasswordSection />
      <InstanceSection />
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
        <p className="mt-1 text-sm text-pretty text-muted-foreground">{description}</p>
      </div>
      <div className="min-w-0 @3xl:max-w-[560px]">{children}</div>
    </section>
  );
}

const MODES: { value: AccessMode; title: string; body: string }[] = [
  {
    value: "directory",
    title: "Only people with an access key can create an account",
    body: "Directory mode. The shared instance key is rejected; mint one key per person on Access keys.",
  },
  {
    value: "open",
    title: "Anyone with the instance key can create an account",
    body: "Open mode. Share the instance link code below. Access keys keep working too.",
  },
];

function AccessSection() {
  const settings = useResource("settings", (signal) => api.settings(signal));
  const { refreshOverview } = useSession();
  const [saving, setSaving] = useState<keyof SettingsPatch | null>(null);
  const [confirmOpen, setConfirmOpen] = useState(false);
  const signupId = useId();
  const data = settings.data;

  async function save(patch: SettingsPatch, message: string) {
    const key = Object.keys(patch)[0] as keyof SettingsPatch;
    const previous = data;
    setSaving(key);
    settings.mutate((s) => (s ? { ...s, ...patch } : s));
    try {
      const next: Settings = await api.updateSettings(patch);
      settings.mutate(() => next);
      toast.success(message);
      void refreshOverview();
    } catch (error) {
      settings.mutate(() => previous);
      toast.error(`Couldn't save. ${errorMessage(error)}`);
    } finally {
      setSaving(null);
    }
  }

  function chooseMode(mode: AccessMode) {
    if (!data || mode === data.access_mode) return;
    if (mode === "open") setConfirmOpen(true);
    else
      void save(
        { access_mode: "directory" },
        "Directory mode on. Only access keys get in.",
      );
  }

  return (
    <Section
      id="access-title"
      title="Access"
      description="Decide how people get onto this instance. Changes reach the server's gate within a few seconds."
    >
      {settings.error && !data ? (
        <LoadError error={settings.error} onRetry={settings.reload} />
      ) : !data ? (
        <div className="grid gap-2" aria-busy="true">
          <Skeleton className="h-[74px] w-full rounded-lg" />
          <Skeleton className="h-[74px] w-full rounded-lg" />
          <Skeleton className="mt-4 h-10 w-full rounded-lg" />
        </div>
      ) : (
        <div className="grid gap-6">
          <RadioGroup
            aria-label="Access mode"
            value={data.access_mode}
            onValueChange={(v) => chooseMode(v as AccessMode)}
            disabled={saving === "access_mode"}
            className="gap-2"
          >
            {MODES.map((m) => {
              const checked = data.access_mode === m.value;
              return (
                <label
                  key={m.value}
                  className={cn(
                    "flex cursor-pointer items-start gap-3 rounded-lg border px-3.5 py-3 transition-colors hover:bg-muted/40",
                    checked && "border-primary/45 bg-primary/[0.035] hover:bg-primary/5",
                  )}
                >
                  <RadioGroupItem value={m.value} className="mt-0.5" />
                  <span className="grid gap-0.5">
                    <span className="font-medium">{m.title}</span>
                    <span className="text-pretty text-muted-foreground">{m.body}</span>
                  </span>
                </label>
              );
            })}
          </RadioGroup>

          <div
            className={cn(
              "flex items-start justify-between gap-6 rounded-lg border px-3.5 py-3",
              data.access_mode === "directory" && "bg-well",
            )}
          >
            <div className="grid gap-0.5">
              <Label htmlFor={signupId} className="leading-5">
                Allow sign-up
              </Label>
              <p className="text-pretty text-muted-foreground">
                {data.access_mode === "open"
                  ? "Let people holding the instance link code create their own account. Off means only existing accounts can sign in."
                  : "Applies in open mode only. In directory mode, holding an access key is the permission to sign up."}
              </p>
            </div>
            <Switch
              id={signupId}
              checked={data.allow_signup}
              disabled={saving === "allow_signup"}
              onCheckedChange={(checked) =>
                void save(
                  { allow_signup: checked },
                  checked ? "Sign-up turned on" : "Sign-up turned off",
                )
              }
              className="mt-0.5"
            />
          </div>

          <div className="grid gap-2">
            {data.instance_link_code ? (
              <SecretWell
                label="Instance link code"
                value={data.instance_link_code}
                concealed
                className={cn(data.access_mode === "directory" && "opacity-75")}
                hint={
                  data.access_mode === "open"
                    ? "Anyone with this code can connect a Copper. Share it like a password."
                    : "Inactive in directory mode: the gate rejects it until you switch to open."
                }
              />
            ) : (
              <p className="rounded-md border bg-well px-3 py-2.5 text-sm text-muted-foreground">
                The instance link code is unavailable because the server can&rsquo;t read
                its TLS certificate. Run{" "}
                <code className="font-mono text-xs">copper-cloud doctor</code> on the
                server.
              </p>
            )}
          </div>
        </div>
      )}

      <ConfirmDialog
        open={confirmOpen}
        onOpenChange={setConfirmOpen}
        destructive={false}
        title="Switch to open mode?"
        description={
          data?.allow_signup
            ? "Anyone holding the instance link code can connect a Copper and create an account. Access keys keep working."
            : "Anyone holding the instance link code can connect a Copper. Sign-up stays off, so only existing accounts can sign in."
        }
        confirmLabel="Switch to open"
        pendingLabel="Switching…"
        onConfirm={() =>
          save({ access_mode: "open" }, "Open mode on. The instance key works again.")
        }
      />
    </Section>
  );
}

function PasswordSection() {
  const { me } = useSession();
  const [current, setCurrent] = useState("");
  const [next, setNext] = useState("");
  const [confirm, setConfirm] = useState("");
  const [errors, setErrors] = useState<{
    current?: string;
    next?: string;
    confirm?: string;
    form?: string;
  }>({});
  const [pending, setPending] = useState(false);
  const ids = { current: useId(), next: useId(), confirm: useId(), user: useId() };

  async function submit(event: React.FormEvent) {
    event.preventDefault();
    if (pending) return;
    const e: typeof errors = {};
    if (!current) e.current = "Enter your current password.";
    if (next.length < MIN_PASSWORD_LENGTH)
      e.next = `Use at least ${MIN_PASSWORD_LENGTH} characters.`;
    else if (next === current) e.next = "Pick a password different from the current one.";
    if (confirm !== next) e.confirm = "Doesn't match the new password.";
    setErrors(e);
    if (e.current || e.next || e.confirm) return;

    setPending(true);
    try {
      const res = await api.changePassword(current, next);
      setCurrent("");
      setNext("");
      setConfirm("");
      toast.success(
        res.revoked_sessions > 0
          ? `Password changed. Signed out ${plural(res.revoked_sessions, "other session")}.`
          : "Password changed",
      );
    } catch (error) {
      if (error instanceof ApiError && error.code === "credentials") {
        setErrors({ current: "That isn't your current password." });
      } else {
        setErrors({ form: errorMessage(error) });
      }
    } finally {
      setPending(false);
    }
  }

  const field = (
    key: "current" | "next" | "confirm",
    label: string,
    value: string,
    set: (v: string) => void,
    autoComplete: string,
    hint?: string,
  ) => (
    <div className="grid content-start gap-1.5">
      <Label htmlFor={ids[key]}>{label}</Label>
      <Input
        id={ids[key]}
        type="password"
        value={value}
        onChange={(e) => set(e.target.value)}
        autoComplete={autoComplete}
        aria-invalid={errors[key] ? true : undefined}
        aria-describedby={errors[key] || hint ? `${ids[key]}-hint` : undefined}
      />
      {(errors[key] || hint) && (
        <p
          id={`${ids[key]}-hint`}
          className={cn(
            "text-xs",
            errors[key] ? "text-destructive" : "text-muted-foreground",
          )}
        >
          {errors[key] ?? hint}
        </p>
      )}
    </div>
  );

  return (
    <Section
      id="password-title"
      title="Admin password"
      description={
        me ? (
          <>
            For <span className="text-foreground">{me.admin.email}</span>. Changing it
            signs out your other sessions.
          </>
        ) : (
          "Changing it signs out your other sessions."
        )
      }
    >
      <form onSubmit={submit} className="grid gap-4" noValidate>
        {/* Lets password managers attach the new password to the right account. */}
        <input
          id={ids.user}
          type="text"
          name="username"
          autoComplete="username"
          value={me?.admin.email ?? ""}
          readOnly
          hidden
        />
        {field("current", "Current password", current, setCurrent, "current-password")}
        <div className="grid gap-4 @xl:grid-cols-2">
          {field(
            "next",
            "New password",
            next,
            setNext,
            "new-password",
            `${MIN_PASSWORD_LENGTH}+ characters`,
          )}
          {field("confirm", "Confirm new password", confirm, setConfirm, "new-password")}
        </div>
        {errors.form && (
          <p role="alert" className="flex items-start gap-1.5 text-sm text-destructive">
            <CircleAlertIcon className="mt-0.5 size-3.5 shrink-0" aria-hidden="true" />
            {errors.form}
          </p>
        )}
        <div>
          <Button type="submit" disabled={pending}>
            {pending ? "Changing…" : "Change password"}
          </Button>
        </div>
      </form>
    </Section>
  );
}

function InstanceSection() {
  const { overview, me } = useSession();
  return (
    <Section
      id="instance-title"
      title="Instance"
      description="What Coppers connect to. The fingerprint pins the self-signed certificate inside every link code."
    >
      <div className="rounded-lg border">
        <InstanceDetails overview={overview} />
      </div>
      {me && (
        <p className="mt-3 text-xs text-muted-foreground">
          Signed in as <span className="text-foreground">{me.admin.email}</span>. This
          session expires <RelativeTime value={me.session.expires_at} />.
        </p>
      )}
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
        <p className="font-medium">Couldn&rsquo;t load settings</p>
        <p className="mt-0.5 text-muted-foreground">{errorMessage(error)}</p>
        <Button variant="outline" size="sm" className="mt-3" onClick={onRetry}>
          Try again
        </Button>
      </div>
    </div>
  );
}
