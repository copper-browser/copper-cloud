"use client";

import { KeyRoundIcon, PlusIcon, TriangleAlertIcon } from "lucide-react";
import Link from "next/link";
import { useCallback, useEffect, useId, useMemo, useState } from "react";
import { toast } from "sonner";

import { ConfirmDialog } from "@/components/confirm-dialog";
import {
  type Column,
  DataTable,
  EmptyState,
  LoadMore,
  TableToolbar,
} from "@/components/data-table";
import { PageHeader } from "@/components/page-header";
import { RelativeTime } from "@/components/relative-time";
import { RowActions } from "@/components/row-actions";
import { SecretWell } from "@/components/secret-well";
import { Segmented } from "@/components/segmented";
import { useSession } from "@/components/session-provider";
import { StatusBadge, type Tone } from "@/components/status-badge";
import { Button } from "@/components/ui/button";
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from "@/components/ui/dialog";
import { DropdownMenuItem } from "@/components/ui/dropdown-menu";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from "@/components/ui/select";
import { Tooltip, TooltipContent, TooltipTrigger } from "@/components/ui/tooltip";
import { useList } from "@/hooks/use-list";
import { api, ApiError, errorMessage } from "@/lib/api";
import { formatCount, matches, plural } from "@/lib/format";
import type { AccessKey, AccessKeyStatus, CreatedAccessKey } from "@/lib/types";

const STATUS: Record<AccessKeyStatus, { label: string; tone: Tone; hint: string }> = {
  active: { label: "Active", tone: "success", hint: "Connects and signs people in." },
  exhausted: {
    label: "Used up",
    tone: "neutral",
    hint: "Reached its max uses. Coppers already signed in with it keep working.",
  },
  expired: {
    label: "Expired",
    tone: "warning",
    hint: "Past its expiry date. Rejected at the gate.",
  },
  revoked: {
    label: "Revoked",
    tone: "danger",
    hint: "Revoked by an admin. Rejected at the gate.",
  },
};

type StatusFilter = "all" | AccessKeyStatus;

