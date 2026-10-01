"use client";

import { RefreshCwIcon, UserPlusIcon, UsersIcon } from "lucide-react";
import Link from "next/link";
import { useEffect, useId, useMemo, useState } from "react";
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
import { Tooltip, TooltipContent, TooltipTrigger } from "@/components/ui/tooltip";
import { useList } from "@/hooks/use-list";
import { api, errorMessage } from "@/lib/api";
import { formatCount, initials, matches, plural } from "@/lib/format";
import { generatePassword, MIN_PASSWORD_LENGTH } from "@/lib/password";
import type { User } from "@/lib/types";
import { cn } from "@/lib/utils";

type StatusFilter = "all" | "active" | "disabled";
const nameOf = (u: User) => u.display_name?.trim() || u.email;

export function PeopleView() {
  const list = useList<User>("users", (offset, limit, signal) =>
    api.users.list({ offset, limit }, signal),
  );
  const { refreshOverview } = useSession();
  const [filter, setFilter] = useState("");
  const [status, setStatus] = useState<StatusFilter>("all");
  const [disabling, setDisabling] = useState<User | null>(null);
  const [deleting, setDeleting] = useState<User | null>(null);
  const [resetting, setResetting] = useState<User | null>(null);

  const disabledCount = useMemo(
    () => list.items.filter((u) => u.disabled).length,
    [list.items],
  );
  const rows = useMemo(
    () =>
      list.items.filter(
        (u) =>
          (status === "all" || (status === "disabled") === u.disabled) &&
          matches(filter, u.display_name, u.email),
      ),
    [list.items, filter, status],
  );

  const replace = (updated: User) =>
    list.update((items) => items.map((u) => (u.id === updated.id ? updated : u)));

  async function enable(user: User) {
    try {
      replace(await api.users.update(user.id, { disabled: false }));
      toast.success(`Enabled ${nameOf(user)}`);
    } catch (error) {
      toast.error(errorMessage(error));
    }
  }

  const columns: Column<User>[] = [
    {
      id: "name",
      header: "Name",
      skeleton: "w-32",
      cell: (u) => (
        <span className="flex min-w-0 items-center gap-2.5">
          <span
            aria-hidden="true"
            className={cn(
              "flex size-5 shrink-0 items-center justify-center rounded-full bg-muted text-[9.5px] font-semibold text-muted-foreground shadow-[inset_0_0_0_1px_var(--border)]",
              u.disabled && "opacity-60",
            )}
          >
            {initials(u.display_name, u.email)}
          </span>
          {u.display_name?.trim() ? (
            <span
              className={cn(
                "truncate font-medium",
                u.disabled ? "text-muted-foreground" : "text-foreground",
              )}
              title={u.display_name}
            >
              {u.display_name}
            </span>
          ) : (
            <span className="truncate text-muted-foreground">No name set</span>
          )}
        </span>
      ),
    },
    {
      id: "email",
      header: "Email",
      className: "w-[26%] @max-2xl:hidden",
      skeleton: "w-40",
      cell: (u) => (
        <span className="text-muted-foreground" title={u.email}>
          {u.email}
        </span>
      ),
    },
    {
      id: "created",
      header: "Joined",
      className: "w-24 @max-4xl:hidden",
      skeleton: "w-12",
      cell: (u) => (
        <RelativeTime value={u.created_at} className="text-muted-foreground" />
      ),
    },
    {
      id: "seen",
      header: "Last seen",
      className: "w-24",
      skeleton: "w-12",
      cell: (u) => (
        <RelativeTime
          value={u.last_seen_at}
          empty="Never"
          className="text-muted-foreground"
        />
      ),
    },
    {
      id: "devices",
      header: "Devices",
      align: "right",
      className: "w-[76px] @max-3xl:hidden",
      skeleton: "w-5",
      cell: (u) =>
        u.device_count > 0 ? (
          <Link
            href={`/devices/?user=${encodeURIComponent(u.id)}`}
            className="tabular-nums underline-offset-4 hover:underline"
            aria-label={`${plural(u.device_count, "device")} for ${u.email}`}
          >
            {formatCount(u.device_count)}
          </Link>
        ) : (
          <span className="text-faint tabular-nums">0</span>
        ),
    },
    {
      id: "canvases",
      header: "Canvases",
      align: "right",
      className: "w-[84px] @max-3xl:hidden",
      skeleton: "w-5",
      cell: (u) => (
        <span className={cn("tabular-nums", u.canvas_count === 0 && "text-faint")}>
          {formatCount(u.canvas_count)}
        </span>
      ),
    },
    {
      id: "status",
      header: "Status",
      className: "w-[120px] pl-4",
      skeleton: "w-14",
      cell: (u) =>
        u.disabled ? (
          <Tooltip>
            <TooltipTrigger render={<span className="cursor-default" />}>
              <StatusBadge tone="muted">Disabled</StatusBadge>
            </TooltipTrigger>
            <TooltipContent>Can&rsquo;t sign in. Data is kept.</TooltipContent>
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
      cell: (u) => (
        <RowActions label={`Actions for ${nameOf(u)}`}>
          {u.disabled ? (
            <DropdownMenuItem onClick={() => void enable(u)}>Enable</DropdownMenuItem>
          ) : (
            <DropdownMenuItem onClick={() => setDisabling(u)}>Disable…</DropdownMenuItem>
          )}
          <DropdownMenuItem onClick={() => setResetting(u)}>
            Reset password…
          </DropdownMenuItem>
          <DropdownMenuItem
            render={<Link href={`/devices/?user=${encodeURIComponent(u.id)}`} />}
          >
            Show devices
          </DropdownMenuItem>
          <DropdownMenuSeparator />
          <DropdownMenuItem variant="destructive" onClick={() => setDeleting(u)}>
            Delete…
          </DropdownMenuItem>
        </RowActions>
      ),
    },
  ];

  return (
    <>
      <PageHeader
        title="People"
        description="Everyone with an account here. People join by signing up with an access key."
        action={
          <Button render={<Link href="/keys/?new=1" />} nativeButton={false}>
            <UserPlusIcon aria-hidden="true" />
            Invite with a key
          </Button>
        }
      />

      <TableToolbar
        filter={filter}
        onFilterChange={setFilter}
        placeholder="Filter by name or email"
        count={list.loading ? undefined : plural(rows.length, "person", "people")}
      >
        <Segmented
          label="Status"
          value={status}
          onChange={setStatus}
          options={[
            { value: "all", label: "All" },
            {
              value: "active",
              label: "Active",
              count: list.items.length - disabledCount,
            },
            { value: "disabled", label: "Disabled", count: disabledCount },
          ]}
        />
      </TableToolbar>

      <DataTable
        label="People"
        columns={columns}
        rows={rows}
        rowKey={(u) => u.id}
        loading={list.loading}
        error={list.error}
        onRetry={list.reload}
        filtered={!!filter || status !== "all"}
        rowClassName={(u) => (u.disabled ? "[&_td]:text-muted-foreground" : undefined)}
        empty={
          <EmptyState
            icon={UsersIcon}
            title="Nobody has an account yet"
            action={
              <Button
                size="sm"
                render={<Link href="/keys/?new=1" />}
                nativeButton={false}
              >
                <UserPlusIcon aria-hidden="true" />
                Invite with a key
              </Button>
            }
          >
            Create an access key, send its link code, and the person appears here once
            they create an account from Copper.
          </EmptyState>
        }
        noMatches={
          <EmptyState icon={UsersIcon} title="No one matches">
            Try another name or email.{" "}
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
            noun="people"
            onLoadMore={list.loadMore}
            loading={list.loadingMore}
          />
        }
      />

      <ConfirmDialog
        open={!!disabling}
        onOpenChange={(open) => !open && setDisabling(null)}
        title={`Disable ${disabling ? nameOf(disabling) : ""}?`}
        description={
          disabling
            ? `They're signed out on ${plural(disabling.device_count, "device")} right away and can't sign in until you enable them again. Their data stays.`
            : undefined
        }
        confirmLabel="Disable"
        pendingLabel="Disabling…"
        onConfirm={async () => {
          if (!disabling) return;
          replace(await api.users.update(disabling.id, { disabled: true }));
          toast.success(`Disabled ${nameOf(disabling)}`);
        }}
      />

      <ConfirmDialog
        open={!!deleting}
        onOpenChange={(open) => !open && setDeleting(null)}
        title={`Delete ${deleting ? nameOf(deleting) : ""}?`}
        description="This can't be undone. Deleting the account permanently removes:"
        confirmLabel="Delete person"
        pendingLabel="Deleting…"
        confirmText={deleting?.email}
        className="data-[size=default]:sm:max-w-[460px]"
        onConfirm={async () => {
          if (!deleting) return;
          await api.users.remove(deleting.id);
          list.update((items) => items.filter((u) => u.id !== deleting.id), -1);
          toast.success(`Deleted ${deleting.email}`);
          void refreshOverview();
        }}
      >
        {deleting && <DeleteConsequences user={deleting} />}
      </ConfirmDialog>

      <ResetPasswordDialog user={resetting} onClose={() => setResetting(null)} />
    </>
  );
}

function DeleteConsequences({ user }: { user: User }) {
  const items = [
    `${plural(user.device_count, "device")} and every signed-in session`,
    "Synced bookmarks, history, open tabs and settings",
    `Canvases they own, including their Personal canvas; members of shared ones lose access${
      user.canvas_count
        ? ` (member of ${plural(user.canvas_count, "canvas", "canvases")})`
        : ""
    }`,
    "Pending pairing codes and access keys issued to their email",
  ];
  return (
    <ul className="grid gap-1.5 rounded-md border bg-well px-3 py-2.5 text-sm">
      {items.map((item) => (
        <li key={item} className="flex gap-2 text-pretty">
          <span
            className="mt-2 size-1 shrink-0 rounded-full bg-destructive"
            aria-hidden="true"
          />
          <span>{item}</span>
        </li>
      ))}
    </ul>
  );
}

function ResetPasswordDialog({
  user,
  onClose,
}: {
  user: User | null;
  onClose: () => void;
}) {
  const [password, setPassword] = useState("");
  const [pending, setPending] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [done, setDone] = useState<{ password: string; revoked: number } | null>(null);
  const [shown, setShown] = useState<User | null>(user);
  const inputId = useId();

  useEffect(() => {
    if (user) {
      setShown(user);
      setPassword(generatePassword());
      setError(null);
      setDone(null);
    }
  }, [user]);

  async function submit(event: React.FormEvent) {
    event.preventDefault();
    if (!shown || pending) return;
    if (password.length < MIN_PASSWORD_LENGTH) {
      setError(`Use at least ${MIN_PASSWORD_LENGTH} characters.`);
      return;
    }
    setPending(true);
    setError(null);
    try {
      const res = await api.users.resetPassword(shown.id, password);
      setDone({ password, revoked: res.revoked_sessions });
      toast.success(`Password reset for ${nameOf(shown)}`);
    } catch (err) {
      setError(errorMessage(err));
    } finally {
      setPending(false);
    }
  }

  const name = shown ? nameOf(shown) : "";
  return (
    <Dialog open={!!user} onOpenChange={(next) => !next && !pending && onClose()}>
      <DialogContent className="sm:max-w-[460px]">
        {done ? (
          <>
            <DialogHeader>
              <DialogTitle>New password set</DialogTitle>
              <DialogDescription>
                {name} was signed out of {plural(done.revoked, "session")}. Share this
                password with them privately; it isn&rsquo;t shown again.
              </DialogDescription>
            </DialogHeader>
            <SecretWell label="Password" value={done.password} emphasis />
            <DialogFooter>
              <Button onClick={onClose}>Done</Button>
            </DialogFooter>
          </>
        ) : (
          <form onSubmit={submit} className="contents" noValidate>
            <DialogHeader>
              <DialogTitle>Reset password for {name}</DialogTitle>
              <DialogDescription>
                They&rsquo;re signed out everywhere and sign in again with the new
                password.
              </DialogDescription>
            </DialogHeader>
            <div className="grid gap-1.5">
              <Label htmlFor={inputId}>New password</Label>
              <div className="flex gap-1.5">
                <Input
                  id={inputId}
                  value={password}
                  onChange={(e) => setPassword(e.target.value)}
                  className="font-mono text-[13px]"
                  autoComplete="new-password"
                  spellCheck={false}
                  aria-invalid={error ? true : undefined}
                  aria-describedby={`${inputId}-hint`}
                />
                <Tooltip>
                  <TooltipTrigger
                    render={
                      <Button
                        type="button"
                        variant="outline"
                        size="icon"
                        onClick={() => setPassword(generatePassword())}
                        aria-label="Generate another password"
                      />
                    }
                  >
                    <RefreshCwIcon aria-hidden="true" />
                  </TooltipTrigger>
                  <TooltipContent>Generate another</TooltipContent>
                </Tooltip>
              </div>
              <p
                id={`${inputId}-hint`}
                className={cn(
                  "text-xs",
                  error ? "text-destructive" : "text-muted-foreground",
                )}
              >
                {error ??
                  `Generated for you. Edit it if you like (${MIN_PASSWORD_LENGTH}+ characters).`}
              </p>
            </div>
            <DialogFooter>
              <Button
                type="button"
                variant="outline"
                onClick={onClose}
                disabled={pending}
              >
                Cancel
              </Button>
              <Button type="submit" disabled={pending}>
                {pending ? "Resetting…" : "Reset password"}
              </Button>
            </DialogFooter>
          </form>
        )}
      </DialogContent>
    </Dialog>
  );
}
