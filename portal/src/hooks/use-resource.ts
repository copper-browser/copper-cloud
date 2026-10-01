"use client";

import { useCallback, useEffect, useRef, useState } from "react";

export interface Resource<T> {
  data: T | undefined;
  error: unknown;
  /** True until the first response (success or failure). */
  loading: boolean;
  /** True while a reload runs over existing data. */
  refreshing: boolean;
  reload: () => Promise<void>;
  /** Local update after a mutation (no request). */
  mutate: (update: (prev: T | undefined) => T | undefined) => void;
}

/**
 * Minimal client-side data hook: loads once per `key`, aborts on unmount or
 * key change, keeps data while reloading. `key = null` skips loading.
 */
export function useResource<T>(
  key: string | null,
  loader: (signal: AbortSignal) => Promise<T>,
): Resource<T> {
  const [state, setState] = useState<{
    key: string | null;
    data: T | undefined;
    error: unknown;
    loading: boolean;
    refreshing: boolean;
  }>({
    key,
    data: undefined,
    error: undefined,
    loading: key !== null,
    refreshing: false,
  });

  const loaderRef = useRef(loader);
  loaderRef.current = loader;
  const controllerRef = useRef<AbortController | null>(null);

  const run = useCallback(
    async (mode: "initial" | "reload") => {
      if (key === null) return;
      controllerRef.current?.abort();
      const controller = new AbortController();
      controllerRef.current = controller;
      setState((s) =>
        mode === "initial"
          ? { key, data: undefined, error: undefined, loading: true, refreshing: false }
          : { ...s, refreshing: true },
      );
      try {
        const data = await loaderRef.current(controller.signal);
        if (controller.signal.aborted) return;
        setState({ key, data, error: undefined, loading: false, refreshing: false });
      } catch (error) {
        if (controller.signal.aborted) return;
        setState((s) => ({ ...s, key, error, loading: false, refreshing: false }));
      }
    },
    [key],
  );

  useEffect(() => {
    void run("initial");
    return () => controllerRef.current?.abort();
  }, [run]);

  const reload = useCallback(() => run("reload"), [run]);
  const mutate = useCallback(
    (update: (prev: T | undefined) => T | undefined) =>
      setState((s) => ({ ...s, data: update(s.data) })),
    [],
  );

  return {
    data: state.data,
    error: state.error,
    loading: state.loading,
    refreshing: state.refreshing,
    reload,
    mutate,
  };
}
