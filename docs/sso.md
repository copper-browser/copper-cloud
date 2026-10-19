# Single sign-on and agent integrations

Since 0.9.0 people can join a Copper Cloud with their **company account** — Google Workspace or
any OpenID Connect provider (Okta, Microsoft Entra ID, Keycloak, …). The admin sets it up once;
after that, onboarding is one link:

1. The admin shares `https://HOST/start`.
2. A new person installs Copper, clicks **Open in Copper**, then **Continue with Google**.
3. Copper opens the system browser, they pick their company account, and they are in. There's
   no account to create, no password and no key to paste.

Anyone whose provider says they own a verified address in an **allowed domain** gets in. The
first sign-in creates their account unless you turn that off. Existing password accounts are
linked by email, so their owners can use either method.

## How it works

Copper uses the standard native-app flow (RFC 8252, authorization code + PKCE S256):

1. Copper reads `GET /v1/auth/sso` (client id, authorization endpoint, allowed domains).
2. It opens the provider's sign-in page in the system browser, with a loopback redirect
   `http://127.0.0.1:<port>/`.
3. The provider sends the browser back to Copper's loopback listener with a one-time code.
4. Copper hands the code and its PKCE verifier to `POST /v1/auth/sso/{provider}`.

The **client secret never leaves the server.** The server exchanges the code at the provider's
token endpoint, then validates the ID token from that response:

- `iss`, `aud` (= the client id; `azp` when there are several audiences) and `exp` (60 s skew);
- `email_verified`;
- the email domain must be in the provider's allowed domains;
- Google only: the `hd` (Workspace) claim must also be an allowed domain.

Because the token comes straight from the provider over TLS, the server does not check its
signature (OpenID Connect Core, "ID Token Validation").

The account is then found or created:

1. by the linked identity (provider + subject);
2. otherwise by email, which links the identity to the existing account;
3. otherwise a new account is created (no password; display name from the token).

The device is signed in exactly as with a pairing code: a session plus the gate key (a fresh
personal access key in `directory` mode, the instance key in `open` mode).

The join link `copper-cloud://HOST:PORT/#sso=google&fp=…` carries **no secret**: it only tells
Copper where the server is and which provider to use. The provider's domain check is the gate.

## Set up Google

You need to be a Google Workspace admin (or have the right to create OAuth clients in a Cloud
project that belongs to your Workspace organization).

### 1. Create the OAuth client (Google Cloud console)

1. Open <https://console.cloud.google.com/> and pick (or create) a project in your
   organization.
2. Go to **Google Auth Platform** (formerly *APIs & Services › OAuth consent screen*) and click
   **Get started** if it isn't configured yet:
   - **App name**: e.g. "Copper Cloud"; **User support email**: yours.
   - **Audience: Internal.** Only accounts in your Workspace organization can then sign in.
     Google enforces this before your server is even involved.
   - Contact email, accept the policy, **Create**.
3. **Google Auth Platform › Clients › Create client**:
   - **Application type: Desktop app.**
   - **Name**: "Copper Cloud".
   - **Create.**
4. Copy the **Client ID** (`…apps.googleusercontent.com`) and the **Client secret**
   (`GOCSPX-…`). You can download the JSON too, but only these two values are needed.

No redirect URI needs registering, and no scopes beyond the defaults (`openid email profile`)
are needed.

**Why "Desktop app".** Copper is a native app that receives the sign-in on a loopback address
(`http://127.0.0.1:<random port>/`). Google allows any loopback port only for *Desktop app*
clients; a *Web application* client would need every redirect URI registered in advance. With
a Desktop client, the PKCE verifier proves the code belongs to the Copper that started the
sign-in. The secret, which Google does not treat as confidential for desktop clients, still
stays on your server rather than in every Copper.

### 2. Configure Copper Cloud

In the **admin portal › Single sign-on**, choose Google, paste the client ID and secret, and
enter your email domain(s). The portal's **Test** button checks the credentials against
Google's token endpoint.

Or use the CLI on the server (the secret is read from stdin, never from argv):

```sh
printf '%s\n' 'GOCSPX-…' | sudo -u copper-cloud copper-cloud admin sso set-google \
  --client-id 123-abc.apps.googleusercontent.com --client-secret-stdin \
  --domain example.com            # repeat --domain for more; --no-auto-create = existing accounts only
copper-cloud admin sso show       # providers (never the secret), join link, /start URL
```

Then share the `/start` URL that `show` prints (or `copper-cloud link-code --sso` for the bare
join link).

## Other OpenID Connect providers (Okta, Entra ID, Keycloak, …)

Register a **native / public** application with the loopback redirect
`http://127.0.0.1/` (any port), the authorization code grant and PKCE. Then:

```sh
printf '%s\n' "$SECRET" | copper-cloud admin sso set-oidc --id okta --name Okta \
  --issuer https://example.okta.com --client-id 0oa… --client-secret-stdin --domain example.com
```

- The server reads `<issuer>/.well-known/openid-configuration`, caching it for an hour. The
  document's `issuer` must match exactly and its endpoints must be `https://`.
