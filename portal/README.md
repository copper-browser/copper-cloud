# Copper Cloud admin portal

The web console for a copper-cloud instance: access keys, people, devices, canvases and
instance settings. It is a Next.js 15 static export (`out/`) that the `copper-cloud` binary
embeds with `rust-embed` and serves from `/`. Everything loads client-side from
`/admin/api/*`; the contract is [`../docs/admin-api.md`](../docs/admin-api.md).

## Scripts

| Script              | What it does                                                                                                                                                                                                                            |
| ------------------- | --------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `bun run dev:mock`  | `next dev` with `NEXT_PUBLIC_MOCK=1`: every page runs against the in-memory fixture in `src/lib/mock.ts`, no server needed. Any password signs in except `wrong`.                                                                       |
| `bun run dev:api`   | `next dev` proxying `/admin/api/*` to a local copper-cloud (`COPPER_CLOUD_DEV_API`, default `https://127.0.0.1:8443`). For a self-signed server it trusts exactly that server's certificate for the session and prints its fingerprint. |
| `bun run dev`       | Plain `next dev` with the same proxy, for servers with a trusted certificate or `tls.mode = "off"` (`COPPER_CLOUD_DEV_API=http://127.0.0.1:8080`).                                                                                      |
| `bun run build`     | Static export to `out/` (what CI and release embed).                                                                                                                                                                                    |
| `bun run lint`      | ESLint (next, typescript-eslint, better-tailwindcss), zero warnings allowed.                                                                                                                                                            |
| `bun run typecheck` | `tsc --noEmit`.                                                                                                                                                                                                                         |
| `bun run format`    | Prettier with `prettier-plugin-tailwindcss`.                                                                                                                                                                                            |
| `bun run check`     | typecheck + lint + format check.                                                                                                                                                                                                        |

`NEXT_PUBLIC_API_BASE` prefixes every API URL (default: same origin). The mock fixture is
compiled out of normal builds.

## How it talks to the server

`src/lib/api.ts` is the only network code. Every request is same-origin with
`credentials: 'same-origin'` (the `cc_admin` cookie, `Path=/admin`) and carries
`X-Requested-With: copper-cloud-portal` (the CSRF header). A `401 admin_session` from any
call sends the browser to `/login/?next=…`; a `401 credentials` (wrong password on `login`
or `password`) is returned to the form instead.

The server's CSP allows only `'self'` plus hashes of the export's own inline bootstrap
scripts, so the portal never injects scripts at runtime and loads nothing from other origins
(system fonts, bundled icons).

## Layout

```
src/app/login/            sign-in (no shell)
src/app/(console)/        rail + session; one folder per page
  page.tsx, overview-view.tsx
  keys/ people/ devices/ canvases/ settings/   page.tsx (metadata) + view.tsx (client)
src/components/           shell, data table, dialogs, credential well, status badge
src/components/ui/        shadcn/ui (Base UI) primitives, lightly retuned
src/hooks/                data loading (useResource/useList), clock, copy, `/` shortcut
src/lib/                  api client, wire types, mock fixture, formatting, audit phrasing
```

Design notes: neutral warm-gray palette with a single copper accent (primary action, focus,
checked controls, active nav icon); 13 px UI type on the system font stack; light and dark
follow `prefers-color-scheme`. Tables are fixed-layout and drop low-priority columns via
container queries, so nothing scrolls sideways; the rail collapses to icons below 900 px.
