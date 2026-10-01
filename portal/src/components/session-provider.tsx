"use client";

import {
  createContext,
  useCallback,
  useContext,
  useEffect,
  useMemo,
  useRef,
} from "react";

import { useResource } from "@/hooks/use-resource";
import { api } from "@/lib/api";
import type { Me, Overview } from "@/lib/types";

interface SessionValue {
  me: Me | undefined;
  overview: Overview | undefined;
  overviewError: unknown;
  overviewLoading: boolean;
  /** Re-fetch instance overview (counts, mode). Deduplicated while in flight. */
  refreshOverview: () => Promise<void>;
}

const SessionContext = createContext<SessionValue | null>(null);

/**
 * Loads the admin (`GET me`) and the instance overview once for the console.
 * Any session 401 redirects to /login/ from inside the API client.
 */
export function SessionProvider({ children }: { children: React.ReactNode }) {
  const me = useResource("me", (signal) => api.me({ signal }));
  const overview = useResource("overview", (signal) => api.overview(signal));
  const inflight = useRef<Promise<void> | null>(null);
  const { reload } = overview;

  const refreshOverview = useCallback(() => {
    if (!inflight.current) {
      inflight.current = reload().finally(() => {
        inflight.current = null;
      });
    }
    return inflight.current;
  }, [reload]);

  const value = useMemo<SessionValue>(
    () => ({
      me: me.data,
      overview: overview.data,
      overviewError: overview.error,
      overviewLoading: overview.loading,
      refreshOverview,
    }),
    [me.data, overview.data, overview.error, overview.loading, refreshOverview],
  );

  return <SessionContext.Provider value={value}>{children}</SessionContext.Provider>;
}

export function useSession(): SessionValue {
  const value = useContext(SessionContext);
  if (!value) throw new Error("useSession must be used inside <SessionProvider>");
  return value;
}

/**
 * The shared session, refreshing the overview when a page that shows counts
 * mounts (the provider's own first load covers the very first mount).
 */
export function useFreshOverview() {
  const session = useSession();
  const { refreshOverview, overview } = session;
  const loadedAtMount = useRef(overview !== undefined);
  useEffect(() => {
    if (loadedAtMount.current) void refreshOverview();
  }, [refreshOverview]);
  return session;
}