export function AccessKeysView() {
  const list = useList<AccessKey>("access-keys", (offset, limit, signal) =>
    api.accessKeys.list({ offset, limit }, signal),
  );
  const { overview, refreshOverview } = useSession();
  const [filter, setFilter] = useState("");
  const [status, setStatus] = useState<StatusFilter>("all");
  const [creating, setCreating] = useState(false);
  const [created, setCreated] = useState<CreatedAccessKey | null>(null);
  const [revoking, setRevoking] = useState<AccessKey | null>(null);

  // `/keys/?new=1` (from Overview / People) opens the dialog straight away.
  useEffect(() => {
    const params = new URLSearchParams(window.location.search);
    if (params.get("new") === "1") {
      setCreating(true);
      window.history.replaceState(null, "", window.location.pathname);
    }
  }, []);

  const counts = useMemo(() => {
    const c: Record<StatusFilter, number> = {
      all: list.items.length,
      active: 0,
      exhausted: 0,
      expired: 0,
      revoked: 0,
    };
    list.items.forEach((k) => (c[k.status] += 1));
    return c;
  }, [list.items]);

  const rows = useMemo(
    () =>
      list.items.filter(
        (k) =>
          (status === "all" || k.status === status) &&
          matches(filter, k.label, k.email, k.created_by),
      ),
    [list.items, filter, status],
  );

  const onCreated = useCallback(
    (key: CreatedAccessKey) => {
      const { key: _secret, link_code: _link, ...row } = key;
      list.update((items) => [row, ...items], 1);
      setCreating(false);
      setCreated(key);
      void refreshOverview();
    },
    [list, refreshOverview],
  );

  const columns: Column<AccessKey>[] = [
    {
      id: "label",
      header: "Label",
      skeleton: "w-32",
      cell: (k) => (
        <span className="font-medium text-foreground" title={k.label}>
          {k.label}
        </span>
      ),
    },
    {
      id: "email",
      header: "Email",
      className: "w-[24%] @max-2xl:hidden",
      skeleton: "w-36",
      cell: (k) =>
        k.email ? (
          <span className="text-muted-foreground" title={k.email}>
            {k.email}
          </span>
        ) : (
          <span className="text-faint">Any email</span>
        ),
    },
    {
      id: "created",
      header: "Created",
      className: "w-24 @max-4xl:hidden",
      skeleton: "w-12",
      cell: (k) => (
        <RelativeTime value={k.created_at} className="text-muted-foreground" />
      ),
    },
    {
      id: "expires",
      header: "Expires",
      className: "w-24",
      skeleton: "w-12",
      cell: (k) => (
        <RelativeTime
          value={k.expires_at}
          empty="Never"
          className="text-muted-foreground"
        />
      ),
    },
    {
      id: "last-used",
      header: "Last used",
      className: "w-24 @max-3xl:hidden",
      skeleton: "w-12",
      cell: (k) => (
        <RelativeTime
          value={k.last_used_at}
          empty="Never"
          className="text-muted-foreground"
        />
      ),
    },
    {
      id: "uses",
      header: "Uses",
      align: "right",
      className: "w-20",
      skeleton: "w-8",
      cell: (k) => (
        <span className="tabular-nums">
          {formatCount(k.uses)}
          <span className="text-faint">
            {" "}
            / {k.max_uses === null ? "∞" : formatCount(k.max_uses)}
          </span>
        </span>
      ),
    },
    {
      id: "status",
      header: "Status",
      className: "w-[120px] pl-4",
      skeleton: "w-14",
      cell: (k) => {
        const s = STATUS[k.status] ?? {
          label: k.status,
          tone: "neutral" as Tone,
          hint: "",
        };
        return (
          <Tooltip>
            <TooltipTrigger render={<span className="cursor-default" />}>
              <StatusBadge tone={s.tone}>{s.label}</StatusBadge>
            </TooltipTrigger>
            <TooltipContent>{s.hint}</TooltipContent>
          </Tooltip>
        );
      },
    },
    {
      id: "actions",
      header: "Actions",
      srHeader: true,
      className: "w-12 pr-2",
      align: "right",
      skeleton: "none",
      cell: (k) =>
        k.status === "revoked" ? null : (
          <RowActions label={`Actions for ${k.label}`}>
            <DropdownMenuItem variant="destructive" onClick={() => setRevoking(k)}>
              Revoke key…
            </DropdownMenuItem>
          </RowActions>
        ),
    },
  ];

  const segments: { value: StatusFilter; label: string; count?: number }[] = [
    { value: "all", label: "All" },
    { value: "active", label: "Active", count: counts.active },
    { value: "exhausted", label: "Used up", count: counts.exhausted },
    { value: "expired", label: "Expired", count: counts.expired },
    { value: "revoked", label: "Revoked", count: counts.revoked },
  ];

  return (
    <>
      <PageHeader
        title="Access keys"
        description="One key per person or team. Paste the link code into Copper › Settings › Cloud."
        action={
          <Button onClick={() => setCreating(true)}>
            <PlusIcon aria-hidden="true" />
            New key
          </Button>
        }
      />

      {overview?.access_mode === "open" && (
        <p className="mb-4 rounded-md border bg-well px-3 py-2 text-sm text-muted-foreground">
          This instance is in <span className="font-medium text-foreground">open</span>{" "}
          mode, so the shared instance key also gets people in.{" "}
          <Link
            href="/settings/"
            className="text-foreground underline underline-offset-4"
          >
            Switch to directory
          </Link>{" "}
          to require a key per person.
        </p>
      )}

      <TableToolbar
        filter={filter}
        onFilterChange={setFilter}
        placeholder="Filter keys"
        count={list.loading ? undefined : plural(rows.length, "key")}
      >
        <Segmented
          label="Status"
          value={status}
          onChange={setStatus}
          options={segments}
        />
      </TableToolbar>

      <DataTable
        label="Access keys"
        columns={columns}
        rows={rows}
        rowKey={(k) => k.id}
        loading={list.loading}
        error={list.error}
        onRetry={list.reload}
        filtered={!!filter || status !== "all"}
        rowClassName={(k) =>
          k.status === "revoked" || k.status === "expired"
            ? "[&_td]:text-muted-foreground [&_td_.font-medium]:text-muted-foreground"
            : undefined
        }
        empty={
          <EmptyState
            icon={KeyRoundIcon}
            title="No access keys yet"
            action={
              <Button size="sm" onClick={() => setCreating(true)}>
                <PlusIcon aria-hidden="true" />
                New key
              </Button>
            }
          >
            Create a key for each person who should connect a Copper to this instance,
            then send them its link code.
          </EmptyState>
        }
        noMatches={
          <EmptyState icon={KeyRoundIcon} title="No keys match">
            Nothing matches{filter ? ` “${filter}”` : ""}
            {status !== "all" ? ` in ${STATUS[status].label.toLowerCase()}` : ""}.{" "}
            <button
              type="button"
              className="text-foreground underline underline-offset-4"
              onClick={() => {
                setFilter("");
                setStatus("all");
              }}
            >
              Clear filters
            </button>
          </EmptyState>
        }
        footer={
          <LoadMore
            loaded={list.items.length}
            total={list.total}
            noun="keys"
            onLoadMore={list.loadMore}
            loading={list.loadingMore}
          />
        }
      />

      <NewKeyDialog open={creating} onOpenChange={setCreating} onCreated={onCreated} />
      <KeyCreatedDialog created={created} onClose={() => setCreated(null)} />

      <ConfirmDialog
        open={!!revoking}
        onOpenChange={(open) => !open && setRevoking(null)}
        title={`Revoke “${revoking?.label ?? ""}”?`}
        description="Every Copper using this key is cut off on its next request, and nobody can sign in with it again. This can't be undone."
        confirmLabel="Revoke key"
        pendingLabel="Revoking…"
        onConfirm={async () => {
          if (!revoking) return;
          const updated = await api.accessKeys.revoke(revoking.id);
          list.update((items) => items.map((k) => (k.id === updated.id ? updated : k)));
          toast.success(`Revoked “${updated.label}”`);
          void refreshOverview();
        }}
      />
    </>
  );
}

