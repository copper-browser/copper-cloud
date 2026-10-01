#!/usr/bin/env bash
# Print the link code of a named copper-cloud deployment (from SSM).
#   ./link-code.sh <name>
set -euo pipefail
# shellcheck source=lib.sh
source "$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/lib.sh"

[ $# -eq 1 ] || die "usage: $0 <name>"
validate_name "$1"
state_paths "$1"
check_aws
REGION=$(region_for)

code=$(ssm_param "$REGION" "/copper-cloud/$1/link-code" --with-decryption)
case "$code" in
  copper-cloud://*) printf '%s\n' "$code" ;;
  "") die "no link code for '$1' in $REGION (not deployed?)" ;;
  *)
    status=$(ssm_param "$REGION" "/copper-cloud/$1/status")
    die "link code not published yet (status: ${status:-<unset>}); try ./status.sh $1"
    ;;
esac
