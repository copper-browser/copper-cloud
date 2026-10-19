"use client";

import { PlusIcon, SearchIcon, XIcon } from "lucide-react";
import { useEffect, useId, useMemo, useRef, useState } from "react";

import { EMAIL_RE } from "@/components/addons/util";
import { Input } from "@/components/ui/input";
import { api, errorMessage } from "@/lib/api";
import { initials } from "@/lib/format";
import type { AddonPerson, User } from "@/lib/types";
import { cn } from "@/lib/utils";

type Option = { kind: "user"; user: User } | { kind: "email"; email: string };

const SEARCH_LIMIT = 8;

interface PeoplePickerProps {
  /** Accessible name of the search field. */
  label: string;
  value: AddonPerson[];
  onChange: (next: AddonPerson[]) => void;
  disabled?: boolean;
}

/**
 * Search the instance's people (`GET users?q=`) or type any email, and keep
 * the picks as removable chips. Emails without an account are allowed: they
 * match once that person signs up.
 */
export function PeoplePicker({ label, value, onChange, disabled }: PeoplePickerProps) {
  const [query, setQuery] = useState("");
  const [open, setOpen] = useState(false);
  const [results, setResults] = useState<User[]>([]);
  const [searching, setSearching] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [active, setActive] = useState(0);
  const inputRef = useRef<HTMLInputElement>(null);
  const inputId = useId();
  const listId = useId();
  const hintId = useId();

  const chosen = useMemo(() => new Set(value.map((p) => p.email.toLowerCase())), [value]);
  const trimmed = query.trim();

  useEffect(() => {
    if (!open) return;
    const controller = new AbortController();
    const timer = setTimeout(
      async () => {
        setSearching(true);
        try {
          const page = await api.users.list(
            { q: trimmed || undefined, limit: SEARCH_LIMIT },
            controller.signal,
          );
          setResults(page.items);
          setError(null);
        } catch (err) {
          if (!controller.signal.aborted) setError(errorMessage(err));
        } finally {
          if (!controller.signal.aborted) setSearching(false);
        }
      },
      trimmed ? 180 : 0,
    );
    return () => {
      clearTimeout(timer);
      controller.abort();
    };
  }, [trimmed, open]);

  const options = useMemo<Option[]>(() => {
    const users = results.filter((u) => !chosen.has(u.email.toLowerCase()));
    const opts: Option[] = users.map((user) => ({ kind: "user", user }));
    const typed = trimmed.toLowerCase();
    if (
      EMAIL_RE.test(typed) &&
      !chosen.has(typed) &&
      !results.some((u) => u.email.toLowerCase() === typed)
    ) {
      opts.push({ kind: "email", email: typed });
    }
    return opts;
  }, [results, chosen, trimmed]);

  useEffect(() => setActive(0), [options.length, trimmed]);

  function add(person: AddonPerson) {
    if (chosen.has(person.email.toLowerCase())) return;
    onChange([...value, person]);
    setQuery("");
    inputRef.current?.focus();
  }

  function choose(option: Option) {
    if (option.kind === "user") {
      add({
        email: option.user.email.toLowerCase(),
        name: option.user.display_name,
        user_id: option.user.id,
      });
    } else {
      add({ email: option.email, name: null, user_id: null });
    }
  }

  function remove(email: string) {
    onChange(value.filter((p) => p.email.toLowerCase() !== email.toLowerCase()));
    inputRef.current?.focus();
  }

  function onKeyDown(event: React.KeyboardEvent<HTMLInputElement>) {
    switch (event.key) {
      case "ArrowDown":
        event.preventDefault();
        if (!open) setOpen(true);
        else setActive((i) => Math.min(i + 1, Math.max(options.length - 1, 0)));
        break;
      case "ArrowUp":
        event.preventDefault();
        setActive((i) => Math.max(i - 1, 0));
        break;
      case "Enter": {
        event.preventDefault();
        const option = open ? options[active] : undefined;
        if (option) choose(option);
        else if (EMAIL_RE.test(trimmed))
          add({ email: trimmed.toLowerCase(), name: null, user_id: null });
        break;
      }
      case "Escape":
        if (open) {
          event.preventDefault();
          setOpen(false);
        }
        break;
    }
  }

  let status: string | null = null;
  if (error) status = error;
  else if (searching && options.length === 0) status = "Searching…";
  else if (options.length === 0) {
    status = trimmed
      ? `Nobody matches “${trimmed}”. Type a full email to add it.`
      : results.length > 0
        ? "Everyone listed is already added."
        : "Nobody has signed up yet. Type an email to add it.";
  }

  const activeId = open && options[active] ? `${listId}-${active}` : undefined;

  return (
    <div className="grid gap-2">
      <div className="relative">
        <SearchIcon
          className="pointer-events-none absolute top-1/2 left-2.5 size-3.5 -translate-y-1/2 text-faint"
          aria-hidden="true"
        />
        <Input
          ref={inputRef}
          id={inputId}
          role="combobox"
          aria-label={label}
          aria-expanded={open}
          aria-controls={listId}
          aria-autocomplete="list"
          aria-activedescendant={activeId}
          aria-describedby={hintId}
          value={query}
          disabled={disabled}
          onChange={(e) => {
            setQuery(e.target.value);
            setOpen(true);
          }}
          onFocus={() => setOpen(true)}
          onBlur={() => setOpen(false)}
          onKeyDown={onKeyDown}
          placeholder="Search people or type an email"
          autoComplete="off"
          spellCheck={false}
          className="pl-8"
        />
        <ul
          id={listId}
          role="listbox"
          aria-label="Matching people"
          hidden={!open}
          onMouseDown={(e) => e.preventDefault()}
          className="absolute top-full left-0 z-30 mt-1 max-h-72 w-full overflow-y-auto rounded-lg bg-popover p-1 text-popover-foreground shadow-md ring-1 ring-foreground/10"
        >
          {options.map((option, i) => (
            <li
              key={option.kind === "user" ? option.user.id : `email:${option.email}`}
              id={`${listId}-${i}`}
              role="option"
              aria-selected={i === active}
              onMouseMove={() => i !== active && setActive(i)}
              onClick={() => choose(option)}
              className={cn(
                "flex min-h-9 cursor-default items-center gap-2.5 rounded-md px-2 py-1.5",
                i === active && "bg-accent text-accent-foreground",
              )}
            >
              {option.kind === "user" ? (
                <>
                  <Avatar name={option.user.display_name} email={option.user.email} />
                  <span className="min-w-0 flex-1">
                    <span className="block truncate font-medium">
                      {option.user.display_name?.trim() || option.user.email}
                    </span>
                    {option.user.display_name?.trim() && (
                      <span className="block truncate text-xs text-muted-foreground">
                        {option.user.email}
                      </span>
                    )}
                  </span>
                  {option.user.disabled && (
                    <span className="shrink-0 text-xs text-faint">Disabled</span>
                  )}
                </>
              ) : (
                <>
                  <span
                    aria-hidden="true"
                    className="flex size-5 shrink-0 items-center justify-center rounded-full border border-dashed text-muted-foreground"
                  >
                    <PlusIcon className="size-3" />
                  </span>
                  <span className="min-w-0 flex-1 truncate">
                    Add <span className="font-medium">{option.email}</span>
                  </span>
                </>
              )}
            </li>
          ))}
          {status && (
            <li role="presentation" className="px-2 py-1.5 text-muted-foreground">
              {status}
            </li>
          )}
        </ul>
      </div>
      <p id={hintId} className="sr-only">
        {value.length === 0 ? "Nobody added yet." : `${value.length} added.`} Use the
        arrow keys to pick a match and Enter to add it.
      </p>

      {value.length > 0 && (
        <ul aria-label="People who get it" className="flex flex-wrap gap-1.5">
          {value.map((p) => (
            <li
              key={p.email}
              className="flex h-7 max-w-full min-w-0 items-center gap-1.5 rounded-full border bg-well pr-0.5 pl-1 text-sm"
              title={p.user_id ? p.email : `${p.email} (no account yet)`}
            >
              <Avatar name={p.name} email={p.email} pending={!p.user_id} />
              <span className="truncate">{p.name?.trim() || p.email}</span>
              <button
                type="button"
                onClick={() => remove(p.email)}
                disabled={disabled}
                aria-label={`Remove ${p.name?.trim() || p.email}`}
                className="flex size-6 shrink-0 items-center justify-center rounded-full text-muted-foreground transition-colors outline-none hover:bg-muted hover:text-foreground focus-visible:ring-2 focus-visible:ring-ring disabled:opacity-50"
              >
                <XIcon className="size-3.5" aria-hidden="true" />
              </button>
            </li>
          ))}
        </ul>
      )}
    </div>
  );
}

function Avatar({
  name,
  email,
  pending = false,
}: {
  name: string | null;
  email: string;
  pending?: boolean;
}) {
  return (
    <span
      aria-hidden="true"
      className={cn(
        "flex size-5 shrink-0 items-center justify-center rounded-full bg-muted text-[9.5px] font-semibold text-muted-foreground shadow-[inset_0_0_0_1px_var(--border)]",
        pending && "bg-transparent shadow-none ring-1 ring-border ring-inset",
      )}
    >
      {initials(name, email)}
    </span>
  );
}
