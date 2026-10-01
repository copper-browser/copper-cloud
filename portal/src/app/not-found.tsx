import Link from "next/link";

import { BrandMark } from "@/components/brand-mark";

export default function NotFound() {
  return (
    <div className="flex min-h-dvh flex-col items-center justify-center gap-4 p-6 text-center">
      <BrandMark className="size-7" />
      <div>
        <h1 className="text-base font-semibold">Page not found</h1>
        <p className="mt-1 text-sm text-muted-foreground">
          This address isn&rsquo;t part of the admin console.
        </p>
      </div>
      <Link
        href="/"
        className="text-sm font-medium text-foreground underline-offset-4 hover:underline"
      >
        Go to overview
      </Link>
    </div>
  );
}
