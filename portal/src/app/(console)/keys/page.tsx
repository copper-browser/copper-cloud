import type { Metadata } from "next";

import { AccessKeysView } from "./view";

export const metadata: Metadata = { title: "Access keys" };

export default function Page() {
  return <AccessKeysView />;
}
