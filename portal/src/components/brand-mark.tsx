import { cn } from "@/lib/utils";

/** Copper Cloud mark: a copper tile with an open ring and a node. */
export function BrandMark({ className }: { className?: string }) {
  return (
    <svg
      viewBox="0 0 20 20"
      aria-hidden="true"
      className={cn("size-5 shrink-0 text-primary", className)}
    >
      <rect width="20" height="20" rx="5" fill="currentColor" />
      <path
        d="M13.6 6.6A4.8 4.8 0 1 0 13.6 13.4"
        fill="none"
        stroke="var(--primary-foreground)"
        strokeWidth="2.1"
        strokeLinecap="round"
      />
      <circle cx="14.6" cy="10" r="1.35" fill="var(--primary-foreground)" />
    </svg>
  );
}
