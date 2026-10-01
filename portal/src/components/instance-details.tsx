"use client";

import { LockKeyholeIcon, LockOpenIcon } from "lucide-react";

import { CopyButton } from "@/components/copy-button";
import { Fingerprint } from "@/components/fingerprint";
import { Skeleton } from "@/components/ui/skeleton";
import { formatUptime } from "@/lib/format";
import type { AccessMode, Overview, TlsMode } from "@/lib/types";
import { cn } from "@/lib/utils";

const TLS_LABEL: Record<TlsMode, string> = {
  "self-signed": "Self-signed, pinned",
  acme: "ACME (publicly trusted)",
  off: "Off (TLS ends at a proxy)",
};

export function AccessModeBadge({
  mode,
  className,
}: {
  mode: AccessMode;
  className?: string;
}) {
  const Icon = mode === "directory" ? LockKeyholeIcon : LockOpenIcon;
  return (
    <span
      className={cn(
        "inline-flex h-5 items-center gap-1 rounded-full border px-2 text-xs font-medium text-foreground/85",
        className,
      )}
    >
      <Icon className="size-3 text-muted-foreground" aria-hidden="true" />
      {mode === "directory" ? "Directory" : "Open"}
    </span>
  );
}

function Row({ label, children }: { label: string; children: React.ReactNode }) {
  return (
    <div className="grid grid-cols-[88px_minmax(0,1fr)] items-center gap-3 px-4 py-2">
      <dt className="text-muted-foreground">{label}</dt>
      <dd className="min-w-0">{children}</dd>
    </div>
  );
}

/** Public URL, version, uptime, access mode, TLS and fingerprint. */
export function InstanceDetails({
  overview,
  accessAction,
}: {
  overview: Overview | undefined;
  /** Optional trailing control on the access row (e.g. a "Change" link). */
  accessAction?: React.ReactNode;
}) {
  if (!overview) {
    return (
      <dl className="py-1.5" aria-busy="true">
        {["w-40", "w-28", "w-16", "w-20", "w-44"].map((w, i) => (
          <div
            key={i}
            className="grid grid-cols-[88px_minmax(0,1fr)] items-center gap-3 px-4 py-2.5"
          >
            <Skeleton className="h-3 w-16" />
            <Skeleton className={cn("h-3", w)} />
          </div>
        ))}
      </dl>
    );
  }
  const { tls } = overview;
  return (
    <dl className="py-1.5 text-sm">
      <Row label="Public URL">
        <div className="flex items-center gap-1">
          <code className="truncate font-mono text-[12.5px]" title={overview.public_url}>
            {overview.public_url}
          </code>
          <CopyButton value={overview.public_url} label="public URL" className="-my-1" />
        </div>
      </Row>
      <Row label="Version">
        <span className="font-mono text-[12.5px]">{overview.version}</span>
      </Row>
      <Row label="Uptime">
        <span className="tabular-nums">{formatUptime(overview.uptime_s)}</span>
      </Row>
      <Row label="Access">
        <div className="flex items-center gap-2">
          <AccessModeBadge mode={overview.access_mode} />
          {accessAction}
        </div>
      </Row>
      <Row label="TLS">{TLS_LABEL[tls.mode] ?? tls.mode}</Row>
      {tls.fingerprint ? (
        <div className="px-4 pt-1.5 pb-2.5">
          <p className="mb-1.5 text-muted-foreground">
            Certificate fingerprint (SHA-256)
          </p>
          <div className="rounded-md border bg-well px-3 py-2">
            <Fingerprint value={tls.fingerprint} />
          </div>
        </div>
      ) : (
        <Row label="Fingerprint">
          <span className="text-muted-foreground">
            {tls.mode === "acme"
              ? "Not needed, publicly trusted"
              : "Certificate not readable"}
          </span>
        </Row>
      )}
    </dl>
  );
}
