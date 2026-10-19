import type { Metadata } from "next";

import { CanvasCloudView } from "./view";

export const metadata: Metadata = { title: "Canvas Cloud" };

export default function Page() {
  return <CanvasCloudView />;
}
