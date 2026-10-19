"use client";

import { XIcon } from "lucide-react";
import { useRef, useState } from "react";

import { cn } from "@/lib/utils";

const DOMAIN_RE =
  /^(?=.{1,253}$)(?:[a-z0-9](?:[a-z0-9-]{0,61}[a-z0-9])?\.)+[a-z][a-z0-9-]{0,61}[a-z0-9]$/;

/** `"Ana@Example.com "` → `"example.com"`; `""` when there's nothing to add. */
export function normalizeDomain(raw: string): string {
  const t = raw.trim().toLowerCase().replace(/\.$/, "");
  return t.includes("@") ? (t.split("@").pop() ?? "") : t;
}

export const isDomain = (value: string) => DOMAIN_RE.test(value);

/** The email domain of an address, if it has a usable one. */
export function domainOf(email: string | null | undefined): string | undefined {
  const d = normalizeDomain(email ?? "");
  return email?.includes("@") && isDomain(d) ? d : undefined;
}

/** Read-only domain pill, matching the chips inside {@link DomainInput}. */
export function DomainChip({
  children,
  className,
}: {
  children: React.ReactNode;
  className?: string;
}) {
  return (
    <span
      className={cn(
        "inline-flex h-5.5 max-w-full items-center truncate rounded-sm border bg-well px-1.5 text-xs font-medium text-foreground",
        className,
      )}
    >
      {children}
    </span>
  );
}

interface DomainInputProps {
  id?: string;
  value: string[];
  onChange: (domains: string[]) => void;
  /** Called with a message while the typed text isn't a domain, `undefined` once it is fixed. */
  onInvalid?: (message: string | undefined) => void;
  placeholder?: string;
  invalid?: boolean;
  describedBy?: string;
  disabled?: boolean;
  /** Accessible name for the chip list ("Allowed domains"). */
  label: string;
}

/**
 * Chips for email domains. Enter, comma, space, Tab or leaving the field adds
 * what was typed; pasting a list adds every entry; Backspace on an empty field
 * removes the last chip.
 */
export function DomainInput({
  id,
  value,
  onChange,
  onInvalid,
  placeholder = "example.com",
  invalid,
  describedBy,
  disabled,
  label,
}: DomainInputProps) {
  const [draft, setDraft] = useState("");
  const inputRef = useRef<HTMLInputElement>(null);

  /** Add every entry in `text`; keep whatever doesn't parse in the field. */
  function commit(text: string): boolean {
    const parts = text
      .split(/[\s,;]+/)
      .map(normalizeDomain)
      .filter(Boolean);
    if (parts.length === 0) {
      setDraft("");
      onInvalid?.(undefined);
      return true;
    }
    const good = parts.filter(isDomain);
    const bad = parts.filter((p) => !isDomain(p));
    const next = [...value];
    for (const d of good) if (!next.includes(d)) next.push(d);
    if (next.length !== value.length) onChange(next);
    setDraft(bad.join(" "));
    onInvalid?.(
      bad.length
        ? `“${bad[0]}” isn’t a domain. Use the part after the @, like example.com.`
        : undefined,
    );
    return bad.length === 0;
  }

  function remove(domain: string) {
    onChange(value.filter((d) => d !== domain));
    inputRef.current?.focus();
  }

  return (
    <div
      data-invalid={invalid || undefined}
      data-disabled={disabled || undefined}
      onMouseDown={(e) => {
        if (e.target === e.currentTarget) {
          e.preventDefault();
          inputRef.current?.focus();
        }
      }}
      className="flex min-h-8 w-full min-w-0 cursor-text flex-wrap items-center gap-1 rounded-md border border-input px-1 py-[3px] transition-colors focus-within:border-ring focus-within:ring-3 focus-within:ring-ring/50 data-invalid:border-destructive data-invalid:ring-destructive/20 dark:bg-input/30 data-disabled:pointer-events-none data-disabled:opacity-50"
    >
      {value.length > 0 && (
        <ul aria-label={label} className="contents">
          {value.map((domain) => (
            <li key={domain} className="contents">
              <span className="inline-flex h-5.5 max-w-full min-w-0 items-center gap-0.5 rounded-sm border bg-well pr-0.5 pl-1.5 text-xs font-medium">
                <span className="truncate">{domain}</span>
                <button
                  type="button"
                  onClick={() => remove(domain)}
                  disabled={disabled}
                  aria-label={`Remove ${domain}`}
                  className="flex size-4 shrink-0 items-center justify-center rounded-[3px] text-faint transition-colors outline-none hover:bg-muted hover:text-foreground focus-visible:ring-2 focus-visible:ring-ring"
                >
                  <XIcon className="size-3" aria-hidden="true" />
                </button>
              </span>
            </li>
          ))}
        </ul>
      )}
      <input
        ref={inputRef}
        id={id}
        value={draft}
        disabled={disabled}
        onChange={(e) => {
          setDraft(e.target.value);
          if (invalid) onInvalid?.(undefined);
        }}
        onKeyDown={(e) => {
          if (e.key === "Enter" || e.key === "," || e.key === ";" || e.key === " ") {
            // Enter on an empty field still submits the surrounding form.
            if (draft.trim()) {
              e.preventDefault();
              commit(draft);
            }
          } else if (e.key === "Tab" && draft.trim()) {
            if (!commit(draft)) e.preventDefault();
          } else if (e.key === "Backspace" && !draft && value.length > 0) {
            e.preventDefault();
            onChange(value.slice(0, -1));
          }
        }}
        onBlur={() => {
          if (draft.trim()) commit(draft);
        }}
        onPaste={(e) => {
          const text = e.clipboardData.getData("text");
          if (/[\s,;]/.test(text.trim())) {
            e.preventDefault();
            commit(`${draft} ${text}`);
          }
        }}
        placeholder={value.length ? "Add another" : placeholder}
        autoComplete="off"
        autoCapitalize="none"
        spellCheck={false}
        aria-invalid={invalid || undefined}
        aria-describedby={describedBy}
        className="h-6 min-w-28 flex-1 bg-transparent px-1.5 text-base outline-none placeholder:text-muted-foreground md:text-sm"
      />
    </div>
  );
}
