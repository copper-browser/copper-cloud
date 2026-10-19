import type { Metadata } from "next";

import { AddonsView } from "./view";

export const metadata: Metadata = { title: "Addons" };

export default function Page() {
  return <AddonsView />;
}
