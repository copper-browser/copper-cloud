"use client";

import { BotIcon, CircleAlertIcon, PlusIcon, TriangleAlertIcon } from "lucide-react";
import Link from "next/link";
import { useEffect, useId, useMemo, useState } from "react";
import { toast } from "sonner";

import { ConfirmDialog } from "@/components/confirm-dialog";
import { CopyButton } from "@/components/copy-button";
import { type Column, DataTable, EmptyState } from "@/components/data-table";
import { DomainInput } from "@/components/domain-input";
import { PageHeader } from "@/components/page-header";
import { RelativeTime } from "@/components/relative-time";
import { RowActions } from "@/components/row-actions";
import { SecretWell } from "@/components/secret-well";
import { useSession } from "@/components/session-provider";
import { StatusBadge } from "@/components/status-badge";
import { Button } from "@/components/ui/button";
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from "@/components/ui/dialog";
import { DropdownMenuItem, DropdownMenuSeparator } from "@/components/ui/dropdown-menu";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import {
  Sheet,
  SheetContent,
  SheetDescription,
  SheetFooter,
  SheetHeader,
  SheetTitle,
} from "@/components/ui/sheet";
import { Skeleton } from "@/components/ui/skeleton";
import { Switch } from "@/components/ui/switch";
import { Tooltip, TooltipContent, TooltipTrigger } from "@/components/ui/tooltip";
import { UnsupportedNotice } from "@/components/unsupported-notice";
import { useResource } from "@/hooks/use-resource";
import { api, errorMessage, isUnsupported } from "@/lib/api";
import { displayHost, formatCount, plural } from "@/lib/format";
import type { Integration } from "@/lib/types";
import { cn } from "@/lib/utils";

/** A key to show exactly once: right after create or rotate. */
interface ShownKey {
  name: string;
  key: string;
  rotated: boolean;
}

