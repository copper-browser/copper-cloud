import type { Metadata } from "next";

import { PeopleView } from "./view";

export const metadata: Metadata = { title: "People" };

export default function Page() {
  return <PeopleView />;
}
