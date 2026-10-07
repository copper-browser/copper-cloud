# Onboarding a team

How to bring people onto your own Copper Cloud instance. The Copper side (install, moving in
from another browser, Settings › Cloud, the agent) is covered by
[Copper's onboarding guide](https://github.com/copper-browser/Copper/blob/fork/docs/onboarding.md);
this page fills in what it leaves to "your administrator".

> Link codes, access keys, instance keys and passwords are secrets. Keep them out of docs,
> tickets and group chats; send each person their link code directly.

## Before you start

| | |
|---|---|
| Instance | Any host you run: the VM one-liner ([install.md](install.md)) or [deploy/aws](../deploy/aws/README.md). Examples below use `cloud.example.com`. |
| Certificate | Self-signed by default — Copper pins it through the `fp=` in each link code; a web browser opening the admin portal warns once. With `COPPER_CLOUD_DOMAIN=…` you get a Let's Encrypt certificate instead. |
| Access mode | `directory` (recommended): everyone gets a personal access key. `open`: one shared link code. |
| Model keys | Optional. Set a Jev key and/or an LLM router key (with **your** gateway's URL) once and every signed-in Copper receives them ([operations.md](operations.md#intelligence-ai-keys)). |

## For a new user

1. **Install Copper** and move in from your previous browser (see Copper's guide).
2. **Get a link code** from your administrator for the email you want on the account.
3. **Settings › Cloud**: paste the code and press **Connect** (the host shown should be your
   instance), then **Create account** with the **same email** the key was issued for, a
   password of at least 10 characters and your name (shown on shared canvases). Pick what
   syncs and press **Turn on sync**.
4. **Second Mac**: on the signed-in Mac, Settings › Cloud › **Pair another Mac** › **Make a
   code**; paste it on the new Mac and press **Pair this Mac**. Don't reuse a one-use link code.

## For the administrator

### Issue a link code

1. Open the admin portal at `https://cloud.example.com/` and sign in. The installer prints the
   initial admin password once (also in `/etc/copper-cloud/admin-credentials`; on AWS see the
   `admin_password_command` output).
2. **Access keys › New key** — *Label*: the person's name; *Email*: their address (sign-up must
   then use it); *Max uses*: `1`; *Expires*: optional.
3. The key and its link code are **shown once** — send the link code to that person directly.

To create the account for them instead: `sudo copper-cloud admin create-user --email …
--password-stdin` on the host, then send them a fresh key so their Copper can pass the gate,
and have them **Sign in**.

### People and offboarding

- **People** in the portal: reset a password, disable (can't sign in, data kept) or delete
  (removes all their synced data and canvases).
- **Access keys** › revoke cuts every Copper using that key off on its next request. Pairing
  mints a separate key per paired Mac (`<device> via pairing`), revocable on its own.
- Lost portal password: `sudo copper-cloud admin reset-admin-password --email …` on the host.

### Don't break everyone's link

With a self-signed certificate, the `fp=` in every link code is that certificate's
fingerprint. Anything that replaces the host or its `tls/` directory (`tls-init --force`, a new
VM without a domain) changes it, and every linked Copper stops connecting until it gets a new
link code. Upgrade in place instead (back up, then re-run the release's `install.sh`) and check
that `copper-cloud link-code` is unchanged and `copper-cloud doctor` is clean. Details:
[operations.md](operations.md), [deploy/aws/README.md](../deploy/aws/README.md).

## Troubleshooting

| Symptom | What to do |
|---|---|
| "The server's certificate doesn't match the link code's fingerprint" | The instance's certificate changed. Ask your administrator for a new link code. |
| "The instance refused this key" | The key was revoked or expired, or a one-use code was reused. Pair from a signed-in Mac, or ask for a new code. |
| Sign-up refused for your email | The key is bound to another address. Use the email the key was issued for, or ask for a new key. |
| The agent says no model is set up | Check Settings › Cloud shows you signed in, then Settings › Intelligence › **Test**. The instance may not share model keys. |
