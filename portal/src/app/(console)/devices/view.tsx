"use client";

import { LaptopIcon, LinkIcon, Trash2Icon, XIcon } from "lucide-react";
import { useEffect, useMemo, useState } from "react";
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
import { useSession } from "@/components/session-provider";
import { Button } from "@/components/ui/button";
import { Tooltip, TooltipContent, TooltipTrigger } from "@/components/ui/tooltip";
import { useList } from "@/hooks/use-list";
import { useNow } from "@/hooks/use-now";
import { api, errorMessage } from "@/lib/api";
import { matches, plural } from "@/lib/format";
import type { Device, PairingCode } from "@/lib/types";

const deviceKey = (d: Device) => `${d.user_id}:${d.id}`;

export function DevicesView() {
  // `/devices/?user=<id>` (from People) narrows the list to one account.
  // `undefined` until the URL has been read after hydration.
  const [userId, setUserId] = useState<string | null | undefined>(undefined);
  useEffect(() => {
    setUserId(new URLSearchParams(window.location.search).get("user"));
  }, []);
  return (
    <DevicesPage
      key={userId ?? "all"}
      userId={userId}
      onClearUser={() => {
        window.history.replaceState(null, "", window.location.pathname);
        setUserId(null);
      }}
    />
  );
}

function DevicesPage({
  userId,
  onClearUser,
}: {
  userId: string | null | undefined;
  onClearUser: () => void;
}) {
  const list = useList<Device>(
    userId === undefined ? null : `devices:${userId ?? ""}`,
    (offset, limit, signal) =>
      api.devices.list({ offset, limit, user_id: userId ?? undefined }, signal),
  );
  const { refreshOverview } = useSession();
  const [filter, setFilter] = useState("");
  const [removing, setRemoving] = useState<Device | null>(null);

  const rows = useMemo(
    () => list.items.filter((d) => matches(filter, d.name, d.user_email, d.id)),
    [list.items, filter],
  );
  const scopedEmail = userId ? list.items[0]?.user_email : undefined;

  const columns: Column<Device>[] = [
    {
      id: "device",
      header: "Device",
      skeleton: "w-36",
      cell: (d) => (
        <span className="flex min-w-0 items-center gap-2.5">
          <LaptopIcon className="size-4 shrink-0 text-faint" aria-hidden="true" />
          <span className="truncate font-medium text-foreground" title={d.name}>
            {d.name}
          </span>
          <code
            className="hidden shrink-0 font-mono text-[11px] text-faint @3xl:inline"
            title={d.id}
          >
            {d.id.slice(0, 8)}
          </code>
        </span>
      ),
    },
    {
      id: "person",
      header: "Person",
      className: "w-[32%]",
      skeleton: "w-40",
      cell: (d) => (
        <span className="text-muted-foreground" title={d.user_email}>
          {d.user_email}
        </span>
      ),
    },
    {
      id: "added",
      header: "Added",
      className: "w-24 @max-3xl:hidden",
      skeleton: "w-12",
      cell: (d) => (
        <RelativeTime value={d.created_at} className="text-muted-foreground" />
      ),
    },
    {
      id: "seen",
      header: "Last seen",
      className: "w-24",
      skeleton: "w-12",
      cell: (d) => (
        <RelativeTime
          value={d.last_seen_at}
          empty="Never"
          className="text-muted-foreground"
        />
      ),
    },
    {
      id: "actions",
      header: "Actions",
      srHeader: true,
      align: "right",
      className: "w-12 pr-2",
      skeleton: "none",
      cell: (d) => (
        <Tooltip>
          <TooltipTrigger
            render={
              <Button
                variant="ghost"
                size="icon-sm"
                onClick={() => setRemoving(d)}
                aria-label={`Remove ${d.name}`}
                className="text-muted-foreground hover:text-destructive"
              />
            }
          >
            <Trash2Icon aria-hidden="true" />
          </TooltipTrigger>
          <TooltipContent>Remove device</TooltipContent>
        </Tooltip>
      ),
    },
  ];

  return (
    <>
      <PageHeader
        title="Devices"
        description="Each Copper signed in to an account. Removing one signs it out and clears its list of open tabs."
      />

      <TableToolbar
        filter={filter}
        onFilterChange={setFilter}
        placeholder="Filter by device or person"
        count={list.loading ? undefined : plural(rows.length, "device")}
      >
        {userId && (
          <span className="flex h-8 items-center gap-1 rounded-md border bg-well pr-1 pl-2.5 text-xs">
            <span className="text-muted-foreground">Person</span>
            <span className="max-w-48 truncate font-medium">{scopedEmail ?? "…"}</span>
            <button
              type="button"
              onClick={onClearUser}
              className="ml-0.5 flex size-5 items-center justify-center rounded-sm text-faint hover:text-foreground focus-visible:ring-2 focus-visible:ring-ring focus-visible:outline-none"
              aria-label="Show devices for everyone"
            >
              <XIcon className="size-3.5" aria-hidden="true" />
            </button>
          </span>
        )}
      </TableToolbar>

      <DataTable
        label="Devices"
        columns={columns}
        rows={rows}
        rowKey={deviceKey}
        loading={list.loading}
        error={list.error}
        onRetry={list.reload}
        filtered={!!filter}
        empty={
          <EmptyState
            icon={LaptopIcon}
            title={userId ? "No devices for this person" : "No devices yet"}
          >
            {userId
              ? "They haven't signed in from a Copper yet, or every device was removed."
              : "A device appears here the first time someone signs in from Copper › Settings › Cloud."}
          </EmptyState>
        }
        noMatches={
          <EmptyState icon={LaptopIcon} title="No devices match">
            Nothing matches “{filter}”.{" "}
            <button
              type="button"
              className="text-foreground underline underline-offset-4"
              onClick={() => setFilter("")}
            >
              Clear filter
            </button>
          </EmptyState>
        }
        footer={
          <LoadMore
            loaded={list.items.length}
            total={list.total}
            noun="devices"
            onLoadMore={list.loadMore}
            loading={list.loadingMore}
          />
        }
      />

      {userId !== undefined && <PairingCodes userId={userId} />}

      <ConfirmDialog
        open={!!removing}
        onOpenChange={(open) => !open && setRemoving(null)}
        title={`Remove “${removing?.name ?? ""}”?`}
        description={
          removing
            ? `It's signed out of ${removing.user_email} and its open-tabs list is deleted. The person can sign in from it again later.`
            : undefined
        }
        confirmLabel="Remove device"
        pendingLabel="Removing…"
        onConfirm={async () => {
          if (!removing) return;
          await api.devices.remove(removing.id, removing.user_id);
          list.update(
            (items) => items.filter((d) => deviceKey(d) !== deviceKey(removing)),
            -1,
          );
          toast.success(`Removed ${removing.name}`);
          void refreshOverview();
        }}
      />
    </>
  );
}

