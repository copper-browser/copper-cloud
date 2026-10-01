"use client";

import { useCallback, useState } from "react";

import { useResource } from "@/hooks/use-resource";
import { errorMessage, MAX_PAGE } from "@/lib/api";
import type { Page } from "@/lib/types";
import { toast } from "sonner";

export interface List<T> {
  items: T[];
  total: number;
  loading: boolean;
  error: unknown;
  reload: () => Promise<void>;
  loadMore: () => Promise<void>;
  loadingMore: boolean;
  /** Replace rows locally after a mutation. */
  update: (fn: (items: T[]) => T[], totalDelta?: number) => void;
}

/**
 * A paginated admin list loaded `MAX_PAGE` rows at a time. `key = null` means
 * "not ready yet" (e.g. URL params unread): it reports loading and fetches nothing.
 */
export function useList<T>(
  key: string | null,
  fetchPage: (offset: number, limit: number, signal?: AbortSignal) => Promise<Page<T>>,
): List<T> {
  const resource = useResource<Page<T>>(key, (signal) => fetchPage(0, MAX_PAGE, signal));
  const [loadingMore, setLoadingMore] = useState(false);
  const { data, mutate } = resource;

  const loadMore = useCallback(async () => {
    if (!data) return;
    setLoadingMore(true);
    try {
      const next = await fetchPage(data.items.length, MAX_PAGE);
      mutate((prev) =>
        prev
          ? { ...prev, items: [...prev.items, ...next.items], total: next.total }
          : next,
      );
    } catch (error) {
      toast.error(errorMessage(error));
    } finally {
      setLoadingMore(false);
    }
  }, [data, fetchPage, mutate]);

  const update = useCallback(
    (fn: (items: T[]) => T[], totalDelta = 0) =>
      mutate((prev) =>
        prev ? { ...prev, items: fn(prev.items), total: prev.total + totalDelta } : prev,
      ),
    [mutate],
  );

  return {
    items: data?.items ?? [],
    total: data?.total ?? 0,
    loading: key === null || resource.loading,
    error: resource.error,
    reload: resource.reload,
    loadMore,
    loadingMore,
    update,
  };
}
