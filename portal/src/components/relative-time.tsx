"use client";

import { Tooltip, TooltipContent, TooltipTrigger } from "@/components/ui/tooltip";
import { useNow } from "@/hooks/use-now";
import { fullTimestamp, relativeTime } from "@/lib/format";
import { cn } from "@/lib/utils";

interface RelativeTimeProps {
  value: string | null | undefined;
  /** Shown when `value` is null. */
  empty?: string;
  className?: string;
}

/** "4m ago" with the full local timestamp in a tooltip. */
export function RelativeTime({ value, empty = "—", className }: RelativeTimeProps) {
  const now = useNow() || Date.now();
  if (!value) return <span className={cn("text-faint", className)}>{empty}</span>;
  return (
    <Tooltip>
      <TooltipTrigger
        render={
          <time
            dateTime={value}
            className={cn("cursor-default tabular-nums", className)}
          />
        }
      >
        {relativeTime(value, now)}
      </TooltipTrigger>
      <TooltipContent>{fullTimestamp(value)}</TooltipContent>
    </Tooltip>
  );
}
