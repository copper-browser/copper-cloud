"use client";

import {
  ActivityIcon,
  ArrowUpRightIcon,
  CircleAlertIcon,
  FrameIcon,
  KeyRoundIcon,
  LaptopIcon,
  LinkIcon,
  LogInIcon,
  type LucideIcon,
  PlusIcon,
  RectangleEllipsisIcon,
  SettingsIcon,
  UserIcon,
  UserMinusIcon,
  UserXIcon,
  KeyIcon,
} from "lucide-react";
import Link from "next/link";

import { InstanceDetails } from "@/components/instance-details";
import { PageHeader } from "@/components/page-header";
import { RelativeTime } from "@/components/relative-time";
import { useFreshOverview } from "@/components/session-provider";
import { Button } from "@/components/ui/button";
import { Skeleton } from "@/components/ui/skeleton";
import { useResource } from "@/hooks/use-resource";
import { api, errorMessage } from "@/lib/api";
import { type AuditIcon, describeAudit } from "@/lib/audit";
import { displayHost, formatCount, formatUptime } from "@/lib/format";
import type { AuditEntry, Counts } from "@/lib/types";
import { cn } from "@/lib/utils";

const AUDIT_ICONS: Record<AuditIcon, LucideIcon> = {
  session: LogInIcon,
  password: RectangleEllipsisIcon,
  settings: SettingsIcon,
  key: KeyRoundIcon,
  "key-off": KeyIcon,
  user: UserIcon,
  "user-off": UserMinusIcon,
  "user-x": UserXIcon,
  device: LaptopIcon,
  canvas: FrameIcon,
  pairing: LinkIcon,
  other: ActivityIcon,
};

const STATS: {
  key: keyof Counts;
  label: string;
  href?: string;
  live?: boolean;
}[] = [
  { key: "users", label: "People", href: "/people/" },
  { key: "devices", label: "Devices", href: "/devices/" },
  { key: "canvases", label: "Canvases", href: "/canvases/" },
  { key: "access_keys", label: "Active keys", href: "/keys/" },
  { key: "live_rooms", label: "Live rooms", live: true },
  { key: "live_peers", label: "Live peers", live: true },
];

export function OverviewView() {
  const { overview, overviewError, me } = useFreshOverview();
  const audit = useResource("audit:20", (signal) => api.audit({ limit: 20 }, signal));

  const subtitle = overview ? (
    <>
      {displayHost(overview.public_url)}
      <span className="px-1.5 text-faint">·</span>
      {overview.access_mode === "directory" ? "Directory access" : "Open access"}
      <span className="px-1.5 text-faint">·</span>
      up {formatUptime(overview.uptime_s)}
    </>
  ) : (
    <Skeleton className="mt-1.5 h-3 w-64" />
  );

  return (
    <>
      <PageHeader
        title="Overview"
        description={subtitle}
        action={
          <Button render={<Link href="/keys/?new=1" />} nativeButton={false}>
            <PlusIcon aria-hidden="true" />
            New access key
          </Button>
        }
      />

      {overviewError && !overview ? (
        <div className="flex items-start gap-2 rounded-lg border px-4 py-3 text-sm">
          <CircleAlertIcon
            className="mt-0.5 size-4 text-destructive"
            aria-hidden="true"
          />
          <p>
            <span className="font-medium">
              Couldn&rsquo;t load the instance overview.
            </span>{" "}
            <span className="text-muted-foreground">{errorMessage(overviewError)}</span>
          </p>
        </div>
      ) : (
        <StatStrip counts={overview?.counts} />
      )}

      <div className="mt-6 grid items-start gap-6 @4xl:grid-cols-[minmax(0,1fr)_352px]">
        <section aria-labelledby="activity-title" className="rounded-lg border">
          <header className="flex h-11 items-center justify-between border-b px-4">
            <h2 id="activity-title" className="text-sm font-medium">
              Recent activity
            </h2>
            <span className="text-xs text-muted-foreground">
              Admin actions, newest first
            </span>
          </header>
          <ActivityList
            loading={audit.loading}
            error={audit.error}
            onRetry={audit.reload}
            entries={audit.data?.items ?? []}
            selfId={me?.admin.id}
          />
        </section>

        <section
          aria-labelledby="instance-title"
          className="order-first rounded-lg border @4xl:sticky @4xl:top-6 @4xl:order-0"
        >
          <header className="flex h-11 items-center border-b px-4">
            <h2 id="instance-title" className="text-sm font-medium">
              Instance
            </h2>
          </header>
          <InstanceDetails
            overview={overview}
            accessAction={
              <Link
                href="/settings/"
                className="text-xs text-muted-foreground underline-offset-4 hover:text-foreground hover:underline"
              >
                Change
              </Link>
            }
          />
        </section>
      </div>
    </>
  );
}

