# shellcheck shell=bash
# shellcheck disable=SC2034 # constants are used by the sourcing scripts
# Shared helpers for up.sh / down.sh / link-code.sh / status.sh. Sourced, not run.

DEPLOY_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_SLUG="${COPPER_CLOUD_REPO:-copper-browser/copper-cloud}"
DEFAULT_REGION="us-east-1"
DEFAULT_INSTANCE_TYPE="t3.small"

log() { printf '==> %s\n' "$*" >&2; }
warn() { printf 'WARN: %s\n' "$*" >&2; }
die() {
  printf 'ERROR: %s\n' "$*" >&2
  exit 1
}

need() { command -v "$1" >/dev/null 2>&1 || die "'$1' not found in PATH. $2"; }

# terraform, or OpenTofu as a drop-in.
tf_bin() {
  if [ -n "${TERRAFORM:-}" ]; then
    printf '%s\n' "$TERRAFORM"
  elif command -v terraform >/dev/null 2>&1; then
    printf 'terraform\n'
  elif command -v tofu >/dev/null 2>&1; then
    printf 'tofu\n'
  else
    die "terraform (or tofu) not found. brew install hashicorp/tap/terraform"
  fi
}

validate_name() {
  local n=$1
  [[ $n =~ ^[a-z][a-z0-9-]{0,22}[a-z0-9]$ && $n != *--* ]] ||
    die "invalid name '$n': 2-24 chars, lowercase letters/digits/single hyphens, starts with a letter."
}

# Sets STATE_DIR / STATE_FILE / TFVARS for deployment $1.
state_paths() {
  STATE_DIR="$DEPLOY_DIR/state/$1"
  STATE_FILE="$STATE_DIR/terraform.tfstate"
  TFVARS="$STATE_DIR/terraform.tfvars"
}

# tfvar_get <file> <key> → value of a simple `key = "value"` / `key = value` line.
tfvar_get() {
  [ -f "$1" ] || return 0
  sed -nE "s/^[[:space:]]*$2[[:space:]]*=[[:space:]]*\"?([^\"]*)\"?[[:space:]]*$/\1/p" "$1" | tail -n1
}

region_for() {
  local r
  r=$(tfvar_get "$TFVARS" region)
  printf '%s\n' "${r:-${AWS_REGION:-${AWS_DEFAULT_REGION:-$DEFAULT_REGION}}}"
}

check_aws() {
  need aws "Install the AWS CLI v2."
  aws sts get-caller-identity --output text --query Account >/dev/null 2>&1 ||
    die "AWS credentials not usable. Run: aws sso login${AWS_PROFILE:+ --profile $AWS_PROFILE}"
}

# One shared .terraform/ for all deployments; serialize `init` so parallel
# up.sh runs don't race on the provider install.
tf_init() {
  local tf lock="$DEPLOY_DIR/.terraform-init.lock" waited=0
  tf=$(tf_bin)
  until mkdir "$lock" 2>/dev/null; do
    if [ "$waited" -ge 600 ]; then
      die "timed out waiting for $lock (remove it if no other up.sh is running)"
    fi
    sleep 2
    waited=$((waited + 2))
  done
  # shellcheck disable=SC2064
  trap "rmdir '$lock' 2>/dev/null || true" EXIT
  if [ ! -d "$DEPLOY_DIR/.terraform/providers" ]; then
    log "terraform init"
    "$tf" -chdir="$DEPLOY_DIR" init -input=false >&2
  fi
  rmdir "$lock" 2>/dev/null || true
  trap - EXIT
}

# ssm_param <region> <name> [--with-decryption] → value, or empty if missing.
ssm_param() {
  local region=$1 pname=$2
  shift 2
  aws ssm get-parameter --region "$region" --name "$pname" "$@" \
    --query Parameter.Value --output text 2>/dev/null || true
}

tf_output() {
  local tf
  tf=$(tf_bin)
  [ -f "$STATE_FILE" ] || return 0
  "$tf" -chdir="$DEPLOY_DIR" output -state="$STATE_FILE" -raw "$1" 2>/dev/null || true
}
