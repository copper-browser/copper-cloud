"use client";

import { CircleAlertIcon, PuzzleIcon, UploadIcon } from "lucide-react";
import { useCallback, useEffect, useMemo, useState } from "react";
import { toast } from "sonner";

import { AddonCard } from "@/components/addons/addon-card";
import { UploadDialog } from "@/components/addons/upload-dialog";
import { EmptyState } from "@/components/data-table";
import { PageHeader } from "@/components/page-header";
import { Button } from "@/components/ui/button";
import { Skeleton } from "@/components/ui/skeleton";
import { useResource } from "@/hooks/use-resource";
import { api, errorMessage } from "@/lib/api";
import type { Addon, AddonSlot, AddonUpload } from "@/lib/types";

interface UploadState {
  slot?: string;
  target?: Addon | null;
}

/** Copper's own pick order within a slot: priority, then first uploaded. */
const byPriority = (a: Addon, b: Addon) =>
  b.priority - a.priority || Date.parse(a.created_at) - Date.parse(b.created_at);

export function AddonsView() {
  const slots = useResource("addon-slots", (signal) => api.addons.slots(signal));
  const list = useResource("addons", (signal) => api.addons.list(signal));
  const [upload, setUpload] = useState<UploadState | null>(null);
  const { mutate } = list;

  // `/addons/?upload=1[&slot=…]` opens the upload dialog straight away.
  useEffect(() => {
    const url = new URL(window.location.href);
    if (url.searchParams.get("upload") !== "1") return;
    setUpload({ slot: url.searchParams.get("slot") ?? undefined });
    url.searchParams.delete("upload");
    url.searchParams.delete("slot");
    window.history.replaceState(null, "", url.pathname + url.search);
  }, []);

  /** Insert or replace a row; never let a stale response undo a newer one. */
  const upsert = useCallback(
    (next: Addon) =>
      mutate((prev) => {
        const rows = prev ?? [];
        const i = rows.findIndex((a) => a.id === next.id);
        if (i === -1) return [...rows, next];
        if (next.revision < rows[i]!.revision) return rows;
        const copy = [...rows];
        copy[i] = next;
        return copy;
      }),
    [mutate],
  );

  const remove = useCallback(
    (addonId: string) => mutate((prev) => prev?.filter((a) => a.id !== addonId)),
    [mutate],
  );

  const groups = useMemo(() => {
    const addons = list.data ?? [];
    const known: AddonSlot[] = [...(slots.data ?? [])];
    for (const a of addons) {
      if (a.slot && !known.some((s) => s.id === a.slot)) {
        known.push({
          id: a.slot,
          name: a.slot,
          description: "This server no longer offers this slot.",
        });
      }
    }
    return known.map((slot) => ({
      slot,
      addons: addons.filter((a) => a.slot === slot.id).sort(byPriority),
    }));
  }, [slots.data, list.data]);

  const error = slots.error ?? list.error;
  const ready = !!slots.data && !!list.data;
  const total = list.data?.length ?? 0;

  function onUploaded({ addon: next, outcome }: AddonUpload) {
    const target = upload?.target;
    const existed = list.data?.some((a) => a.id === next.id) ?? false;
    const label = `${next.name} ${next.current?.version ?? ""}`.trim();
    upsert(next);
    setUpload(null);
    if (outcome === "unchanged") {
      toast.info(`${label} was already uploaded. Nothing changed.`);
    } else if (target && target.id !== next.id) {
      toast.success(`Uploaded ${label} as a separate addon`);
    } else if (outcome === "new_version" || existed) {
      toast.success(`${label} is now current`);
    } else {
      toast.success(`Uploaded ${label}. It stays off until you turn it on.`);
    }
  }

  return (
    <>
      <PageHeader
        title="Addons"
        description="Small web apps that replace part of Copper for the people you choose. Coppers pick up changes without a new release."
        action={
          // When there are no addons, the empty state carries the one Upload button.
          ready && total === 0 ? undefined : (
            <Button onClick={() => setUpload({})} disabled={!slots.data}>
              <UploadIcon aria-hidden="true" />
              Upload addon
            </Button>
          )
        }
      />

      {error && !ready ? (
        <LoadError
          error={error}
          onRetry={() => {
            if (slots.error) void slots.reload();
            if (list.error) void list.reload();
          }}
        />
      ) : !ready ? (
        <div
          aria-busy="true"
          className="grid gap-x-10 gap-y-4 pt-2 @3xl:grid-cols-[220px_minmax(0,1fr)]"
        >
          <div className="grid content-start gap-2">
            <Skeleton className="h-4 w-28" />
            <Skeleton className="h-3.5 w-44" />
          </div>
          <Skeleton className="h-[420px] w-full rounded-lg @3xl:max-w-[560px]" />
        </div>
      ) : (
        groups.map(({ slot, addons }) => (
          <SlotSection key={slot.id} slot={slot}>
            {addons.length === 0 ? (
              <div className="rounded-lg border px-4 py-8">
                <div className="mx-auto max-w-md">
                  <EmptyState
                    icon={PuzzleIcon}
                    title={total === 0 ? "No addons yet" : `Nothing in the ${slot.name}`}
                    action={
                      <Button
                        size="sm"
                        variant={total === 0 ? "default" : "outline"}
                        onClick={() => setUpload({ slot: slot.id })}
                      >
                        <UploadIcon aria-hidden="true" />
                        Upload addon
                      </Button>
                    }
                  >
                    {total === 0
                      ? "An addon is a small web app that takes over one part of Copper, like the agent sidebar, for the people you choose."
                      : "Coppers show their built-in pane here."}
                  </EmptyState>
                </div>
              </div>
            ) : (
              <div className="grid gap-3">
                {addons.map((addon) => (
                  <AddonCard
                    key={addon.id}
                    addon={addon}
                    siblings={addons.length - 1}
                    onChange={upsert}
                    onRemoved={remove}
                    onUploadVersion={(a) => setUpload({ slot: a.slot, target: a })}
                  />
                ))}
                {addons.length > 1 && (
                  <p className="text-xs text-pretty text-muted-foreground">
                    Copper shows one addon per slot: the highest priority one that&rsquo;s
                    on for that person.
                  </p>
                )}
              </div>
            )}
          </SlotSection>
        ))
      )}

      <UploadDialog
        open={upload !== null}
        onOpenChange={(open) => !open && setUpload(null)}
        slots={slots.data ?? []}
        slot={upload?.slot}
        target={upload?.target}
        onUploaded={onUploaded}
      />
    </>
  );
}

function SlotSection({ slot, children }: { slot: AddonSlot; children: React.ReactNode }) {
  const id = `slot-${slot.id}`;
  return (
    <section
      aria-labelledby={id}
      className="grid gap-x-10 gap-y-4 border-t py-8 first-of-type:border-t-0 first-of-type:pt-2 @3xl:grid-cols-[220px_minmax(0,1fr)]"
    >
      <div>
        <h2 id={id} className="text-sm font-medium">
          {slot.name}
        </h2>
        <p className="mt-1 text-sm text-pretty text-muted-foreground">
          {slot.description}
        </p>
        <p className="mt-2 text-xs text-faint">
          Slot <code className="font-mono">{slot.id}</code>
        </p>
      </div>
      <div className="min-w-0 @3xl:max-w-[560px]">{children}</div>
    </section>
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
        <p className="font-medium">Couldn&rsquo;t load addons</p>
        <p className="mt-0.5 text-muted-foreground">{errorMessage(error)}</p>
        <Button variant="outline" size="sm" className="mt-3" onClick={onRetry}>
          Try again
        </Button>
      </div>
    </div>
  );
}
