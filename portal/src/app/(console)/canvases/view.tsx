"use client";

import { CircleAlertIcon, CloudOffIcon, FrameIcon, LockIcon } from "lucide-react";
import Link from "next/link";
import { useMemo, useState } from "react";
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
import { Segmented } from "@/components/segmented";
import { useSession } from "@/components/session-provider";
import { Button } from "@/components/ui/button";
import { DropdownMenuItem } from "@/components/ui/dropdown-menu";
import { useList } from "@/hooks/use-list";
import { api, ApiError, errorMessage } from "@/lib/api";
import { formatCount, matches, plural } from "@/lib/format";
import type { Canvas, CanvasKind } from "@/lib/types";
import { cn } from "@/lib/utils";

type KindFilter = "all" | CanvasKind;

/** Where the table would be, for states with no table to show. */
function CanvasCloudState({ children }: { children: React.ReactNode }) {
  return (
    <div className="border-t px-3 py-10 text-sm">
      <div className="mx-auto max-w-md">{children}</div>
    </div>
  );
}

export function CanvasesView() {
  // `linked: false` = no Canvas Cloud is linked, so there is nothing to list.
  const [linked, setLinked] = useState<boolean>();
  const list = useList<Canvas>("canvases", async (offset, limit, signal) => {
    const page = await api.canvases.list({ offset, limit }, signal);
    setLinked(page.linked !== false);
    return page;
  });
  const errorCode = list.error instanceof ApiError ? list.error.code : undefined;
  const notLinked = linked === false || errorCode === "canvas_unavailable";
  const upstreamDown = !notLinked && errorCode === "canvas_upstream";
  const { refreshOverview } = useSession();
  const [filter, setFilter] = useState("");
  const [kind, setKind] = useState<KindFilter>("all");
  const [deleting, setDeleting] = useState<Canvas | null>(null);

  const sharedCount = useMemo(
    () => list.items.filter((c) => c.kind === "shared").length,
    [list.items],
  );
  const rows = useMemo(
    () =>
      list.items.filter(
        (c) =>
          (kind === "all" || c.kind === kind) && matches(filter, c.name, c.owner_email),
      ),
    [list.items, filter, kind],
  );

  const columns: Column<Canvas>[] = [
    {
      id: "name",
      header: "Name",
      skeleton: "w-36",
      cell: (c) => (
        <span className="flex min-w-0 items-center gap-2.5">
          {c.kind === "personal" ? (
            <LockIcon className="size-3.5 shrink-0 text-faint" aria-hidden="true" />
          ) : (
            <FrameIcon className="size-3.5 shrink-0 text-faint" aria-hidden="true" />
          )}
          <span className="truncate font-medium text-foreground" title={c.name}>
            {c.name}
          </span>
        </span>
      ),
    },
    {
      id: "kind",
      header: "Kind",
      className: "w-24",
      skeleton: "w-12",
      cell: (c) => (
        <span
          className={cn(
            c.kind === "personal" ? "text-muted-foreground" : "text-foreground/85",
          )}
        >
          {c.kind === "personal" ? "Personal" : "Shared"}
        </span>
      ),
    },
    {
      id: "owner",
      header: "Owner",
      className: "w-[30%]",
      skeleton: "w-40",
      cell: (c) => (
        <span className="text-muted-foreground" title={c.owner_email}>
          {c.owner_email}
        </span>
      ),
    },
    {
      id: "members",
      header: "Members",
      align: "right",
      className: "w-[88px]",
      skeleton: "w-5",
      cell: (c) => <span className="tabular-nums">{formatCount(c.member_count)}</span>,
    },
    {
      id: "share-links",
      header: "Share links",
      align: "right",
      className: "w-[104px]",
      skeleton: "w-5",
      cell: (c) => (
        <span className="tabular-nums">{formatCount(c.share_link_count)}</span>
      ),
    },
    {
      id: "updated",
      header: "Updated",
      className: "w-24 pl-5",
      skeleton: "w-12",
      cell: (c) => (
        <RelativeTime value={c.updated_at} className="text-muted-foreground" />
      ),
    },
    {
      id: "actions",
      header: "Actions",
      srHeader: true,
      align: "right",
      className: "w-12 pr-2",
      skeleton: "none",
      cell: (c) => (
        <RowActions label={`Actions for ${c.name}`}>
          <DropdownMenuItem variant="destructive" onClick={() => setDeleting(c)}>
            Delete canvas…
          </DropdownMenuItem>
        </RowActions>
      ),
    },
  ];

  return (
    <>
      <PageHeader
        title="Canvases"
        description="Names, owners and members only. Canvas content never reaches the admin console."
      />

      {notLinked ? (
        <CanvasCloudState>
          <EmptyState
            icon={CloudOffIcon}
            title="No Canvas Cloud linked"
            action={
              <Button render={<Link href="/canvas-cloud/" />} nativeButton={false}>
                Link a Canvas Cloud
              </Button>
            }
          >
            Canvases live on a Canvas Cloud server. Until one is linked, Copper users
            can&rsquo;t open canvases. Nothing has been deleted.
          </EmptyState>
        </CanvasCloudState>
      ) : upstreamDown ? (
        <CanvasCloudState>
          <div className="flex items-start gap-3">
            <CircleAlertIcon
              className="mt-0.5 size-4 shrink-0 text-destructive"
              aria-hidden="true"
            />
            <div className="min-w-0">
              <p className="font-medium text-foreground">
                Can&rsquo;t reach the Canvas Cloud
              </p>
              <p className="mt-0.5 text-pretty wrap-break-word text-muted-foreground">
                {errorMessage(list.error)}
              </p>
              <div className="mt-3 flex flex-wrap gap-2">
                <Button variant="outline" size="sm" onClick={list.reload}>
                  Try again
                </Button>
                <Button
                  variant="ghost"
                  size="sm"
                  render={<Link href="/canvas-cloud/" />}
                  nativeButton={false}
                >
                  Check the link
                </Button>
              </div>
            </div>
          </div>
        </CanvasCloudState>
      ) : (
        <>
          <TableToolbar
            filter={filter}
            onFilterChange={setFilter}
            placeholder="Filter by name or owner"
            count={list.loading ? undefined : plural(rows.length, "canvas", "canvases")}
          >
            <Segmented
              label="Kind"
              value={kind}
              onChange={setKind}
              options={[
                { value: "all", label: "All" },
                { value: "shared", label: "Shared", count: sharedCount },
                {
                  value: "personal",
                  label: "Personal",
                  count: list.items.length - sharedCount,
                },
              ]}
            />
          </TableToolbar>

          <DataTable
            label="Canvases"
            columns={columns}
            rows={rows}
            rowKey={(c) => c.id}
            loading={list.loading}
            error={list.error}
            onRetry={list.reload}
            filtered={!!filter || kind !== "all"}
            empty={
              <EmptyState icon={FrameIcon} title="No canvases yet">
                Each person gets a Personal canvas the first time they open Canvas in
                Copper. Shared canvases appear when someone creates one and invites
                others.
              </EmptyState>
            }
            noMatches={
              <EmptyState icon={FrameIcon} title="No canvases match">
                Try another name or owner.{" "}
                <button
                  type="button"
                  className="text-foreground underline underline-offset-4"
                  onClick={() => {
                    setFilter("");
                    setKind("all");
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
                noun="canvases"
                onLoadMore={list.loadMore}
                loading={list.loadingMore}
              />
            }
          />
        </>
      )}

      <ConfirmDialog
        open={!!deleting}
        onOpenChange={(open) => !open && setDeleting(null)}
        title={`Delete “${deleting?.name ?? ""}”?`}
        description={
          deleting?.kind === "personal"
            ? `This erases ${deleting.owner_email}'s Personal canvas and its history. It comes back empty the next time they open Canvas.`
            : deleting
              ? `This deletes the canvas, its history, its ${plural(deleting.member_count, "member")} and pending invites. Anyone viewing it is disconnected. This can't be undone.`
              : undefined
        }
        confirmLabel="Delete canvas"
        pendingLabel="Deleting…"
        onConfirm={async () => {
          if (!deleting) return;
          await api.canvases.remove(deleting.id);
          list.update((items) => items.filter((c) => c.id !== deleting.id), -1);
          toast.success(`Deleted “${deleting.name}”`);
          void refreshOverview();
        }}
      />
    </>
  );
}
