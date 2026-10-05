import type { Metadata } from "next";

import { IntelligenceView } from "./view";

export const metadata: Metadata = { title: "AI keys" };

export default function Page() {
  return <IntelligenceView />;
}
