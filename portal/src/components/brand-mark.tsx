import Image from "next/image";

import { cn } from "@/lib/utils";

/** The Copper logo (the copper plate), served from the app icon `src/app/icon.png`. */
export function BrandMark({ className }: { className?: string }) {
  return (
    <Image
      src="/icon.png"
      alt=""
      aria-hidden="true"
      width={128}
      height={128}
      className={cn("size-5 shrink-0", className)}
    />
  );
}
