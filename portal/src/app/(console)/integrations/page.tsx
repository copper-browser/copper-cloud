import type { Metadata } from "next";

import { IntegrationsView } from "./view";

export const metadata: Metadata = { title: "Agent access" };

export default function Page() {
  return <IntegrationsView />;
}
