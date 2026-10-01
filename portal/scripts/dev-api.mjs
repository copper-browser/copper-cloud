#!/usr/bin/env node
/**
 * `bun run dev:api`: `next dev` proxying /admin/api/* to a local copper-cloud.
 *
 * Next's rewrite proxy verifies TLS no matter what NODE_TLS_REJECT_UNAUTHORIZED
 * says, so for an https target with a self-signed certificate this fetches the
 * server's certificate once and trusts exactly that certificate (via
 * NODE_EXTRA_CA_CERTS) for this dev session, printing its fingerprint so it can
 * be compared with the `fp=` in the link code.
 *
 * Env: COPPER_CLOUD_DEV_API (default https://127.0.0.1:8443).
 */
import { spawn } from "node:child_process";
import { createHash } from "node:crypto";
import { existsSync, mkdtempSync, readFileSync, writeFileSync } from "node:fs";
import { isIP } from "node:net";
import { tmpdir } from "node:os";
import { dirname, join } from "node:path";
import tls from "node:tls";
import { fileURLToPath } from "node:url";

const root = join(dirname(fileURLToPath(import.meta.url)), "..");
const target = new URL(process.env.COPPER_CLOUD_DEV_API ?? "https://127.0.0.1:8443");
const env = { ...process.env, COPPER_CLOUD_DEV_API: target.origin };

function peerCertificate(host, port) {
  return new Promise((resolve, reject) => {
    const socket = tls.connect(
      {
        host,
        port,
        servername: isIP(host) ? undefined : host,
        rejectUnauthorized: false,
      },
      () => {
        const cert = socket.getPeerCertificate();
        socket.end();
        if (!cert?.raw) reject(new Error("the server sent no certificate"));
        else resolve(cert.raw);
      },
    );
    socket.setTimeout(4000, () => socket.destroy(new Error("timed out")));
    socket.on("error", reject);
  });
}

if (target.protocol === "https:") {
  try {
    const raw = await peerCertificate(target.hostname, Number(target.port || 443));
    const b64 = raw
      .toString("base64")
      .match(/.{1,64}/g)
      .join("\n");
    const file = join(mkdtempSync(join(tmpdir(), "copper-cloud-portal-")), "server.pem");
    // Keep any CAs the shell already trusts; add this server's certificate.
    const existing = process.env.NODE_EXTRA_CA_CERTS;
    const prior =
      existing && existsSync(existing)
        ? `${readFileSync(existing, "utf8").trim()}\n`
        : "";
    writeFileSync(
      file,
      `${prior}-----BEGIN CERTIFICATE-----\n${b64}\n-----END CERTIFICATE-----\n`,
    );
    env.NODE_EXTRA_CA_CERTS = file;
    const fp = createHash("sha256").update(raw).digest("hex");
    console.log(
      `Proxying /admin/api to ${target.origin}, trusting its certificate fp=${fp}`,
    );
  } catch (error) {
    console.error(
      `Can't reach copper-cloud at ${target.origin} (${error.message}).\n` +
        "Start it, point COPPER_CLOUD_DEV_API at it, or run `bun run dev:mock` instead.",
    );
    process.exit(1);
  }
}

const child = spawn(
  join(root, "node_modules", ".bin", "next"),
  ["dev", ...process.argv.slice(2)],
  {
    cwd: root,
    env,
    stdio: "inherit",
  },
);
child.on("exit", (code, signal) => process.exit(signal ? 1 : (code ?? 0)));
for (const sig of ["SIGINT", "SIGTERM"]) process.on(sig, () => child.kill(sig));