function StatStrip({ counts }: { counts: Counts | undefined }) {
  const liveNow = (counts?.live_peers ?? 0) > 0;
  return (
    <div className="grid grid-cols-3 gap-px overflow-hidden rounded-lg border bg-border @3xl:grid-cols-6">
      {STATS.map((s) => {
        const value = counts?.[s.key];
        const inner = (
          <>
            <span className="flex items-center gap-1.5 text-xs text-muted-foreground">
              {s.live && (
                <span
                  className={cn(
                    "relative size-1.5 rounded-full",
                    liveNow ? "bg-success" : "bg-foreground/25",
                  )}
                  aria-hidden="true"
                >
                  {liveNow && (
                    <span className="absolute inset-0 animate-ping rounded-full bg-success/60 motion-reduce:hidden" />
                  )}
                </span>
              )}
              {s.label}
              {s.href && (
                <ArrowUpRightIcon
                  className="ml-auto size-3.5 text-faint opacity-0 transition-opacity group-hover:opacity-100 group-focus-visible:opacity-100"
                  aria-hidden="true"
                />
              )}
            </span>
            {value === undefined ? (
              <Skeleton className="mt-2.5 mb-1 h-5 w-10" />
            ) : (
              <span className="mt-1 block text-[22px]/8 font-semibold tracking-[-0.02em] tabular-nums">
                {formatCount(value)}
              </span>
            )}
          </>
        );
        const cell = cn("group block px-4 py-3", s.live ? "bg-well" : "bg-background");
        return s.href ? (
          <Link
            key={s.key}
            href={s.href}
            className={cn(
              cell,
              "transition-colors outline-none hover:bg-muted/60 focus-visible:ring-2 focus-visible:ring-ring focus-visible:ring-inset",
            )}
          >
            {inner}
          </Link>
        ) : (
          <div key={s.key} className={cell}>
            {inner}
          </div>
        );
      })}
    </div>
  );
}

function ActivityList({
  loading,
  error,
  onRetry,
  entries,
  selfId,
}: {
  loading: boolean;
  error: unknown;
  onRetry: () => void;
  entries: AuditEntry[];
  selfId: string | undefined;
}) {
  if (loading) {
    return (
      <ul aria-busy="true">
        {Array.from({ length: 8 }, (_, i) => (
          <li
            key={i}
            className="flex h-10 items-center gap-3 border-b border-border/70 px-4 last:border-0"
          >
            <Skeleton className="size-4 rounded-sm" />
            <Skeleton className={cn("h-3", ["w-64", "w-48", "w-56", "w-40"][i % 4])} />
            <Skeleton className="ml-auto h-3 w-12" />
          </li>
        ))}
      </ul>
    );
  }
  if (error) {
    return (
      <div className="flex items-start gap-3 px-4 py-6 text-sm">
        <CircleAlertIcon
          className="mt-0.5 size-4 shrink-0 text-destructive"
          aria-hidden="true"
        />
        <div>
          <p className="font-medium">Couldn&rsquo;t load activity</p>
          <p className="mt-0.5 text-muted-foreground">{errorMessage(error)}</p>
          <Button variant="outline" size="sm" className="mt-3" onClick={onRetry}>
            Try again
          </Button>
        </div>
      </div>
    );
  }
  if (entries.length === 0) {
    return (
      <p className="px-4 py-8 text-sm text-pretty text-muted-foreground">
        No admin activity yet. Creating keys, changing settings and managing people all
        land in this log.
      </p>
    );
  }
  return (
    <ul>
      {entries.map((entry) => {
        const self = !!selfId && entry.admin_id === selfId;
        const line = describeAudit(entry, self);
        const Icon = AUDIT_ICONS[line.icon];
        const actor = self ? "You" : (entry.admin_email ?? "A deleted admin");
        return (
          <li
            key={entry.id}
            className="flex h-10 items-center gap-3 border-b border-border/70 px-4 last:border-0"
          >
            <Icon className="size-3.5 shrink-0 text-faint" aria-hidden="true" />
            <p className="min-w-0 flex-1 truncate text-muted-foreground">
              <span className="text-foreground">{actor}</span> {line.verb}
              {line.object && (
                <>
                  {" "}
                  <span className="font-medium text-foreground">{line.object}</span>
                </>
              )}
              {line.suffix && <> {line.suffix}</>}
            </p>
            <RelativeTime
              value={entry.at}
              className="shrink-0 text-xs text-muted-foreground"
            />
          </li>
        );
      })}
    </ul>
  );
}
