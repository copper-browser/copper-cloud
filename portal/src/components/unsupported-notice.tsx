"use client";

import { CircleFadingArrowUpIcon } from "lucide-react";

import { useSession } from "@/components/session-provider";
import { SSO_MIN_VERSION } from "@/lib/api";

/** Shown when the server answers 404 for a route this portal knows about. */
export function UnsupportedNotice({ feature }: { feature: string }) {
  const { overview } = useSession();
  return (
    <div className="flex items-start gap-3 rounded-lg border px-3.5 py-3 text-sm">
      <CircleFadingArrowUpIcon
        className="mt-0.5 size-4 shrink-0 text-muted-foreground"
        aria-hidden="true"
      />
      <div className="min-w-0">
        <p className="font-medium">This server doesn&rsquo;t support {feature} yet</p>
        <p className="mt-0.5 text-pretty text-muted-foreground">
          Update Copper Cloud to {SSO_MIN_VERSION} or later
          {overview?.version ? <> (this one runs {overview.version})</> : null}, then
          reload this page.
        </p>
      </div>
    </div>
  );
}