/** Active pairing codes (10-minute, single use). Hidden when there are none. */
function PairingCodes({ userId }: { userId: string | null }) {
  const list = useList<PairingCode>(`pairing:${userId ?? ""}`, (offset, limit, signal) =>
    api.pairingCodes.list({ offset, limit, user_id: userId ?? undefined }, signal),
  );
  const now = useNow() || Date.now();
  const [revoking, setRevoking] = useState<PairingCode | null>(null);
  const live = list.items.filter((p) => Date.parse(p.expires_at) > now);
  if (list.loading || list.error || live.length === 0) return null;

  return (
    <section aria-labelledby="pairing-title" className="mt-10">
      <div className="mb-2 flex items-baseline justify-between gap-4">
        <h2 id="pairing-title" className="text-sm font-medium">
          Pairing codes in use
        </h2>
        <p className="text-xs text-muted-foreground">
          Minted by a signed-in Copper to sign in another Mac. Single use, 10 minutes.
        </p>
      </div>
      <ul className="rounded-lg border">
        {live.map((p) => (
          <li
            key={p.id}
            className="flex h-11 items-center gap-3 border-b border-border/70 px-3 last:border-0"
          >
            <LinkIcon className="size-4 shrink-0 text-faint" aria-hidden="true" />
            <p className="min-w-0 flex-1 truncate">
              <span className="font-medium">{p.user_email}</span>
              <span className="text-muted-foreground">
                {p.device_name ? ` from ${p.device_name}` : ""}
              </span>
            </p>
            <span className="shrink-0 text-xs text-muted-foreground">
              expires <RelativeTime value={p.expires_at} />
            </span>
            <Button variant="outline" size="sm" onClick={() => setRevoking(p)}>
              Revoke
            </Button>
          </li>
        ))}
      </ul>
      <ConfirmDialog
        open={!!revoking}
        onOpenChange={(open) => !open && setRevoking(null)}
        title="Revoke this pairing code?"
        description={
          revoking
            ? `${revoking.user_email} will need to mint a new one to sign in another Mac.`
            : undefined
        }
        confirmLabel="Revoke code"
        pendingLabel="Revoking…"
        onConfirm={async () => {
          if (!revoking) return;
          try {
            await api.pairingCodes.revoke(revoking.id);
          } catch (error) {
            // Already used or expired: drop it from the list either way.
            list.update((items) => items.filter((p) => p.id !== revoking.id), -1);
            throw new Error(`Couldn't revoke it: ${errorMessage(error)}`);
          }
          list.update((items) => items.filter((p) => p.id !== revoking.id), -1);
          toast.success("Pairing code revoked");
        }}
      />
    </section>
  );
}
