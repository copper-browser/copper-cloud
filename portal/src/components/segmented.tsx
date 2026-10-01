"use client";

import { cn } from "@/lib/utils";

interface SegmentedProps<V extends string> {
  label: string;
  value: V;
  onChange: (value: V) => void;
  options: { value: V; label: string; count?: number }[];
}

/** Compact toggle group for list filters (status, kind). */
export function Segmented<V extends string>({
  label,
  value,
  onChange,
  options,
}: SegmentedProps<V>) {
  return (
    <div
      role="group"
      aria-label={label}
      className="flex h-8 items-center gap-0.5 rounded-md border p-0.5"
    >
      {options.map((o) => {
        const active = o.value === value;
        return (
          <button
            key={o.value}
            type="button"
            aria-pressed={active}
            onClick={() => onChange(o.value)}
            className={cn(
              "flex h-full items-center gap-1.5 rounded-[5px] px-2 text-xs font-medium text-muted-foreground transition-colors outline-none hover:text-foreground focus-visible:ring-2 focus-visible:ring-ring",
              active && "bg-muted text-foreground shadow-[inset_0_0_0_1px_var(--border)]",
            )}
          >
            {o.label}
            {o.count !== undefined && (
              <span
                className={cn(
                  "tabular-nums",
                  active ? "text-muted-foreground" : "text-faint",
                )}
              >
                {o.count}
              </span>
            )}
          </button>
        );
      })}
    </div>
  );
}
