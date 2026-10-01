#!/usr/bin/env bash
# Destroy a named copper-cloud deployment (EC2, EIP, RDS, S3, IAM, SSM params).
#
#   ./down.sh <name> [--purge]   # --purge also deletes state/<name>/ afterwards
#
# Idempotent: a deployment with no state (or already destroyed) is a no-op.
set -euo pipefail
# shellcheck source=lib.sh
source "$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/lib.sh"

[ $# -ge 1 ] || die "usage: $0 <name> [--purge]"
NAME=$1
PURGE=${2:-}
validate_name "$NAME"
state_paths "$NAME"

if [ ! -f "$STATE_FILE" ]; then
  log "no state for '$NAME' ($STATE_FILE) — nothing to destroy"
  exit 0
fi
[ -f "$TFVARS" ] || die "missing $TFVARS (needed for destroy)"

check_aws
TF=$(tf_bin)
tf_init
log "terraform destroy ($STATE_FILE)"
"$TF" -chdir="$DEPLOY_DIR" destroy -input=false -auto-approve \
  -state="$STATE_FILE" -var-file="$TFVARS"

if [ "$PURGE" = "--purge" ]; then
  rm -rf "$STATE_DIR"
  log "removed $STATE_DIR"
fi
log "'$NAME' destroyed"
