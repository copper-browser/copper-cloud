"use client";

import { CircleAlertIcon, SearchIcon, XIcon } from "lucide-react";
import { useRef } from "react";

import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { Skeleton } from "@/components/ui/skeleton";
import { useSlashFocus } from "@/hooks/use-slash-focus";
import { errorMessage } from "@/lib/api";
import { formatCount } from "@/lib/format";
import { cn } from "@/lib/utils";

export interface Column<T> {
  id: string;
  header: React.ReactNode;
  cell: (row: T) => React.ReactNode;
  /** Classes for both th and td: width, responsive hiding. */
  className?: string;
  align?: "left" | "right";
  /** Skeleton bar width while loading, e.g. "w-24". */
  skeleton?: string;
  /** Visually hidden header (action columns). */
  srHeader?: boolean;
}

interface DataTableProps<T> {
  label: string;
  columns: Column<T>[];
  /** Rows after filtering. */
  rows: T[];
  rowKey: (row: T) => string;
  loading: boolean;
  error?: unknown;
  onRetry?: () => void;
  /** Shown when there is no data at all. */
  empty: React.ReactNode;
  /** Shown when data exists but the filter hides all of it. */
  noMatches?: React.ReactNode;
  filtered: boolean;
  rowClassName?: (row: T) => string | undefined;
  footer?: React.ReactNode;
}

/**
 * Dense, fixed-layout table: sticky header, hairline rows, truncating cells.
 * No horizontal scroll: columns carry widths and drop out at narrow widths
 * via container queries on the page column.
 */
export function DataTable<T>({
  label,
  columns,
  rows,
  rowKey,
  loading,
  error,
  onRetry,
  empty,
  noMatches,
  filtered,
  rowClassName,
  footer,
}: DataTableProps<T>) {
  const span = columns.length;
  const align = (c: Column<T>) => (c.align === "right" ? "text-right" : "text-left");

  let body: React.ReactNode;
  if (loading) {
    body = Array.from({ length: 7 }, (_, i) => (
      <tr key={`s${i}`} className="h-10 border-b border-border/70" aria-hidden="true">
        {columns.map((c) => (
          <td key={c.id} className={cn("px-3", c.className, align(c))}>
            {c.skeleton !== "none" && (
              <Skeleton
                className={cn(
                  "inline-block h-3 align-middle",
                  c.skeleton ?? "w-20",
                  i % 3 === 1 && "opacity-70",
                  i % 3 === 2 && "opacity-50",
                )}
              />
            )}
          </td>
        ))}
      </tr>
    ));
  } else if (error && rows.length === 0) {
    body = (
      <StateRow span={span}>
        <div className="flex items-start gap-3">
          <CircleAlertIcon
            className="mt-0.5 size-4 shrink-0 text-destructive"
            aria-hidden="true"
          />
          <div>
            <p className="font-medium text-foreground">
              Couldn&rsquo;t load {label.toLowerCase()}
            </p>
            <p className="mt-0.5 text-muted-foreground">{errorMessage(error)}</p>
            {onRetry && (
              <Button variant="outline" size="sm" className="mt-3" onClick={onRetry}>
                Try again
              </Button>
            )}
          </div>
        </div>
      </StateRow>
    );
  } else if (rows.length === 0) {
    body = <StateRow span={span}>{filtered && noMatches ? noMatches : empty}</StateRow>;
  } else {
    body = rows.map((row) => (
      <tr
        key={rowKey(row)}
        className={cn(
          "group/row h-10 border-b border-border/70 transition-colors hover:bg-muted/60 has-aria-expanded:bg-muted/60",
          rowClassName?.(row),
        )}
      >
        {columns.map((c) => (
          <td key={c.id} className={cn("truncate px-3", c.className, align(c))}>
            {c.cell(row)}
          </td>
        ))}
      </tr>
    ));
  }

  return (
    <div>
      <table
        aria-label={label}
        aria-busy={loading || undefined}
        className="w-full table-fixed border-collapse text-sm"
      >
        <thead>
          <tr>
            {columns.map((c) => (
              <th
                key={c.id}
                scope="col"
                className={cn(
                  "sticky top-0 z-10 h-9 truncate bg-background px-3 text-xs font-medium text-muted-foreground shadow-[inset_0_-1px_0_var(--border)]",
                  c.className,
                  align(c),
                )}
              >
                {c.srHeader ? <span className="sr-only">{c.header}</span> : c.header}
              </th>
            ))}
          </tr>
        </thead>
        <tbody>{body}</tbody>
      </table>
      {footer}
    </div>
  );
}