- The secret is optional: leave out `--client-secret-stdin` for a public client.
- **Okta**: create an *OIDC – Native Application*, sign-in redirect URI `http://127.0.0.1/`
  (Okta matches loopback ports loosely), grant type *Authorization Code*, *Require PKCE*.
  Issuer: your org's URL or `…/oauth2/default`.
- **Microsoft Entra ID**: register an app, add a *Mobile and desktop applications* platform with
  redirect URI `http://localhost`. Issuer: `https://login.microsoftonline.com/<tenant id>/v2.0`.
  **Known limitation:** Entra ID tokens carry no `email_verified` claim, and the server refuses
  any sign-in without `email_verified: true`. So Entra ID sign-ins fail with `sso_failed` in
  this release. Use Google, Okta, Keycloak or another provider that sends `email_verified`, or
  put Entra behind one of them.

The id is used in the join link (`#sso=okta`), so pick a short slug (`a-z 0-9 - _`).

## Password sign-in

Password sign-up/login keeps working alongside SSO. To make SSO the only way in for people,
turn it off in the portal or with:

```sh
copper-cloud admin sso password-signin off
```

When it is off:

- signup and login answer `403 password_signin_disabled`;
- existing password users sign in with SSO (matched by email);
- admins and the portal are not affected.

An SSO-only account has no password. Password login for it answers `401 sso_only`, telling the
person to use single sign-on. An admin can still give it a password from the portal or with
`admin reset-password`.

## Onboarding page: `/start`

`https://HOST/start` is a server-rendered page with no external assets and no key in it. It
shows:

1. how to install Copper (Mac zip or Homebrew cask, the Linux install one-liner);
2. **Open in Copper**: the deep link `copper://cloud/join?code=<join link>`, plus the join
   link as copyable text;
3. "Continue with Google" (the provider's name).

The heading uses the instance name (`admin set-instance-name`, or the portal's settings).
Before SSO is set up, the page politely says so.

## Agent integrations

An agent integration lets a **trusted service** sign headless Coppers in to people's accounts
without their passwords. A bot platform, for example, can give each bot its own browser signed
in to its owner's Copper Cloud.

```sh
copper-cloud admin integration create --name "Bots" [--domain example.com] [--no-create-users]
#   → prints the key (cai_…) once on stdout; store it in the service's secret store
copper-cloud admin integration list
copper-cloud admin integration rotate <id>   # new key, old one stops working immediately
copper-cloud admin integration revoke <id>   # key dead + every agent device signed out
```

The service calls `POST /v1/agents/pair` with `Authorization: Bearer cai_…` and
`{email, device: {id, name}}` (see [api.md](api.md#post-v1agentspair--no-gate-header)). It gets
back a single-use, 10-minute pairing code **bound to that device**, which the headless Copper
redeems like any pairing code. Re-pairing the same device id signs out its previous session and
revokes its previous gate key. Every pairing is audited (`agent.pair`).

**What the key can do.** An integration key can mint a session for **any user whose email is in
its allowed domains**. With `create_users` (the default), it can also create such accounts. That
is effectively an admin-level credential for those domains, so:

- give it only to services you trust as much as an admin;
- restrict its domains (`--domain`). Without its own domains, it uses the SSO providers'
  domains; with neither, it can pair nobody;
- use `--no-create-users` when the service should only reach existing accounts;
- rotate it when people with access to it leave, and revoke it when the service is retired.

Only a SHA-256 of the key is stored. A lost key cannot be recovered, only rotated.

## Troubleshooting

| Symptom | Cause / fix |
|---|---|
| Copper shows no "Continue with …" button | No enabled provider (`admin sso show`), or the server is older than 0.9.0. A Google provider without a secret is not offered. |
| `403 sso_domain` "use your @example.com account" | The account's email (or, for Google, its Workspace domain `hd`) is not an allowed domain. A personal Gmail with a company address is refused on purpose. |
| `401 sso_failed` "…invalid_client…" | Wrong client ID/secret: run the portal's **Test** button and paste the secret again. |
| `401 sso_failed` "…invalid_grant…" | The code expired or was used already (or the redirect/PKCE did not match). Sign in again. |
| `401 sso_failed` "…not verified…" | The provider did not send `email_verified: true` (Entra ID never does; see above). |
| `401 sso_failed` "expired (check the server clock)" | The server clock is off by more than a minute: check NTP. |
| `403 sso_no_account` | `auto_create` is off and there is no account with that email: create one, or turn auto-create on. |
| `400 bad_redirect` | The client did not use a loopback `http://127.0.0.1:<port>/` redirect (old or modified client). |
| Google says "Access blocked: … can only be used within its organization" | The consent screen is *Internal* and the person picked an account outside your Workspace. That is working as intended. |
| OIDC provider missing from `GET /v1/auth/sso` | Discovery failed (logged as `sso provider unavailable`). Check the issuer URL from the server: `curl <issuer>/.well-known/openid-configuration`. |
| `copper-cloud doctor` says "nobody can sign in" | Password sign-in is off and no provider is enabled. |
