#!/usr/bin/env bash
# Show install status + public health of a named copper-cloud deployment.
#   ./status.sh <name>
# Exit 0 when status=ready and /healthz answers 200 "ok", 1 otherwise.
set -euo pipefail
# shellcheck source=lib.sh
source "$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/lib.sh"

[ $# -eq 1 ] || die "usage: $0 <name>"
validate_name "$1"
state_paths "$1"
check_aws
REGION=$(region_for)

status=$(ssm_param "$REGION" "/copper-cloud/$1/status")
url=$(tf_output url)
echo "name:     $1 ($REGION)"
echo "status:   ${status:-<unset>}"
echo "url:      ${url:-<unknown: no state in $STATE_DIR>}"
echo "instance: $(tf_output instance_id)"
echo "db:       $(tf_output db_endpoint)"

rc=0
[ "$status" = "ready" ] || rc=1
if [ -n "$url" ]; then
  body=$(curl -sSk --max-time 10 "$url/healthz" 2>&1) && code=ok || code=fail
  if [ "$code" = ok ] && [ "$body" = "ok" ]; then
    echo "healthz:  ok"
  else
    echo "healthz:  FAIL (${body:-no response})"
    rc=1
  fi
  echo "shell:    $(tf_output ssm_session_command)"
fi
exit "$rc"
