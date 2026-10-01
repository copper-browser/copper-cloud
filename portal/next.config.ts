import type { NextConfig } from "next";
import { PHASE_DEVELOPMENT_SERVER } from "next/constants";

/**
 * The portal ships as a static export (`out/`) that the copper-cloud binary
 * embeds and serves from `/`. All data loads client-side from `/admin/api/*`.
 *
 * `next dev` proxies `/admin/api/*` to a local copper-cloud
 * (`COPPER_CLOUD_DEV_API`, default https://127.0.0.1:8443). Mock mode
 * (`NEXT_PUBLIC_MOCK=1`) needs no server at all.
 */
const base: NextConfig = {
  // Always defined so `process.env.NEXT_PUBLIC_MOCK === "1"` folds at build
  // time and the mock fixture is left out of production bundles entirely.
  env: {
    NEXT_PUBLIC_MOCK: process.env.NEXT_PUBLIC_MOCK === "1" ? "1" : "0",
    NEXT_PUBLIC_API_BASE: process.env.NEXT_PUBLIC_API_BASE ?? "",
  },
  trailingSlash: true,
  images: { unoptimized: true },
  poweredByHeader: false,
  devIndicators: false,
  reactStrictMode: true,
};

export default function config(phase: string): NextConfig {
  if (phase !== PHASE_DEVELOPMENT_SERVER) {
    return { ...base, output: "export" };
  }
  if (process.env.NEXT_PUBLIC_MOCK === "1") {
    return base;
  }
  const target = (process.env.COPPER_CLOUD_DEV_API ?? "https://127.0.0.1:8443").replace(
    /\/$/,
    "",
  );
  return {
    ...base,
    // `/admin/api/me` must reach the server as-is, not as `/admin/api/me/`.
    skipTrailingSlashRedirect: true,
    async rewrites() {
      return [
        {
          source: "/admin/api/:path*",
          destination: `${target}/admin/api/:path*`,
        },
      ];
    },
  };
}
