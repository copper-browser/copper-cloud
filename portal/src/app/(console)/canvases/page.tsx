import type { Metadata } from "next";

import { CanvasesView } from "./view";

export const metadata: Metadata = { title: "Canvases" };

export default function Page() {
  return <CanvasesView />;
}
