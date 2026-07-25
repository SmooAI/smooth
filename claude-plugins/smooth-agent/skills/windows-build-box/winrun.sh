#!/bin/zsh
# Run a PowerShell script on a Windows EC2 build box over SSM and stream
# its output. No RDP / no inbound ports — SSM only.
#
# Usage:  WIN_IID=i-0abc... ./winrun.sh <script.ps1> [timeout-sec]
#
# Requires: AWS creds for the box's account (e.g. `assume smooai.dev`) and
# an instance registered with SSM (PingStatus=Online). See
# docs/Operations/Windows-Build-Box-Runbook.md for provisioning + teardown.
set -e

IID="${WIN_IID:?set WIN_IID to the target instance id}"
SCRIPT="${1:?usage: winrun.sh <script.ps1> [timeout-sec]}"
TIMEOUT="${2:-600}"

# The SSM --parameters *shorthand* mangles newlines; pass a JSON file so
# the whole script arrives as one intact command string.
PARAMS=$(mktemp /tmp/ssm-params.XXXX.json)
trap 'rm -f "$PARAMS"' EXIT
python3 -c 'import json,sys; print(json.dumps({"commands":[open(sys.argv[1]).read()]}))' "$SCRIPT" > "$PARAMS"

CID=$(aws ssm send-command \
  --instance-ids "$IID" \
  --document-name AWS-RunPowerShellScript \
  --parameters "file://$PARAMS" \
  --timeout-seconds "$TIMEOUT" \
  --query "Command.CommandId" --output text)
echo "cmd=$CID"

while true; do
  ST=$(aws ssm get-command-invocation --command-id "$CID" --instance-id "$IID" --query "Status" --output text 2>/dev/null || echo Pending)
  case "$ST" in
    Success|Failed|Cancelled|TimedOut) break ;;
  esac
  sleep 6
done

echo "=== STATUS: $ST ==="
echo "--- STDOUT ---"
aws ssm get-command-invocation --command-id "$CID" --instance-id "$IID" --query "StandardOutputContent" --output text
echo "--- STDERR ---"
aws ssm get-command-invocation --command-id "$CID" --instance-id "$IID" --query "StandardErrorContent" --output text