function StateRow({ span, children }: { span: number; children: React.ReactNode }) {
  return (
    <tr>
      <td colSpan={span} className="px-3 py-10 whitespace-normal">
        <div className="mx-auto max-w-md">{children}</div>
      </td>
    </tr>
  );
}

/** Inline empty state: what this is, and what to do next. */
export function EmptyState({
  icon: Icon,
  title,
  children,
  action,
}: {
  icon: React.ComponentType<{ className?: string; "aria-hidden"?: boolean | "true" }>;
  title: string;
  children?: React.ReactNode;
  action?: React.ReactNode;
}) {
  return (
    <div className="flex items-start gap-3">
      <div className="flex size-8 shrink-0 items-center justify-center rounded-md border bg-well">
        <Icon className="size-4 text-muted-foreground" aria-hidden="true" />
      </div>
      <div className="min-w-0">
        <p className="font-medium text-foreground">{title}</p>
        {children && (
          <p className="mt-0.5 text-pretty text-muted-foreground">{children}</p>
        )}
        {action && <div className="mt-3">{action}</div>}
      </div>
    </div>
  );
}

interface TableToolbarProps {
  filter: string;
  onFilterChange: (value: string) => void;
  placeholder: string;
  /** Segmented status/kind filter, rendered after the input. */
  children?: React.ReactNode;
  /** "12 keys" etc. */
  count?: React.ReactNode;
}

/** Filter input (`/` focuses it) + optional segments + a count. */
export function TableToolbar({
  filter,
  onFilterChange,
  placeholder,
  children,
  count,
}: TableToolbarProps) {
  const inputRef = useRef<HTMLInputElement>(null);
  useSlashFocus(inputRef);
  return (
    <div className="flex flex-wrap items-center gap-2 pb-3">
      <div className="relative w-64 max-w-full">
        <SearchIcon
          className="pointer-events-none absolute top-1/2 left-2.5 size-3.5 -translate-y-1/2 text-faint"
          aria-hidden="true"
        />
        <Input
          ref={inputRef}
          type="search"
          value={filter}
          onChange={(e) => onFilterChange(e.target.value)}
          onKeyDown={(e) => {
            if (e.key === "Escape" && filter) {
              e.preventDefault();
              onFilterChange("");
            } else if (e.key === "Escape") {
              e.currentTarget.blur();
            }
          }}
          placeholder={placeholder}
          aria-label={placeholder}
          aria-keyshortcuts="/"
          className="h-8 px-8 [&::-webkit-search-cancel-button]:hidden"
        />
        {filter ? (
          <button
            type="button"
            onClick={() => {
              onFilterChange("");
              inputRef.current?.focus();
            }}
            className="absolute top-1/2 right-1.5 flex size-5 -translate-y-1/2 items-center justify-center rounded-sm text-faint hover:text-foreground focus-visible:ring-2 focus-visible:ring-ring focus-visible:outline-none"
            aria-label="Clear filter"
          >
            <XIcon className="size-3.5" aria-hidden="true" />
          </button>
        ) : (
          <kbd
            className="pointer-events-none absolute top-1/2 right-2 flex h-4.5 min-w-4.5 -translate-y-1/2 items-center justify-center rounded-sm border px-1 text-[11px] text-faint"
            aria-hidden="true"
          >
            /
          </kbd>
        )}
      </div>
      {children}
      {count !== undefined && (
        <p className="ml-auto text-xs text-muted-foreground tabular-nums">{count}</p>
      )}
    </div>
  );
}

/** "Showing 500 of 812" + load more, only when the list is partial. */
export function LoadMore({
  loaded,
  total,
  noun,
  onLoadMore,
  loading,
}: {
  loaded: number;
  total: number;
  noun: string;
  onLoadMore: () => void;
  loading: boolean;
}) {
  if (loaded >= total) return null;
  return (
    <div className="flex items-center justify-between gap-3 p-3 text-xs text-muted-foreground">
      <span className="tabular-nums">
        Showing {formatCount(loaded)} of {formatCount(total)} {noun}. The filter searches
        loaded rows.
      </span>
      <Button variant="outline" size="sm" onClick={onLoadMore} disabled={loading}>
        {loading ? "Loading…" : "Load more"}
      </Button>
    </div>
  );
}