export function IntegrationsView() {
  const list = useResource("integrations", (signal) => api.integrations.list(signal));
  // Only for the default-domains hint; a server without SSO just leaves it empty.
  const sso = useResource("sso", (signal) => api.sso.get(signal));
  const [creating, setCreating] = useState(false);
  const [shown, setShown] = useState<ShownKey | null>(null);
  const [rotating, setRotating] = useState<Integration | null>(null);
  const [revoking, setRevoking] = useState<Integration | null>(null);
  const [devicesFor, setDevicesFor] = useState<Integration | null>(null);

  const ssoDomains = useMemo(() => {
    const all = (sso.data?.providers ?? [])
      .filter((p) => p.enabled)
      .flatMap((p) => p.domains);
    return [...new Set(all)];
  }, [sso.data]);

  const rows = useMemo(
    () =>
      [...(list.data ?? [])].sort(
        (a, b) =>
          Number(!!a.revoked_at) - Number(!!b.revoked_at) ||
          Date.parse(b.created_at) - Date.parse(a.created_at),
      ),
    [list.data],
  );

  const unsupported = !list.data && isUnsupported(list.error);

  const columns: Column<Integration>[] = [
    {
      id: "name",
      header: "Name",
      skeleton: "w-28",
      cell: (i) => (
        <span className="flex min-w-0 items-center gap-2.5">
          <BotIcon className="size-4 shrink-0 text-faint" aria-hidden="true" />
          <span className="truncate font-medium text-foreground" title={i.name}>
            {i.name}
          </span>
        </span>
      ),
    },
    {
      id: "domains",
      header: "Domains",
      className: "w-[24%] @max-2xl:hidden",
      skeleton: "w-28",
      cell: (i) =>
        i.domains.length ? (
          <span className="text-muted-foreground" title={i.domains.join(", ")}>
            {i.domains.join(", ")}
          </span>
        ) : (
          <Tooltip>
            <TooltipTrigger render={<span className="cursor-default text-faint" />}>
              Sign-in domains
            </TooltipTrigger>
            <TooltipContent>
              {ssoDomains.length
                ? `Follows single sign-on: ${ssoDomains.join(", ")}`
                : "Follows single sign-on, which has no domains yet"}
            </TooltipContent>
          </Tooltip>
        ),
    },
    {
      id: "create-users",
      header: "New people",
      className: "w-[120px] @max-3xl:hidden",
      skeleton: "w-16",
      cell: (i) =>
        i.create_users ? (
          <span className="text-muted-foreground">Gets an account</span>
        ) : (
          <span className="text-faint">Existing only</span>
        ),
    },
    {
      id: "devices",
      header: "Devices",
      align: "right",
      className: "w-[76px]",
      skeleton: "w-5",
      cell: (i) => (
        <button
          type="button"
          onClick={() => setDevicesFor(i)}
          className={cn(
            "rounded-sm tabular-nums underline-offset-4 outline-none hover:underline focus-visible:ring-2 focus-visible:ring-ring",
            i.device_count === 0 && "text-faint",
          )}
          aria-label={`Show ${plural(i.device_count, "device")} for ${i.name}`}
        >
          {formatCount(i.device_count)}
        </button>
      ),
    },
    {
      id: "last-used",
      header: "Last used",
      className: "w-24 @max-xl:hidden",
      skeleton: "w-12",
      cell: (i) => (
        <RelativeTime
          value={i.last_used_at}
          empty="Never"
          className="text-muted-foreground"
        />
      ),
    },
    {
      id: "status",
      header: "Status",
      className: "w-[110px] pl-4",
      skeleton: "w-14",
      cell: (i) =>
        i.revoked_at ? (
          <Tooltip>
            <TooltipTrigger render={<span className="cursor-default" />}>
              <StatusBadge tone="danger">Revoked</StatusBadge>
            </TooltipTrigger>
            <TooltipContent>
              Revoked <RelativeTime value={i.revoked_at} />. Its key is rejected.
            </TooltipContent>
          </Tooltip>
        ) : (
          <StatusBadge tone="success">Active</StatusBadge>
        ),
    },
    {
      id: "actions",
      header: "Actions",
      srHeader: true,
      align: "right",
      className: "w-12 pr-2",
      skeleton: "none",
      cell: (i) => (
        <RowActions label={`Actions for ${i.name}`}>
          <DropdownMenuItem onClick={() => setDevicesFor(i)}>
            Show devices
          </DropdownMenuItem>
          {!i.revoked_at && (
            <>
              <DropdownMenuItem onClick={() => setRotating(i)}>
                Rotate key…
              </DropdownMenuItem>
              <DropdownMenuSeparator />
              <DropdownMenuItem variant="destructive" onClick={() => setRevoking(i)}>
                Revoke…
              </DropdownMenuItem>
            </>
          )}
        </RowActions>
      ),
    },
  ];

  return (
    <>
      <PageHeader
        title="Agent access"
        description="An integration key lets a trusted service, such as a bot platform, sign its own Copper in as a person at your allowed domains. Keys can create sessions for those people, so treat them like passwords."
        action={
          unsupported ? undefined : (
            <Button onClick={() => setCreating(true)}>
              <PlusIcon aria-hidden="true" />
              New integration
            </Button>
          )
        }
      />

      {unsupported ? (
        <UnsupportedNotice feature="agent access" />
      ) : (
        <DataTable
          label="Integrations"
          columns={columns}
          rows={rows}
          rowKey={(i) => i.id}
          loading={list.loading}
          error={list.error}
          onRetry={list.reload}
          filtered={false}
          rowClassName={(i) =>
            i.revoked_at
              ? "[&_td]:text-muted-foreground [&_td_.font-medium]:text-muted-foreground"
              : undefined
          }
          empty={
            <EmptyState
              icon={BotIcon}
              title="No integrations yet"
              action={
                <Button size="sm" onClick={() => setCreating(true)}>
                  <PlusIcon aria-hidden="true" />
                  New integration
                </Button>
              }
            >
              Create one for each service that runs Copper on someone&rsquo;s behalf, like
              a bot platform giving every bot its own browser signed in as its owner.
            </EmptyState>
          }
        />
      )}

      <NewIntegrationDialog
        open={creating}
        onOpenChange={setCreating}
        ssoDomains={ssoDomains}
        onCreated={(name, key) => {
          setCreating(false);
          setShown({ name, key, rotated: false });
          void list.reload();
        }}
      />
      <KeyOnceDialog shown={shown} onClose={() => setShown(null)} />
      <DevicesSheet integration={devicesFor} onClose={() => setDevicesFor(null)} />

      <ConfirmDialog
        open={!!rotating}
        onOpenChange={(open) => !open && setRotating(null)}
        destructive={false}
        title={`Rotate the key for “${rotating?.name ?? ""}”?`}
        description="The current key stops working right away. You get a new one on the next screen; give it to the service to keep it pairing."
        confirmLabel="Rotate key"
        pendingLabel="Rotating…"
        onConfirm={async () => {
          if (!rotating) return;
          const res = await api.integrations.rotate(rotating.id);
          setShown({ name: rotating.name, key: res.key, rotated: true });
          void list.reload();
        }}
      />
      <ConfirmDialog
        open={!!revoking}
        onOpenChange={(open) => !open && setRevoking(null)}
        title={`Revoke “${revoking?.name ?? ""}”?`}
        description={`The key stops working and every Copper it paired is signed out (${plural(revoking?.device_count ?? 0, "device")}). You can’t undo this.`}
        confirmLabel="Revoke"
        pendingLabel="Revoking…"
        onConfirm={async () => {
          if (!revoking) return;
          await api.integrations.revoke(revoking.id);
          await list.reload();
          toast.success(`Revoked “${revoking.name}”`);
        }}
      />
    </>
  );
}

