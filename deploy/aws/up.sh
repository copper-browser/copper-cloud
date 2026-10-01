#!/usr/bin/env bash
# Bring up (or update) a named copper-cloud deployment on AWS.
#
#   ./up.sh <name> [--release vX.Y.Z | --binary path/to/copper-cloud-*-linux-<arch>.tar.gz]
#                  [--domain cloud.example.com --email ops@example.com]
#                  [--instance-type t3.small] [--region us-east-1]
#                  [--db-instance-class db.t4g.micro] [--ssh-cidr 1.2.3.4/32 [--key-name kp]]
#                  [--no-signup | --signup] [--no-eip | --eip]
#                  [--admin-email admin@example.com] [--access-mode directory|open]
#
# Idempotent: re-running reuses the settings saved in state/<name>/terraform.tfvars
# unless overridden by flags. State lives in state/<name>/terraform.tfstate.
set -euo pipefail
# shellcheck source=lib.sh
source "$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/lib.sh"

usage() {
  sed -n '2,14p' "${BASH_SOURCE[0]}" | sed 's/^# \{0,1\}//'
  exit "${1:-0}"
}

[ $# -ge 1 ] || usage 1
case "$1" in -h | --help) usage 0 ;; esac
NAME=$1
shift
validate_name "$NAME"
state_paths "$NAME"

# Defaults: previous run's settings, then built-in defaults.
RELEASE=""
BINARY="$(tfvar_get "$TFVARS" binary_path)"
DOMAIN="$(tfvar_get "$TFVARS" domain)"
EMAIL="$(tfvar_get "$TFVARS" acme_email)"
INSTANCE_TYPE="$(tfvar_get "$TFVARS" instance_type)"
REGION="$(tfvar_get "$TFVARS" region)"
DB_CLASS="$(tfvar_get "$TFVARS" db_instance_class)"
SSH_CIDR="$(tfvar_get "$TFVARS" allow_ssh_cidr)"
KEY_NAME="$(tfvar_get "$TFVARS" ssh_key_name)"
SIGNUP="$(tfvar_get "$TFVARS" allow_signup)"
USE_EIP="$(tfvar_get "$TFVARS" use_eip)"
ADMIN_EMAIL="$(tfvar_get "$TFVARS" admin_email)"
ACCESS_MODE="$(tfvar_get "$TFVARS" access_mode)"
BINARY_FLAG=""

while [ $# -gt 0 ]; do
  case "$1" in
    --release) RELEASE=${2:?--release needs a tag}; BINARY=""; shift 2 ;;
    --binary) BINARY_FLAG=${2:?--binary needs a path}; shift 2 ;;
    --domain) DOMAIN=${2?--domain needs a value (use "" to remove)}; shift 2 ;;
    --email) EMAIL=${2?--email needs a value}; shift 2 ;;
    --instance-type) INSTANCE_TYPE=${2:?}; shift 2 ;;
    --region) REGION=${2:?}; shift 2 ;;
    --db-instance-class) DB_CLASS=${2:?}; shift 2 ;;
    --ssh-cidr) SSH_CIDR=${2?}; shift 2 ;;
    --key-name) KEY_NAME=${2?}; shift 2 ;;
    --no-signup) SIGNUP=false; shift ;;
    --signup) SIGNUP=true; shift ;;
    --no-eip) USE_EIP=false; shift ;;
    --eip) USE_EIP=true; shift ;;
    --admin-email) ADMIN_EMAIL=${2?--admin-email needs a value}; shift 2 ;;
    --access-mode) ACCESS_MODE=${2:?--access-mode needs directory or open}; shift 2 ;;
    -h | --help) usage 0 ;;
    *) die "unknown argument: $1 (see --help)" ;;
  esac
done

[ -n "$RELEASE" ] && [ -n "$BINARY_FLAG" ] && die "use either --release or --binary, not both"
INSTANCE_TYPE=${INSTANCE_TYPE:-$DEFAULT_INSTANCE_TYPE}
REGION=${REGION:-${AWS_REGION:-${AWS_DEFAULT_REGION:-$DEFAULT_REGION}}}
DB_CLASS=${DB_CLASS:-db.t4g.micro}
SIGNUP=${SIGNUP:-true}
ACCESS_MODE=${ACCESS_MODE:-directory}
case "$ACCESS_MODE" in directory | open) ;; *) die "--access-mode must be directory or open" ;; esac
if [ -n "$DOMAIN" ] && [ -z "$EMAIL" ]; then
  die "--domain needs --email (ACME contact)"
fi

check_aws
TF=$(tf_bin)

# --- Architecture from the instance type -----------------------------------
ARCHS=$(aws ec2 describe-instance-types --region "$REGION" --instance-types "$INSTANCE_TYPE" \
  --query 'InstanceTypes[0].ProcessorInfo.SupportedArchitectures' --output text 2>/dev/null || true)
if [ -z "$ARCHS" ]; then
  warn "could not describe $INSTANCE_TYPE; guessing architecture from the family name"
  if [[ ${INSTANCE_TYPE%%.*} =~ ^(a1|[a-z]+[0-9]+g[a-z]*)$ ]]; then ARCHS=arm64; else ARCHS=x86_64; fi
fi
if [[ $ARCHS == *arm64* ]]; then ARCH=aarch64; else ARCH=x86_64; fi
log "$NAME: $INSTANCE_TYPE → linux-$ARCH, region $REGION"

