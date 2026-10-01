"use client";

import {
  FrameIcon,
  KeyRoundIcon,
  LaptopIcon,
  LayoutGridIcon,
  LogOutIcon,
  SettingsIcon,
  UsersIcon,
} from "lucide-react";
import Link from "next/link";
import { usePathname } from "next/navigation";
import { useState } from "react";
import { toast } from "sonner";

import { BrandMark } from "@/components/brand-mark";
import { useSession } from "@/components/session-provider";
import { Button } from "@/components/ui/button";
import { Skeleton } from "@/components/ui/skeleton";
import { Tooltip, TooltipContent, TooltipTrigger } from "@/components/ui/tooltip";
import { api, errorMessage, LOGIN_PATH } from "@/lib/api";
import { displayHost } from "@/lib/format";
import { cn } from "@/lib/utils";

const NAV = [
  { href: "/", label: "Overview", icon: LayoutGridIcon },
  { href: "/keys/", label: "Access keys", icon: KeyRoundIcon },
  { href: "/people/", label: "People", icon: UsersIcon },
  { href: "/devices/", label: "Devices", icon: LaptopIcon },
  { href: "/canvases/", label: "Canvases", icon: FrameIcon },
  { href: "/settings/", label: "Settings", icon: SettingsIcon },
] as const;

const normalize = (path: string) => (path.length > 1 ? path.replace(/\/+$/, "") : path);

export function AppRail() {
  const pathname = normalize(usePathname() ?? "/");
  const { me, overview, overviewError } = useSession();
  const [signingOut, setSigningOut] = useState(false);
  // overviewError only exists after hydration, so reading window here is safe.
  const host = overview?.public_url ?? (overviewError ? window.location.host : undefined);

  async function signOut() {
    setSigningOut(true);
    try {
      await api.logout();
      window.location.replace(LOGIN_PATH);
    } catch (error) {
      setSigningOut(false);
      toast.error(`Couldn't sign out. ${errorMessage(error)}`);
    }
  }

  return (
    <aside className="sticky top-0 flex h-dvh w-60 shrink-0 flex-col border-r bg-rail max-rail:w-14">
      <div className="flex h-12 items-center gap-2.5 px-4 max-rail:justify-center max-rail:px-0">
        <BrandMark />
        <span className="text-sm font-semibold tracking-tight max-rail:sr-only">
          Copper Cloud
        </span>
      </div>

      <nav aria-label="Main" className="flex flex-col gap-px px-2 pt-2">
        {NAV.map(({ href, label, icon: Icon }) => {
          const active = normalize(href) === pathname;
          return (
            <Tooltip key={href}>
              <TooltipTrigger
                render={
                  <Link
                    href={href}
                    aria-current={active ? "page" : undefined}
                    className={cn(
                      "group flex h-8 items-center gap-2.5 rounded-md px-2 text-sm text-muted-foreground transition-colors outline-none hover:bg-rail-active/60 hover:text-foreground focus-visible:ring-2 focus-visible:ring-ring max-rail:justify-center max-rail:px-0",
                      active &&
                        "bg-rail-active font-medium text-foreground hover:bg-rail-active",
                    )}
                  />
                }
              >
                <Icon
                  aria-hidden="true"
                  className={cn(
                    "size-4 shrink-0",
                    active
                      ? "text-primary"
                      : "text-faint group-hover:text-muted-foreground",
                  )}
                />
                <span className="truncate max-rail:sr-only">{label}</span>
              </TooltipTrigger>
              <TooltipContent side="right" className="rail:hidden">
                {label}
              </TooltipContent>
            </Tooltip>
          );
        })}
      </nav>

      <div className="mt-auto border-t p-2">
        <div className="flex items-center gap-2 rounded-md px-2 py-1.5 max-rail:justify-center max-rail:px-0">
          <div className="min-w-0 flex-1 max-rail:hidden">
            {host ? (
              <p className="truncate text-xs font-medium text-foreground" title={host}>
                {displayHost(host)}
              </p>
            ) : (
              <Skeleton className="my-0.5 h-3 w-28" />
            )}
            {me ? (
              <p
                className="truncate text-xs text-muted-foreground"
                title={me.admin.email}
              >
                {me.admin.email}
              </p>
            ) : (
              <Skeleton className="mt-1.5 h-3 w-36" />
            )}
          </div>
          <Tooltip>
            <TooltipTrigger
              render={
                <Button
                  variant="ghost"
                  size="icon-sm"
                  onClick={signOut}
                  disabled={signingOut}
                  aria-label="Sign out"
                  className="text-muted-foreground hover:text-foreground"
                />
              }
            >
              <LogOutIcon aria-hidden="true" />
            </TooltipTrigger>
            <TooltipContent side="right">Sign out</TooltipContent>
          </Tooltip>
        </div>
      </div>
    </aside>
  );
}
