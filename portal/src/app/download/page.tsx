import type { Metadata } from "next";

import { DownloadView } from "./view";

export const metadata: Metadata = { title: "Get Copper" };

export default function DownloadPage() {
  return <DownloadView />;
}
