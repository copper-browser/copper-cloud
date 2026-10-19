"use client";

import { ArrowUpRightIcon, CircleAlertIcon, CopyIcon, DownloadIcon } from "lucide-react";
import { useEffect, useState } from "react";
import { toast } from "sonner";

import { BrandMark } from "@/components/brand-mark";
import { MockBanner } from "@/components/mock-banner";
import { SecretWell } from "@/components/secret-well";
import { StatusBadge } from "@/components/status-badge";
import { Button, buttonVariants } from "@/components/ui/button";
import { Skeleton } from "@/components/ui/skeleton";
import { useCopy } from "@/hooks/use-copy";
import { cn } from "@/lib/utils";

// Plain links only: this page never fetches anything, from GitHub or the server.
const MAC_DMG = "https://github.com/copper-browser/Copper/releases/latest";
const MAC_BREW = "brew install --cask copper-browser/copper/copper";
const LINUX_RELEASES = "https://github.com/copper-browser/copper-linux/releases/latest";
const LINUX_INSTALL = `curl -fsSL ${LINUX_RELEASES}/download/install.sh | sh`;
const WINDOWS_RELEASES =
  "https://github.com/copper-browser/copper-windows/releases/latest";

type Os = "mac" | "linux" | "windows";
type Arch = "arm" | "x86" | null;
interface Detected {
  /** `null`: a phone/tablet or something we don't recognise. */
  os: Os | null;
  mobile: boolean;
  arch: Arch;
}

interface UADataLike {
  platform?: string;
  mobile?: boolean;
  getHighEntropyValues?: (hints: string[]) => Promise<{ architecture?: string }>;
}

function detect(): Detected {
  const nav = navigator as Navigator & { userAgentData?: UADataLike };
  const data = nav.userAgentData;
  const platform = (data?.platform ?? "").toLowerCase();
  const ua = navigator.userAgent;
  const mobile =
    data?.mobile === true ||
    /android|iphone|ipad|ipod|mobile/i.test(ua) ||
    // iPadOS reports itself as a Mac.
    (/macintosh/i.test(ua) && navigator.maxTouchPoints > 1);
  let os: Os | null = null;
  if (!mobile) {
    if (platform.includes("mac") || /mac os x|macintosh/i.test(ua)) os = "mac";
    else if (platform.includes("win") || /windows/i.test(ua)) os = "windows";
    else if (
      platform.includes("linux") ||
      platform.includes("chrome os") ||
      platform.includes("chromeos") ||
      /linux|x11|cros/i.test(ua)
    )
      os = "linux";
  }
  // The UA string can't tell Apple silicon from Intel, so arch only comes from
  // userAgentData's high-entropy hints.
  return { os, mobile, arch: null };
}

/** `k` and `fp` from `#k=…&fp=…`. Read here only; never sent anywhere. */
interface LinkCode {
  k: string;
  fp: string | null;
  valid: boolean;
}

