import { MOCK_MODE } from "@/lib/api";

/** Thin notice so mock data is never mistaken for a real instance. */
export function MockBanner() {
  if (!MOCK_MODE) return null;
  return (
    <div className="border-b bg-warning-surface px-6 py-1.5 text-center text-xs text-warning-foreground">
      Mock data. Set <code className="font-mono">NEXT_PUBLIC_MOCK=0</code> to talk to a
      real copper-cloud.
    </div>
  );
}