# --- Release tarball ---------------------------------------------------------
if [ -n "$BINARY_FLAG" ]; then
  [ -f "$BINARY_FLAG" ] || die "--binary: no such file: $BINARY_FLAG"
  BINARY="$(cd "$(dirname "$BINARY_FLAG")" && pwd)/$(basename "$BINARY_FLAG")"
elif [ -n "$BINARY" ] && [ -z "$RELEASE" ] && [ -f "$BINARY" ] && [[ $BINARY == *"linux-$ARCH"* ]]; then
  log "reusing $BINARY"
else
  need gh "Install the GitHub CLI and run gh auth login."
  if [ -z "$RELEASE" ]; then
    RELEASE=$(gh release view -R "$REPO_SLUG" --json tagName --jq .tagName) ||
      die "could not resolve the latest release of $REPO_SLUG (gh auth login?)"
  fi
  DL="$DEPLOY_DIR/bin/$RELEASE"
  mkdir -p "$DL"
  log "downloading $REPO_SLUG $RELEASE (linux-$ARCH)"
  gh release download "$RELEASE" -R "$REPO_SLUG" -p "copper-cloud-*-linux-$ARCH.tar.gz" -D "$DL" --skip-existing
  gh release download "$RELEASE" -R "$REPO_SLUG" -p "copper-cloud-*-linux-$ARCH.tar.gz.sha256" -D "$DL" --skip-existing 2>/dev/null || true
  BINARY=$(find "$DL" -maxdepth 1 -type f -name "copper-cloud-*-linux-$ARCH.tar.gz" | sort | tail -n1)
  [ -n "$BINARY" ] || die "no copper-cloud-*-linux-$ARCH.tar.gz asset in $RELEASE"
  if [ -f "$BINARY.sha256" ]; then
    expected=$(awk '{print $1}' "$BINARY.sha256")
    actual=$(shasum -a 256 "$BINARY" | awk '{print $1}')
    [ "$expected" = "$actual" ] || die "sha256 mismatch for $BINARY"
    log "sha256 verified"
  fi
fi
[[ $(basename "$BINARY") == *"linux-$ARCH"* ]] || warn "$(basename "$BINARY") does not look like a linux-$ARCH build"

# --- tfvars + apply ----------------------------------------------------------
mkdir -p "$STATE_DIR"
chmod 700 "$STATE_DIR"
tfstr() { printf '"%s"' "$(printf '%s' "$1" | sed 's/\\/\\\\/g; s/"/\\"/g')"; }
{
  echo "# Written by up.sh — edit via up.sh flags or by hand, then re-run ./up.sh $NAME"
  echo "name              = $(tfstr "$NAME")"
  echo "region            = $(tfstr "$REGION")"
  echo "instance_type     = $(tfstr "$INSTANCE_TYPE")"
  echo "db_instance_class = $(tfstr "$DB_CLASS")"
  echo "binary_path       = $(tfstr "$BINARY")"
  echo "domain            = $(tfstr "$DOMAIN")"
  echo "acme_email        = $(tfstr "$EMAIL")"
  echo "allow_ssh_cidr    = $(tfstr "$SSH_CIDR")"
  echo "ssh_key_name      = $(tfstr "$KEY_NAME")"
  echo "allow_signup      = $SIGNUP"
  echo "use_eip           = ${USE_EIP:-true}"
  echo "admin_email       = $(tfstr "$ADMIN_EMAIL")"
  echo "access_mode       = $(tfstr "$ACCESS_MODE")"
} >"$TFVARS.tmp"
mv "$TFVARS.tmp" "$TFVARS"

tf_init
log "terraform apply ($STATE_FILE)"
"$TF" -chdir="$DEPLOY_DIR" apply -input=false -auto-approve \
  -state="$STATE_FILE" -var-file="$TFVARS"
chmod 600 "$STATE_FILE" "$STATE_FILE.backup" 2>/dev/null || true

URL=$(tf_output url)
log "instance $(tf_output instance_id) at $(tf_output public_ip) — $URL"

# --- Wait for cloud-init -------------------------------------------------------
PREFIX="/copper-cloud/$NAME"
deadline=$(($(date +%s) + ${COPPER_CLOUD_UP_TIMEOUT:-900}))
last=""
while :; do
  status=$(ssm_param "$REGION" "$PREFIX/status")
  if [ "$status" != "$last" ]; then
    log "status: ${status:-<unset>}"
    last=$status
  fi
  case "$status" in
    ready) break ;;
    failed*)
      die "install failed ($status). Inspect: $(tf_output ssm_session_command)  then: sudo less /var/log/copper-cloud-install.log"
      ;;
  esac
  if [ "$(date +%s)" -ge "$deadline" ]; then
    die "timed out waiting for status=ready (last: ${status:-<unset>}). Check ./status.sh $NAME"
  fi
  sleep 10
done

if curl -fsSk --max-time 10 "$URL/healthz" >/dev/null 2>&1; then
  log "healthz OK"
else
  warn "$URL/healthz not reachable from here yet (DNS/ACME may still be settling)"
fi

echo
echo "copper-cloud '$NAME' is ready: $URL"
echo
echo "Admin portal: $(tf_output admin_url)"
echo "  email:    $(tf_output admin_email)"
echo "  password: $(tf_output admin_password_command)"
echo "  (initial password; changing it in the portal does not update SSM)"
echo
echo "Link code (paste into Copper › Settings › Cloud; in directory mode it creates one"
echo "account — mint a personal access key per person in the portal):"
ssm_param "$REGION" "$PREFIX/link-code" --with-decryption
