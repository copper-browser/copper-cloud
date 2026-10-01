import { CopyButton } from "@/components/copy-button";
import { fingerprintPairs } from "@/lib/format";
import { cn } from "@/lib/utils";

/**
 * SHA-256 certificate fingerprint as colon-separated pairs. Groups of eight
 * pairs never break internally, so it wraps to 1, 2 or 4 even lines.
 */
export function Fingerprint({ value, className }: { value: string; className?: string }) {
  const pairs = fingerprintPairs(value);
  const groups: string[] = [];
  for (let i = 0; i < pairs.length; i += 8) groups.push(pairs.slice(i, i + 8).join(":"));
  return (
    <div className={cn("flex items-start gap-1", className)}>
      <code
        className="min-w-0 flex-1 font-mono text-[11.5px]/[18px] text-foreground/85 select-all"
        aria-label="SHA-256 fingerprint"
      >
        {groups.map((group, i) => (
          <span key={i} className="inline-block whitespace-nowrap">
            {group}
            {i < groups.length - 1 ? ":" : ""}
          </span>
        ))}
      </code>
      <CopyButton value={value} label="fingerprint" className="-mt-1" />
    </div>
  );
}
