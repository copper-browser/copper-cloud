import type { Metadata } from "next";

import { SsoView } from "./view";

export const metadata: Metadata = { title: "Single sign-on" };

export default function Page() {
  return <SsoView />;
}