function readLinkCode(): LinkCode | null {
  const hash = window.location.hash.replace(/^#/, "");
  if (!hash) return null;
  const params = new URLSearchParams(hash);
  const k = params.get("k");
  if (k === null) return null;
  const fp = params.get("fp");
  const valid =
    /^[A-Za-z0-9._~-]{8,512}$/.test(k) &&
    (fp === null || /^[A-Za-z0-9:._-]{8,256}$/.test(fp));
  return { k, fp, valid };
}

/** `copper-cloud://HOST:PORT/#k=…&fp=…` for this server. */
function linkCodeUrl(code: LinkCode): string {
  const { hostname, port, protocol } = window.location;
  const host =
    hostname.includes(":") && !hostname.startsWith("[") ? `[${hostname}]` : hostname;
  const explicit = port || (protocol === "http:" ? "80" : "443");
  const fragment = new URLSearchParams({ k: code.k });
  if (code.fp) fragment.set("fp", code.fp);
  return `copper-cloud://${host}:${explicit}/#${fragment.toString()}`;
}

const OS_NAMES: Record<Os, string> = { mac: "Mac", linux: "Linux", windows: "Windows" };
const ORDER: Os[] = ["mac", "linux", "windows"];

export function DownloadView() {
  const [detected, setDetected] = useState<Detected | null>(null);
  const [host, setHost] = useState<string | null>(null);
  const [code, setCode] = useState<LinkCode | null>(null);

  useEffect(() => {
    setHost(window.location.host);
    const found = detect();
    setDetected(found);
    const data = (navigator as Navigator & { userAgentData?: UADataLike }).userAgentData;
    if (found.os && data?.getHighEntropyValues) {
      data
        .getHighEntropyValues(["architecture"])
        .then(({ architecture }) => {
          const arch: Arch =
            architecture === "arm" ? "arm" : architecture === "x86" ? "x86" : null;
          if (arch) setDetected((d) => (d ? { ...d, arch } : d));
        })
        .catch(() => {});
    }
    const sync = () => setCode(readLinkCode());
    sync();
    window.addEventListener("hashchange", sync);
    return () => window.removeEventListener("hashchange", sync);
  }, []);

  const primary = detected?.os ?? null;
  const others = ORDER.filter((os) => os !== primary);

  return (
    <div className="flex min-h-dvh flex-col bg-rail">
      <MockBanner />
      <main className="flex flex-1 flex-col items-center px-4 py-12 sm:py-16">
        <div className="w-full max-w-[520px]">
          <div className="mb-6 flex items-center gap-2.5">
            <BrandMark className="size-6" />
            <span className="text-[15px] font-semibold tracking-tight">Copper</span>
          </div>

          <h1 className="text-xl font-semibold tracking-tight text-balance">
            Get Copper
          </h1>
          <p className="mt-1.5 text-pretty text-muted-foreground">
            {code ? (
              <>
                Install Copper, then link it to <HostName host={host} />. Your access is
                already set up.
              </>
            ) : (
              <>
                Copper is a web browser with an agent built in. Install it on your
                computer.
              </>
            )}
          </p>

          <ol className="mt-8 grid gap-4">
            <Step n={1} title="Install Copper" showNumber={!!code}>
              {!detected ? (
                <div
                  aria-busy="true"
                  aria-label="Checking your system"
                  className="grid gap-3"
                >
                  <Skeleton className="h-4 w-48" />
                  <Skeleton className="h-9 w-full" />
                  <Skeleton className="h-3.5 w-64" />
                </div>
              ) : primary ? (
                <>
                  <Featured os={primary} arch={detected.arch} />
                  <div className="mt-5 border-t pt-4">
                    <h3 className="text-xs font-medium text-muted-foreground">
                      Other systems
                    </h3>
                    <ul className="mt-1">
                      {others.map((os) => (
                        <OtherRow key={os} os={os} />
                      ))}
                    </ul>
                  </div>
                </>
              ) : (
                <>
                  <p className="text-pretty text-muted-foreground">
                    {detected.mobile
                      ? "Copper runs on Mac, Linux and Windows computers. Open this page on one to install it."
                      : "Choose your system."}
                  </p>
                  <ul className="mt-2">
                    {ORDER.map((os) => (
                      <OtherRow key={os} os={os} />
                    ))}
                  </ul>
                </>
              )}
            </Step>

            {code ? (
              <Step
                n={2}
                title={
                  <>
                    Link Copper to <HostName host={host} />
                  </>
                }
                showNumber
              >
                <LinkStep code={code} />
              </Step>
            ) : (
              <li className="list-none px-1 text-pretty text-muted-foreground">
                Joining a team on Copper Cloud? Ask your admin for a link. It sets up
                Copper and your access in one go.
              </li>
            )}
          </ol>
        </div>
      </main>
    </div>
  );
}

function HostName({ host }: { host: string | null }) {
  return (
    <span className="font-medium break-all text-foreground">{host ?? "this server"}</span>
  );
}

function Step({
  n,
  title,
  showNumber,
  children,
}: {
  n: number;
  title: React.ReactNode;
  showNumber: boolean;
  children: React.ReactNode;
}) {
  return (
    <li className="list-none rounded-xl border bg-background p-5 shadow-[0_1px_2px_rgb(0_0_0/0.04),0_8px_24px_-12px_rgb(0_0_0/0.12)] sm:p-6">
      <h2 className="flex items-center gap-2.5 text-[15px] font-semibold tracking-tight">
        {showNumber && (
          <span
            aria-hidden="true"
            className="flex size-5 shrink-0 items-center justify-center rounded-full bg-muted text-[11px] font-semibold text-muted-foreground tabular-nums"
          >
            {n}
          </span>
        )}
        <span className="min-w-0">
          {showNumber && <span className="sr-only">Step {n}: </span>}
          {title}
        </span>
      </h2>
      <div className="mt-4">{children}</div>
    </li>
  );
}

function archHint(os: Os, arch: Arch): string {
  if (os === "mac") {
    if (arch === "arm") return "Opens the latest release. Choose the Apple silicon .dmg.";
    if (arch === "x86") return "Opens the latest release. Choose the Intel .dmg.";
    return "Opens the latest release on GitHub. Choose the .dmg for your Mac.";
  }
  if (arch === "arm") return "Opens the latest release. Choose the ARM installer.";
  if (arch === "x86") return "Opens the latest release. Choose the x64 installer.";
  return "Opens the latest release on GitHub.";
}

function Featured({ os, arch }: { os: Os; arch: Arch }) {
  if (os === "linux") {
    return (
      <div className="grid gap-3">
        <p className="text-pretty text-muted-foreground">
          For Linux. Run this in a terminal; it picks the right package for your system.
        </p>
        <code className="block rounded-md border bg-well px-3 py-2.5 font-mono text-[12.5px]/5 break-all text-foreground select-all">
          {LINUX_INSTALL}
        </code>
        <CopyCommand
          value={LINUX_INSTALL}
          what="install command"
          text="Copy install command"
          className="w-full"
          size="lg"
          variant="default"
        />
        <p className="text-xs text-muted-foreground">
          Or download a package from the{" "}
          <ExternalLink href={LINUX_RELEASES}>latest release</ExternalLink>.
        </p>
      </div>
    );
  }
  if (os === "windows") {
    return (
      <div className="grid gap-3">
        <p className="flex items-center gap-2 text-muted-foreground">
          For Windows <StatusBadge tone="warning">Preview</StatusBadge>
        </p>
        <a
          href={WINDOWS_RELEASES}
          rel="noreferrer noopener"
          className={cn(buttonVariants({ size: "lg" }), "w-full")}
        >
          <DownloadIcon aria-hidden="true" />
          Download for Windows
        </a>
        <p className="text-xs text-pretty text-muted-foreground">
          {archHint("windows", arch)} The Windows build is a preview and may have rough
          edges.
        </p>
      </div>
    );
  }
  return (
    <div className="grid gap-3">
      <p className="text-muted-foreground">For Mac.</p>
      <a
        href={MAC_DMG}
        rel="noreferrer noopener"
        className={cn(buttonVariants({ size: "lg" }), "w-full")}
      >
        <DownloadIcon aria-hidden="true" />
        Download for Mac
      </a>
      <p className="text-xs text-pretty text-muted-foreground">{archHint("mac", arch)}</p>
      <SecretWell label="Or with Homebrew" value={MAC_BREW} className="mt-1" />
    </div>
  );
}

function OtherRow({ os }: { os: Os }) {
  return (
    <li className="flex min-h-11 flex-wrap items-center justify-between gap-x-3 gap-y-1 border-b border-border/70 py-1.5 last:border-b-0">
      <span className="flex items-center gap-2 font-medium">
        {OS_NAMES[os]}
        {os === "windows" && <StatusBadge tone="warning">Preview</StatusBadge>}
      </span>
      <span className="ml-auto flex items-center gap-1">
        {os === "mac" && (
          <>
            <CopyCommand
              value={MAC_BREW}
              what="Homebrew command"
              text="Copy brew command"
            />
            <ExternalButton href={MAC_DMG}>Download</ExternalButton>
          </>
        )}
        {os === "linux" && (
          <>
            <CopyCommand
              value={LINUX_INSTALL}
              what="Linux install command"
              text="Copy command"
            />
            <ExternalButton href={LINUX_RELEASES}>Packages</ExternalButton>
          </>
        )}
        {os === "windows" && (
          <ExternalButton href={WINDOWS_RELEASES}>Download</ExternalButton>
        )}
      </span>
    </li>
  );
}

function ExternalLink({ href, children }: { href: string; children: React.ReactNode }) {
  return (
    <a
      href={href}
      rel="noreferrer noopener"
      className="font-medium text-foreground underline decoration-border underline-offset-4 hover:decoration-foreground"
    >
      {children}
    </a>
  );
}

function ExternalButton({ href, children }: { href: string; children: React.ReactNode }) {
  return (
    <a
      href={href}
      rel="noreferrer noopener"
      className={cn(
        buttonVariants({ variant: "ghost", size: "sm" }),
        "text-muted-foreground",
      )}
    >
      {children}
      <ArrowUpRightIcon aria-hidden="true" />
    </a>
  );
}

function CopyCommand({
  value,
  what,
  text,
  className,
  size = "sm",
  variant = "ghost",
}: {
  value: string;
  what: string;
  text: string;
  className?: string;
  size?: "sm" | "lg";
  variant?: "ghost" | "default";
}) {
  const { copy, copied } = useCopy();
  return (
    <Button
      type="button"
      variant={variant}
      size={size}
      className={cn(variant === "ghost" && "text-muted-foreground", className)}
      aria-label={`Copy ${what}`}
      onClick={async () => {
        if (!(await copy(value)))
          toast.error(`Couldn't copy the ${what}. Select it and copy it.`);
      }}
    >
      <CopyIcon aria-hidden="true" />
      <span aria-live="polite">{copied ? "Copied" : text}</span>
    </Button>
  );
}

function LinkStep({ code }: { code: LinkCode }) {
  const [url, setUrl] = useState<string | null>(null);
  useEffect(() => setUrl(code.valid ? linkCodeUrl(code) : null), [code]);

  if (!code.valid) {
    return (
      <p role="alert" className="flex items-start gap-2 text-pretty">
        <CircleAlertIcon
          className="mt-0.5 size-4 shrink-0 text-destructive"
          aria-hidden="true"
        />
        <span>
          This link is incomplete. Ask your admin to send it again, and open it exactly as
          sent.
        </span>
      </p>
    );
  }

  return (
    <div className="grid gap-3">
      <p className="text-pretty text-muted-foreground">
        Once Copper is installed, open it from here. If nothing happens, copy the link
        code and paste it into Copper › Settings › Cloud.
      </p>
      {url ? (
        <SecretWell label="Link code" value={url} emphasis />
      ) : (
        <Skeleton className="h-[86px] w-full" />
      )}
      <a
        href={url ?? undefined}
        aria-disabled={!url}
        className={cn(buttonVariants({ variant: "outline", size: "lg" }), "w-full")}
      >
        Open in Copper
        <ArrowUpRightIcon aria-hidden="true" />
      </a>
      <p className="text-xs text-pretty text-muted-foreground">
        The link code is your personal access to this server. Don&rsquo;t share it.
      </p>
    </div>
  );
}