// ---------------------------------------------------------------------------

const EXPIRY = [
  { value: "never", label: "Never" },
  { value: "7", label: "In 7 days" },
  { value: "30", label: "In 30 days" },
  { value: "90", label: "In 90 days" },
];
const MAX_USES = [
  { value: "unlimited", label: "Unlimited" },
  { value: "1", label: "1 sign-in (single invite)" },
  { value: "5", label: "5 sign-ins" },
];
const EMAIL_RE = /^[^@\s]+@[^@\s]+\.[^@\s]+$/;

function NewKeyDialog({
  open,
  onOpenChange,
  onCreated,
}: {
  open: boolean;
  onOpenChange: (open: boolean) => void;
  onCreated: (key: CreatedAccessKey) => void;
}) {
  const [label, setLabel] = useState("");
  const [email, setEmail] = useState("");
  const [expiry, setExpiry] = useState("30");
  const [maxUses, setMaxUses] = useState("unlimited");
  const [errors, setErrors] = useState<{ label?: string; email?: string; form?: string }>(
    {},
  );
  const [pending, setPending] = useState(false);
  const ids = { label: useId(), email: useId(), expiry: useId(), uses: useId() };

  function reset() {
    setLabel("");
    setEmail("");
    setExpiry("30");
    setMaxUses("unlimited");
    setErrors({});
  }

  async function submit(event: React.FormEvent) {
    event.preventDefault();
    if (pending) return;
    const next: typeof errors = {};
    if (!label.trim())
      next.label = "Give the key a label so you can tell it apart later.";
    if (email.trim() && !EMAIL_RE.test(email.trim()))
      next.email = "Enter a valid email, or leave it empty.";
    setErrors(next);
    if (next.label || next.email) return;

    setPending(true);
    try {
      const key = await api.accessKeys.create({
        label: label.trim(),
        email: email.trim() || null,
        expires_in_days: expiry === "never" ? null : Number(expiry),
        max_uses: maxUses === "unlimited" ? null : Number(maxUses),
      });
      reset();
      onCreated(key);
    } catch (error) {
      setErrors({
        form:
          error instanceof ApiError && error.code === "bad_request"
            ? error.message
            : errorMessage(error),
      });
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
      <DialogContent className="sm:max-w-[440px]">
        <form onSubmit={submit} className="contents" noValidate>
          <DialogHeader>
            <DialogTitle>New access key</DialogTitle>
            <DialogDescription>
              The key and its link code are shown once, right after you create it.
            </DialogDescription>
          </DialogHeader>

          <div className="grid gap-4">
            <div className="grid gap-1.5">
              <Label htmlFor={ids.label}>Label</Label>
              <Input
                id={ids.label}
                value={label}
                onChange={(e) => setLabel(e.target.value)}
                placeholder="Ana’s MacBook, Design team…"
                maxLength={200}
                autoFocus
                autoComplete="off"
                aria-invalid={errors.label ? true : undefined}
                aria-describedby={`${ids.label}-hint`}
              />
              <p
                id={`${ids.label}-hint`}
                className={
                  errors.label
                    ? "text-xs text-destructive"
                    : "text-xs text-muted-foreground"
                }
              >
                {errors.label ?? "Only admins see this."}
              </p>
            </div>

            <div className="grid gap-1.5">
              <Label htmlFor={ids.email}>
                Email{" "}
                <span className="font-normal text-muted-foreground">(optional)</span>
              </Label>
              <Input
                id={ids.email}
                type="email"
                value={email}
                onChange={(e) => setEmail(e.target.value)}
                placeholder="ana@example.com"
                autoComplete="off"
                aria-invalid={errors.email ? true : undefined}
                aria-describedby={`${ids.email}-hint`}
              />
              <p
                id={`${ids.email}-hint`}
                className={
                  errors.email
                    ? "text-xs text-destructive"
                    : "text-xs text-muted-foreground"
                }
              >
                {errors.email ??
                  "If set, the key can only create an account with this email."}
              </p>
            </div>

            <div className="grid grid-cols-2 gap-3">
              <div className="grid gap-1.5">
                <Label htmlFor={ids.expiry}>Expires</Label>
                <Select
                  items={EXPIRY}
                  value={expiry}
                  onValueChange={(v) => setExpiry(String(v))}
                >
                  <SelectTrigger id={ids.expiry} className="w-full">
                    <SelectValue />
                  </SelectTrigger>
                  <SelectContent>
                    {EXPIRY.map((o) => (
                      <SelectItem key={o.value} value={o.value}>
                        {o.label}
                      </SelectItem>
                    ))}
                  </SelectContent>
                </Select>
              </div>
              <div className="grid gap-1.5">
                <Label htmlFor={ids.uses}>Max uses</Label>
                <Select
                  items={MAX_USES}
                  value={maxUses}
                  onValueChange={(v) => setMaxUses(String(v))}
                >
                  <SelectTrigger id={ids.uses} className="w-full">
                    <SelectValue />
                  </SelectTrigger>
                  <SelectContent>
                    {MAX_USES.map((o) => (
                      <SelectItem key={o.value} value={o.value}>
                        {o.label}
                      </SelectItem>
                    ))}
                  </SelectContent>
                </Select>
              </div>
            </div>
            <p className="-mt-1 text-xs text-pretty text-muted-foreground">
              A use is one sign-in. Coppers already signed in keep working when the key is
              used up.
            </p>

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
              {pending ? "Creating…" : "Create key"}
            </Button>
          </DialogFooter>
        </form>
      </DialogContent>
    </Dialog>
  );
}

function KeyCreatedDialog({
  created,
  onClose,
}: {
  created: CreatedAccessKey | null;
  onClose: () => void;
}) {
  const [copied, setCopied] = useState(false);
  const [warned, setWarned] = useState(false);

  // Keep the last key around while the dialog animates out.
  const [shown, setShown] = useState<CreatedAccessKey | null>(created);
  useEffect(() => {
    if (created) {
      setShown(created);
      setCopied(false);
      setWarned(false);
    }
  }, [created]);

  function requestClose() {
    if (!copied && !warned) {
      setWarned(true);
      return;
    }
    onClose();
  }

  const who = shown?.email ?? "the person";
  return (
    <Dialog
      open={!!created}
      onOpenChange={(next) => !next && requestClose()}
      disablePointerDismissal
    >
      <DialogContent className="sm:max-w-[520px]" showCloseButton={false}>
        <DialogHeader>
          <DialogTitle>Key created: {shown?.label}</DialogTitle>
          <DialogDescription>
            Send the link code to {who}. They paste it into Copper › Settings › Cloud to
            connect and create their account.
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

        {shown && (
          <div className="grid gap-3">
            <SecretWell
              label="Link code"
              value={shown.link_code}
              emphasis
              onCopied={() => setCopied(true)}
              hint="Paste into Copper › Settings › Cloud › Connect."
            />
            <SecretWell
              label="Access key"
              value={shown.key}
              onCopied={() => setCopied(true)}
              hint="Only needed to connect by host and key instead of the link code."
            />
          </div>
        )}

        <DialogFooter className="items-center sm:justify-between">
          <p className="text-xs text-muted-foreground" aria-live="polite">
            {warned && !copied ? (
              <span className="text-warning-foreground">
                You haven&rsquo;t copied anything yet. Close again to discard it.
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