// ---------------------------------------------------------------------------

function NewIntegrationDialog({
  open,
  onOpenChange,
  onCreated,
  ssoDomains,
}: {
  open: boolean;
  onOpenChange: (open: boolean) => void;
  onCreated: (name: string, key: string) => void;
  ssoDomains: string[];
}) {
  const [name, setName] = useState("");
  const [domains, setDomains] = useState<string[]>([]);
  const [createUsers, setCreateUsers] = useState(true);
  const [typedDomainError, setTypedDomainError] = useState<string>();
  const [errors, setErrors] = useState<{
    name?: string;
    domains?: string;
    form?: string;
  }>({});
  const [pending, setPending] = useState(false);
  const ids = { name: useId(), domains: useId(), create: useId() };

  function reset() {
    setName("");
    setDomains([]);
    setCreateUsers(true);
    setTypedDomainError(undefined);
    setErrors({});
  }

  async function submit(event: React.FormEvent) {
    event.preventDefault();
    if (pending) return;
    const next: typeof errors = {};
    if (!name.trim())
      next.name = "Name it after the service, so you can tell keys apart.";
    if (typedDomainError) next.domains = typedDomainError;
    else if (!domains.length && !ssoDomains.length)
      next.domains = "Add at least one domain. Single sign-on has none to fall back on.";
    setErrors(next);
    if (next.name || next.domains) {
      document.getElementById(next.name ? ids.name : ids.domains)?.focus();
      return;
    }

    setPending(true);
    try {
      const created = await api.integrations.create({
        name: name.trim(),
        domains: domains.length ? domains : undefined,
        create_users: createUsers,
      });
      reset();
      onCreated(created.integration?.name ?? name.trim(), created.key);
    } catch (error) {
      setErrors({ form: errorMessage(error) });
    } finally {
      setPending(false);
    }
  }

  return (
    <Dialog
      open={open}
      onOpenChange={(next) => {
        if (pending) return;
        if (!next) reset();
        onOpenChange(next);
      }}
    >
      <DialogContent className="sm:max-w-[460px]">
        <form onSubmit={submit} className="contents" noValidate>
          <DialogHeader>
            <DialogTitle>New integration</DialogTitle>
            <DialogDescription>
              The key is shown once, right after you create it.
            </DialogDescription>
          </DialogHeader>

          <div className="grid gap-4">
            <div className="grid gap-1.5">
              <Label htmlFor={ids.name}>Name</Label>
              <Input
                id={ids.name}
                value={name}
                onChange={(e) => setName(e.target.value)}
                placeholder="Team bots"
                maxLength={200}
                autoFocus
                autoComplete="off"
                aria-invalid={errors.name ? true : undefined}
                aria-describedby={`${ids.name}-hint`}
              />
              <p
                id={`${ids.name}-hint`}
                className={cn(
                  "text-xs",
                  errors.name ? "text-destructive" : "text-muted-foreground",
                )}
              >
                {errors.name ?? "Only admins see this."}
              </p>
            </div>

            <div className="grid gap-1.5">
              <Label htmlFor={ids.domains}>
                Allowed email domains
                {ssoDomains.length > 0 && (
                  <span className="font-normal text-muted-foreground"> (optional)</span>
                )}
              </Label>
              <DomainInput
                id={ids.domains}
                label="Allowed email domains"
                value={domains}
                onChange={(d) => {
                  setDomains(d);
                  setErrors((e) => ({ ...e, domains: undefined }));
                }}
                onInvalid={(message) => {
                  setTypedDomainError(message);
                  setErrors((e) => ({ ...e, domains: message }));
                }}
                placeholder={ssoDomains[0] ?? "example.com"}
                invalid={!!errors.domains}
                describedBy={`${ids.domains}-hint`}
              />
              <p
                id={`${ids.domains}-hint`}
                className={cn(
                  "text-xs text-pretty",
                  errors.domains ? "text-destructive" : "text-muted-foreground",
                )}
              >
                {errors.domains ??
                  (ssoDomains.length
                    ? `Leave empty to use your sign-in domains: ${ssoDomains.join(", ")}.`
                    : "The service can only sign in people at these domains.")}
              </p>
            </div>

            <div className="flex items-start justify-between gap-6 rounded-lg border px-3.5 py-3">
              <div className="grid gap-0.5">
                <Label htmlFor={ids.create} className="leading-5">
                  Create accounts for new people
                </Label>
                <p
                  id={`${ids.create}-desc`}
                  className="text-pretty text-muted-foreground"
                >
                  {createUsers
                    ? "Someone at these domains without an account gets one on first use."
                    : "Off. The service can only sign in people who already have an account."}
                </p>
              </div>
              <Switch
                id={ids.create}
                checked={createUsers}
                onCheckedChange={setCreateUsers}
                aria-describedby={`${ids.create}-desc`}
                className="mt-0.5"
              />
            </div>

            {errors.form && (
              <p role="alert" className="text-sm text-destructive">
                {errors.form}
              </p>
            )}
          </div>

          <DialogFooter>
            <Button
              type="button"
              variant="outline"
              onClick={() => onOpenChange(false)}
              disabled={pending}
            >
              Cancel
            </Button>
            <Button type="submit" disabled={pending}>
              {pending ? "Creating…" : "Create integration"}
            </Button>
          </DialogFooter>
        </form>
      </DialogContent>
    </Dialog>
  );
}

