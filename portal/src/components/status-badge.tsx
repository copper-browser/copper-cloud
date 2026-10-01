import { cn } from "@/lib/utils";

export type Tone = "success" | "neutral" | "warning" | "danger" | "muted";

const dot: Record<Tone, string> = {
  success: "bg-success",
  neutral: "bg-foreground/45",
  warning: "bg-warning",
  danger: "bg-destructive",
  muted: "bg-foreground/25",
};

/** Hairline pill with a 6px status dot. Text carries the meaning; the dot is a scan aid. */
export function StatusBadge({
  tone,
  children,
  className,
}: {
  tone: Tone;
  children: React.ReactNode;
  className?: string;
}) {
  return (
    <span
      className={cn(
        "inline-flex h-5 items-center gap-1.5 rounded-full border px-2 text-xs font-medium whitespace-nowrap",
        tone === "muted" || tone === "danger"
          ? "text-muted-foreground"
          : "text-foreground/85",
        className,
      )}
    >
      <span
        className={cn("size-1.5 shrink-0 rounded-full", dot[tone])}
        aria-hidden="true"
      />
      {children}
    </span>
  );
}
