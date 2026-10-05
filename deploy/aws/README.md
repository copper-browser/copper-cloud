# copper-cloud on AWS (`deploy/aws`)

One command brings up a named copper-cloud server: EC2 (Ubuntu 24.04) + RDS PostgreSQL 16 +
Elastic IP + security groups. The server's link code is stored in SSM Parameter Store. Each
deployment is fully separate because every resource name includes `<name>`, so you can run as
many in parallel as you like.

```bash
./deploy/aws/up.sh demo          # create or update; waits for the install, then prints the admin
                                 # portal URL + password command and the first link code
./deploy/aws/link-code.sh demo   # print the link code again
./deploy/aws/down.sh demo        # destroy everything
```

`./deploy/aws/status.sh demo` shows the install status, URL, instance and DB endpoint, and
checks `GET /healthz`. It exits 0 only if the status is `ready` and healthz returns `ok`.
You can run all of these scripts from any directory, and running them again is safe.

## Prerequisites

| Tool | Why | Setup |
|---|---|---|
| AWS CLI v2 | Terraform credentials, SSM reads | `aws sso login` (set `AWS_PROFILE` if you're not using the default profile) |
| Terraform ≥ 1.6 (or OpenTofu) | infrastructure | `brew install hashicorp/tap/terraform`, or set `TERRAFORM=tofu` |
| GitHub CLI | downloads the release tarball | `gh auth login` (needs read access to `copper-browser/copper-cloud`) |
| session-manager-plugin (optional) | shell on the VM | `brew install --cask session-manager-plugin` |

Terraform deploys into whichever AWS account your credentials (`AWS_PROFILE`) resolve to; the default region is `us-east-1`.

## `up.sh` options

```
./up.sh <name> [--release vX.Y.Z | --binary path/to/copper-cloud-<ver>-linux-<arch>.tar.gz]
               [--domain cloud.example.com --email ops@example.com]
               [--instance-type t3.small] [--region us-east-1]
               [--db-instance-class db.t4g.micro] [--ssh-cidr 1.2.3.4/32 [--key-name kp]]
               [--no-signup | --signup] [--no-eip | --eip]
               [--admin-email admin@example.com] [--access-mode directory|open]
```

- `name`: 2–24 characters: lowercase letters, digits and single hyphens.
- With no `--release` or `--binary`, the first run downloads the **latest** GitHub release.
  Later runs keep the tarball recorded in `state/<name>/terraform.tfvars`, so a re-run never
  upgrades without being asked. Pass `--release vX.Y.Z` to upgrade.
- The architecture comes from the instance type: Graviton types (`t4g`, `m7g`, `c7g`, …) get
  `arm64` and the `linux-aarch64` tarball; all other types get `amd64` and `linux-x86_64`.
  Terraform refuses a tarball whose architecture doesn't match.
- Every setting is saved in `state/<name>/terraform.tfvars`. Flags override the saved values
  and the rest stay as they were, so `./up.sh demo --domain …` changes only the domain.
- Once `terraform apply` finishes, `up.sh` checks `/copper-cloud/<name>/status` until it reads
  `ready`. The timeout is 15 minutes; change it with `COPPER_CLOUD_UP_TIMEOUT=<seconds>`. Then
  it prints the admin portal URL, the admin email, the command that prints the admin password,
  and the link code. A `failed: <step>` status stops the script with an error.
- `--admin-email` (default `admin@<domain or public IP>`) and `--access-mode` (default
  `directory`) only matter on the first boot of a fresh database; see [Admin portal](#admin-portal).

## Admin portal

Every deployment gets a web admin portal served by the same binary at the instance URL:

```bash
terraform -chdir=deploy/aws output -state=state/demo/terraform.tfstate -raw admin_url
# → https://203.0.113.9/
eval "$(terraform -chdir=deploy/aws output -state=state/demo/terraform.tfstate -raw admin_password_command)"
# → the initial admin password (SSM SecureString /copper-cloud/demo/admin-password)
```

(`up.sh` prints all three: `admin_url`, `admin_email`, `admin_password_command`.) Without a
domain the certificate is self-signed, so the browser warns once — compare the fingerprint with
the `fp=` in the link code if you want to be sure.

- Terraform generates the password (`random_password.admin`) and stores it in SSM; cloud-init
  reads it like the other secrets and `install.sh` creates the admin **only if it does not
  exist**. Changing the password in the portal (Settings) does not update SSM, and replacing
  the VM never resets it. Lost it? `sudo copper-cloud admin reset-admin-password --email …` in
  an SSM session.
- A fresh deployment starts in **directory** mode: only personal access keys minted in the
  portal (Access keys › New key) pass the instance gate, and each comes with its own link
  code. The link code `up.sh` prints is one such key (label "Installer link code", one account)
  for your first Copper. Switch to **open** mode (the shared instance link code works for
  everyone) in the portal › Settings, or deploy with `--access-mode open`.

## Layout and state

```
deploy/aws/
  *.tf, user_data.sh.tftpl          one Terraform configuration shared by every deployment
  .terraform/, .terraform.lock.hcl  providers, initialized once (init is serialized by a lock)
  bin/<tag>/copper-cloud-*.tar.gz   downloaded release tarballs (gitignored)
  state/<name>/terraform.tfvars     inputs for this deployment (gitignored)
  state/<name>/terraform.tfstate    local state for this deployment (gitignored, 0600, CONTAINS SECRETS)
```

The scripts run `terraform -chdir=deploy/aws apply -state=state/<name>/terraform.tfstate
-var-file=state/<name>/terraform.tfvars`. There is no remote backend. Each deployment has its
own state file, and the local backend locks that file. **Back up `state/` if a deployment
matters.** If you lose the state, you have to delete the deployment's resources by hand. Every
resource is tagged `Project=copper-cloud` and `Deployment=<name>`.

To run Terraform yourself:

```bash
cd deploy/aws
terraform init
terraform plan  -state=state/demo/terraform.tfstate -var-file=state/demo/terraform.tfvars
terraform output -state=state/demo/terraform.tfstate
```

## N instances in parallel

```bash
for n in alpha beta gamma; do ./deploy/aws/up.sh "$n" --release v0.4.0 > "/tmp/up-$n.log" 2>&1 & done; wait
```

Each name gets its own RDS instance, bucket, IAM role, SGs, EIP and SSM path. Check the account
quotas: 5 EIPs per region by default, and 40 RDS instances.

## What gets created (per name)

- **Network**: the default VPC and its default subnets, looked up rather than created. The
  instance goes in the first AZ that offers the chosen instance type, which skips
  `us-east-1e`.
  - SG `copper-cloud-<name>-app`: 443/tcp from `0.0.0.0/0` and `::/0`, and 22/tcp only when
    `--ssh-cidr` is set.
  - SG `copper-cloud-<name>-db`: 5432/tcp from the app SG only.
- **RDS** `copper-cloud-<name>`:
  - PostgreSQL 16, latest minor version at creation.
  - `db.t4g.micro`, 20 GB gp3, encrypted, not public, 1-day backups.
  - `skip_final_snapshot`, no deletion protection.
  - Database and user are `copper_cloud`, with a random 32-character alphanumeric password.
  - The app connects with `sslmode=require`.
- **EC2** `copper-cloud-<name>`:
  - Ubuntu 24.04 AMI from Canonical (`099720109477`).
  - IMDSv2 required, 20 GB encrypted gp3 root volume.
  - Instance role: SSM core, `s3:GetObject` on its bucket, and `ssm:GetParameter`/`PutParameter`
    on `/copper-cloud/<name>/*`.
  - An Elastic IP, allocated before the instance and then associated with it.
- **S3** `copper-cloud-<name>-<account>`: private, versioning off, `force_destroy`. It holds the
  release tarball.
- **SSM** `/copper-cloud/<name>/…`:

  | Parameter | Type | Value |
  |---|---|---|
  | `database-url` | SecureString | the RDS connection URL |
  | `instance-key` | SecureString | 32 random bytes, base64url |
  | `master-key` | SecureString | 32 random bytes, base64url |
  | `admin-password` | SecureString | initial admin portal password (24 alphanumerics) |
  | `link-code` | SecureString | written by the VM |
  | `status` | String | `pending` → `installing` → `ready` \| `failed: <step>` |

  Terraform owns all six parameters, so `down.sh` removes them too.

### Boot sequence (`user_data.sh.tftpl`)

1. Wait for the EIP to be associated.
2. Install packages (waiting on the apt lock) and the AWS CLI (snap, or the official v2 bundle
   as a fallback).
3. Copy the tarball from S3, verify its sha256 and extract it to `/tmp/copper-cloud-pkg`.
4. Read the secrets from SSM.
5. Run `install.sh` with this environment:
   - `DATABASE_URL`
   - `COPPER_CLOUD_BINARY`
   - `COPPER_CLOUD_INSTANCE_KEY`
   - `COPPER_CLOUD_MASTER_KEY`
   - `COPPER_CLOUD_ADMIN_PASSWORD` (and `COPPER_CLOUD_ADMIN_EMAIL` when `admin_email` is set)
   - `COPPER_CLOUD_ACCESS_MODE`: `directory` or `open` (fresh database only)
   - `COPPER_CLOUD_PUBLIC_HOST`: the domain, or the EIP if there's no domain
   - `COPPER_CLOUD_ALLOW_SIGNUP`
   - `COPPER_CLOUD_DOMAIN` and `COPPER_CLOUD_ACME_EMAIL`, only when a domain is set
6. Put `/etc/copper-cloud/link-code` into SSM and set `status=ready`.

Any failure sets `status=failed: <step>`.

Secrets never appear in EC2 user data or the console output. The install log is root-only.

**Why the keys live in Terraform:** these changes replace the VM:

- a new `--release`
- adding or changing `--domain`
- changing `--key-name`

The replacement VM gets the same instance key and master key, so the data in RDS stays readable
and existing accounts keep working. One thing still changes: without a domain, each new VM
generates a **new self-signed certificate**. That changes the link code's `fp=`, so linked
Coppers have to paste the new link code (`./link-code.sh <name>`).

## Costs (us-east-1, on-demand, approximate)

| Item | $/month |
|---|---|
| EC2 t3.small | ~15 |
| RDS db.t4g.micro, single-AZ | ~12 |
| RDS 20 GB gp3 + EBS 20 GB gp3 | ~4 |
| Public IPv4 (EIP) | ~3.6 |
| S3 / SSM / data transfer | ≈0 |
| **Total** | **~$35/month (~$1.15/day) per deployment** |

`t4g.small` cuts about $3/month and needs the `aarch64` tarball. Run `down.sh` when you're done.
The only charge left after teardown is any local state you keep.

## Adding a domain (ACME TLS)

Route 53 is **not** managed here.

1. Bring the deployment up without a domain: `./up.sh demo`. The output includes the EIP
   (`public_ip`).
2. Create a DNS `A` record, e.g. `cloud.example.com → <EIP>`, and wait until
   `dig +short cloud.example.com` returns the EIP.
3. Run `./up.sh demo --domain cloud.example.com --email you@example.com`. This replaces the VM,
   keeps the same EIP, RDS and keys, and the server gets a Let's Encrypt certificate. The new
   link code has no `fp=`, and the URL becomes `https://cloud.example.com`.

To go back to self-signed: `./up.sh demo --domain "" --email ""`.

## Teardown

```bash
./deploy/aws/down.sh demo            # terraform destroy; keeps state/demo/ so `up.sh demo` re-creates it with the same settings
./deploy/aws/down.sh demo --purge    # …and deletes state/demo/
```

This destroys the RDS instance **without a final snapshot**, so its data is gone. The S3 bucket
is force-deleted. If you destroy and re-create under the same name, the deployment gets new
keys, a new DB and a new link code.

## Troubleshooting

- **Get a shell (no SSH needed)**: `aws ssm start-session --region us-east-1 --target <instance_id>`.
  `./status.sh <name>` prints the exact command.
- **Bootstrap log**: `sudo less /var/log/copper-cloud-install.log`. It has the full `set -x`
  trace, except for the sections that handle secrets. `/var/log/cloud-init.log` covers
  cloud-init itself.
- **Service**:
  - `systemctl status copper-cloud`
  - `journalctl -u copper-cloud -e`
  - `sudo copper-cloud doctor`
  - `sudo copper-cloud link-code`
- **Status stuck at `pending`**: cloud-init hasn't reached the IAM step. Look for errors in
  `cloud-init status --long` and in the log. The usual causes are the instance profile not yet
  propagated (the script retries for 5 minutes) or no outbound network.
- **`failed: install.sh`**: the installer failed, so the problem is in copper-cloud, not
  Terraform. Read the end of the install log. After fixing it, either re-run inside the
  session (`cd /tmp/copper-cloud-pkg && sudo -E bash install.sh` with the env from the log) or
  replace the VM:
  `terraform -chdir=deploy/aws apply -replace=aws_instance.this -state=… -var-file=…`.
  Re-running `up.sh` with an unchanged tarball does **not** re-run cloud-init.
- **healthz fails from your laptop**:
  - Check that the SG allows 443 and that the URL uses the EIP.
  - With ACME, the certificate is only issued once DNS points at the EIP. Use `curl -k` until
    then.
- **RDS from the VM**: `psql "$(aws ssm get-parameter --name /copper-cloud/<name>/database-url --with-decryption --query Parameter.Value --output text)"`
  (install `postgresql-client` first).
- **Stale init lock** (`.terraform-init.lock` left behind after a killed run): `rmdir deploy/aws/.terraform-init.lock`.

### Elastic IP quota

Each deployment allocates one Elastic IP by default. If the account is at its EIP quota
(`aws service-quotas get-service-quota --service-code ec2 --quota-code L-0263D0A3`), pass
`--no-eip`: the VM then uses its auto-assigned public IPv4 (fine for create/destroy cycles; the
address changes only if the instance is stopped and started, which Terraform never does).