function KeyOnceDialog({
  shown,
  onClose,
}: {
  shown: ShownKey | null;
  onClose: () => void;
}) {
  const { overview } = useSession();
  const [copied, setCopied] = useState(false);
  const [warned, setWarned] = useState(false);

  // Keep the last key around while the dialog animates out.
  const [last, setLast] = useState<ShownKey | null>(shown);
  useEffect(() => {
    if (shown) {
      setLast(shown);
      setCopied(false);
      setWarned(false);
    }
  }, [shown]);

  function requestClose() {
    if (!copied && !warned) {
      setWarned(true);
      return;
    }
    onClose();
  }

  const url = overview ? `https://${displayHost(overview.public_url)}` : null;
  const fingerprint = overview?.tls.fingerprint ?? null;

  return (
    <Dialog
      open={!!shown}
      onOpenChange={(next) => !next && requestClose()}
      disablePointerDismissal
    >
      <DialogContent className="sm:max-w-[540px]" showCloseButton={false}>
        <DialogHeader>
          <DialogTitle>
            {last?.rotated
              ? `New key for ${last.name}`
              : `Integration created: ${last?.name}`}
          </DialogTitle>
          <DialogDescription>
            {last?.rotated
              ? "The old key no longer works. Replace it in the service’s secret store."
              : "Put the key in the service’s secret store, never in code or chat."}
          </DialogDescription>
        </DialogHeader>

        <div
          role="note"
          className="flex items-start gap-2.5 rounded-md border border-warning/40 bg-warning-surface px-3 py-2.5 text-warning-foreground"
        >
          <TriangleAlertIcon
            className="mt-0.5 size-4 shrink-0 text-warning"
            aria-hidden="true"
          />
          <p className="text-sm">
            <span className="font-semibold">Shown once — copy it now.</span> Copper Cloud
            keeps only a hash of this key, so nobody can show it to you again.
          </p>
        </div>

        {last && (
          <SecretWell
            label="Integration key"
            value={last.key}
            emphasis
            onCopied={() => setCopied(true)}
            hint="Anyone holding it can sign in people at its domains. Treat it like a password."
          />
        )}

        {url && (
          <div className="grid gap-1.5">
            <p className="text-xs font-medium text-muted-foreground">
              The service also needs
            </p>
            <div className="rounded-md border text-sm">
              <dl>
                <ValueRow label="Server URL" value={url} />
                {fingerprint && <ValueRow label="Fingerprint" value={fingerprint} />}
              </dl>
              <p className="border-t px-3 py-2 text-xs text-pretty text-muted-foreground">
                A bot platform typically reads these as{" "}
                <code className="font-mono">COPPER_CLOUD_URL</code>
                {fingerprint && (
                  <>
                    , <code className="font-mono">COPPER_CLOUD_FINGERPRINT</code>
                  </>
                )}{" "}
                and the key as <code className="font-mono">COPPER_CLOUD_AGENT_KEY</code>.
              </p>
            </div>
          </div>
        )}

        <DialogFooter className="items-center sm:justify-between">
          <p className="text-xs text-muted-foreground" aria-live="polite">
            {warned && !copied ? (
              <span className="text-warning-foreground">
                You haven&rsquo;t copied the key yet. Close again to discard it.
              </span>
            ) : copied ? (
              "Copied. You can close this now."
            ) : null}
          </p>
          <Button onClick={requestClose} variant={copied ? "default" : "outline"}>
            {warned && !copied ? "Discard and close" : "Done"}
          </Button>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  );
}

