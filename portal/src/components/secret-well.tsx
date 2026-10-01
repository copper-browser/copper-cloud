"use client";

import { EyeIcon, EyeOffIcon } from "lucide-react";
import { useState } from "react";

import { CopyButton } from "@/components/copy-button";
import { Button } from "@/components/ui/button";
import { cn } from "@/lib/utils";

interface SecretWellProps {
  label: string;
  value: string;
  /** Hidden until revealed (copy still works while hidden). */
  concealed?: boolean;
  hint?: React.ReactNode;
  className?: string;
  onCopied?: () => void;
  emphasis?: boolean;
}

/**
 * The credential well: keys, link codes and passwords render here in mono,
 * break anywhere, select in one click, and copy with one press.
 */
export function SecretWell({
  label,
  value,
  concealed = false,
  hint,
  className,
  onCopied,
  emphasis = false,
}: SecretWellProps) {
  const [revealed, setRevealed] = useState(!concealed);
  return (
    <div
      className={cn(
        "rounded-md border bg-well",
        emphasis && "border-foreground/15 shadow-[0_1px_0_0_var(--border)]",
        className,
      )}
    >
      <div className="flex h-8 items-center justify-between gap-2 border-b pr-1 pl-3">
        <span className="text-xs font-medium text-muted-foreground">{label}</span>
        <div className="flex items-center gap-0.5">
          {concealed && (
            <Button
              type="button"
              variant="ghost"
              size="sm"
              className="text-muted-foreground"
              onClick={() => setRevealed((r) => !r)}
              aria-pressed={revealed}
            >
              {revealed ? (
                <EyeOffIcon aria-hidden="true" />
              ) : (
                <EyeIcon aria-hidden="true" />
              )}
              {revealed ? "Hide" : "Reveal"}
            </Button>
          )}
          <CopyButton
            value={value}
            label={label.toLowerCase()}
            onCopied={onCopied}
            text="Copy"
            className="text-muted-foreground hover:text-foreground"
          />
        </div>
      </div>
      <div className="px-3 py-2.5">
        {revealed ? (
          <code className="block font-mono text-[12.5px]/5 break-all text-foreground select-all">
            {value}
          </code>
        ) : (
          <code
            className="block font-mono text-[12.5px]/5 tracking-widest text-faint select-none"
            aria-label={`${label} hidden`}
          >
            {"•".repeat(Math.min(48, Math.max(16, Math.floor(value.length / 2))))}
          </code>
        )}
      </div>
      {hint && (
        <div className="border-t px-3 py-2 text-xs text-muted-foreground">{hint}</div>
      )}
    </div>
  );
}