function ValueRow({ label, value }: { label: string; value: string }) {
  return (
    <div className="grid grid-cols-[96px_minmax(0,1fr)_auto] items-center gap-2 border-b py-1 pr-1 pl-3 last:border-0">
      <dt className="text-muted-foreground">{label}</dt>
      <dd className="min-w-0">
        <code className="block truncate font-mono text-[12.5px]" title={value}>
          {value}
        </code>
      </dd>
      <CopyButton value={value} label={label.toLowerCase()} />
    </div>
  );
}

const seenAt = (iso: string | null) => (iso ? Date.parse(iso) || 0 : 0);

function DevicesSheet({
  integration,
  onClose,
}: {
  integration: Integration | null;
  onClose: () => void;
}) {
  // Keep the last integration while the sheet slides out.
  const [last, setLast] = useState<Integration | null>(integration);
  useEffect(() => {
    if (integration) setLast(integration);
  }, [integration]);

  const devices = useResource(
    integration ? `integration-devices:${integration.id}` : null,
    (signal) => api.integrations.devices(integration!.id, signal),
  );
  const rows = devices.data ?? [];

  return (
    <Sheet open={!!integration} onOpenChange={(open) => !open && onClose()}>
      <SheetContent
        side="right"
        className="gap-0 data-[side=right]:w-full data-[side=right]:sm:max-w-md"
      >
        <SheetHeader className="border-b pr-12">
          <SheetTitle>Devices for {last?.name}</SheetTitle>
          <SheetDescription>
            Coppers this integration signed in, newest activity first.
          </SheetDescription>
        </SheetHeader>
        <div className="min-h-0 flex-1 overflow-y-auto">
          {devices.loading ? (
            <ul aria-busy="true">
              {Array.from({ length: 4 }, (_, i) => (
                <li key={i} className="grid gap-2 border-b border-border/70 px-4 py-3">
                  <Skeleton className="h-3 w-40" />
                  <Skeleton className="h-3 w-28 opacity-70" />
                </li>
              ))}
            </ul>
          ) : devices.error ? (
            <div className="flex items-start gap-3 px-4 py-6 text-sm">
              <CircleAlertIcon
                className="mt-0.5 size-4 shrink-0 text-destructive"
                aria-hidden="true"
              />
              <div>
                <p className="font-medium">Couldn&rsquo;t load the devices</p>
                <p className="mt-0.5 text-muted-foreground">
                  {errorMessage(devices.error)}
                </p>
                <Button
                  variant="outline"
                  size="sm"
                  className="mt-3"
                  onClick={devices.reload}
                >
                  Try again
                </Button>
              </div>
            </div>
          ) : rows.length === 0 ? (
            <div className="px-4 py-8 text-sm">
              <EmptyState icon={BotIcon} title="No devices yet">
                A device appears here the first time the service pairs a Copper with this
                key.
              </EmptyState>
            </div>
          ) : (
            <ul aria-label={`Devices for ${last?.name ?? "this integration"}`}>
              {[...rows]
                .sort((a, b) => seenAt(b.last_seen_at) - seenAt(a.last_seen_at))
                .map((d) => (
                  <li
                    key={`${d.user_email}:${d.device_id}`}
                    className="flex items-start gap-3 border-b border-border/70 px-4 py-3 text-sm"
                  >
                    <BotIcon
                      className="mt-0.5 size-4 shrink-0 text-faint"
                      aria-hidden="true"
                    />
                    <div className="min-w-0 flex-1">
                      <p className="truncate font-medium" title={d.name}>
                        {d.name}
                      </p>
                      <p className="truncate text-muted-foreground" title={d.user_email}>
                        {d.user_email}
                      </p>
                    </div>
                    <RelativeTime
                      value={d.last_seen_at}
                      empty="Never seen"
                      className="shrink-0 pt-px text-xs text-muted-foreground"
                    />
                  </li>
                ))}
            </ul>
          )}
        </div>
        <SheetFooter className="border-t text-xs text-muted-foreground">
          <p>
            To sign one out, remove it on{" "}
            <Link
              href="/devices/"
              className="text-foreground underline underline-offset-4"
            >
              Devices
            </Link>
            .
          </p>
        </SheetFooter>
      </SheetContent>
    </Sheet>
  );
}
