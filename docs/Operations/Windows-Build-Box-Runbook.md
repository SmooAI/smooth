# Windows Build Box Runbook

A throwaway Windows EC2 driven entirely over **SSM** (no RDP, no inbound
ports) for iterating on Windows builds faster than GitHub Actions
round-trips. Used to bring up the CGO/ICU/gozstd toolchain and prove out
`smooth-dolt` + the pearl server on Windows (pearls th-5f35a5 / th-20f330
follow-ups).

> **Why not GitHub Actions?** A red Windows job is a ~10-minute push →
> queue → run → read-log loop. An SSM box is a ~10-second
> `send-command` → poll loop against a warm machine with the toolchain
> already installed. Use Actions for the *gate* (the CI matrix already
> runs `cargo nextest` on `windows-latest`); use the box for
> *iteration*.

## Why Windows is hard here

`smooth-dolt` embeds the full Dolt engine, which pulls **CGO**
dependencies:

- `gozstd` — Zstandard C bindings (bundles its own C, needs a C
  compiler).
- `go-icu-regex` — ICU bindings (needs ICU headers/libs). The
  `gms_pure_go` build tag avoids some of this; confirm empirically.

So Windows needs a working **C compiler** (mingw-w64 gcc) on PATH with
`CGO_ENABLED=1`. This is the whole reason the box exists — the pearl
store can't run on Windows until `smooth-dolt.exe` builds there.

## Provision (from macOS/Linux, `assume smooai.dev`)

One-time IAM (SSM needs an instance profile — none existed):

```bash
ROLE=smooth-win-build-ssm
aws iam create-role --role-name $ROLE \
  --assume-role-policy-document '{"Version":"2012-10-17","Statement":[{"Effect":"Allow","Principal":{"Service":"ec2.amazonaws.com"},"Action":"sts:AssumeRole"}]}'
aws iam attach-role-policy --role-name $ROLE \
  --policy-arn arn:aws:iam::aws:policy/AmazonSSMManagedInstanceCore
aws iam create-instance-profile --instance-profile-name $ROLE
aws iam add-role-to-instance-profile --instance-profile-name $ROLE --role-name $ROLE
```

Launch (Windows Server 2022, public subnet, m5.2xlarge, 120 GB — Go +
Rust builds are heavy; the base 30 GB Windows disk is too small):

```bash
AMI=$(aws ssm get-parameter --name /aws/service/ami-windows-latest/Windows_Server-2022-English-Full-Base --query Parameter.Value --output text)
aws ec2 run-instances --image-id $AMI --instance-type m5.2xlarge \
  --iam-instance-profile Name=smooth-win-build-ssm \
  --subnet-id <public-subnet> --associate-public-ip-address \
  --block-device-mappings 'DeviceName=/dev/sda1,Ebs={VolumeSize=120,VolumeType=gp3}' \
  --tag-specifications 'ResourceType=instance,Tags=[{Key=Name,Value=smooth-win-build},{Key=Temporary,Value=true}]' \
  --query 'Instances[0].InstanceId' --output text
```

No inbound security-group rules needed — the SSM agent (default on AWS
Windows AMIs) dials **out** to the SSM endpoints. Windows registers with
SSM a few minutes after boot:

```bash
aws ssm describe-instance-information \
  --filters Key=InstanceIds,Values=<iid> \
  --query 'InstanceInformationList[0].PingStatus'   # -> Online
```

## Drive it over SSM

`scripts/win-ssm/winrun.sh` (mirrored in this runbook below) sends a
PowerShell script and polls for output. **Gotcha:** the SSM
`--parameters` *shorthand* mangles newlines — pass a JSON **file**
instead (`{"commands":["<whole script>"]}` via `--parameters file://…`).

```bash
#!/bin/zsh
IID=<iid>; SCRIPT="$1"; TIMEOUT="${2:-600}"
PARAMS=$(mktemp /tmp/ssm.XXXX.json)
python3 -c 'import json,sys;print(json.dumps({"commands":[open(sys.argv[1]).read()]}))' "$SCRIPT" > "$PARAMS"
CID=$(aws ssm send-command --instance-ids "$IID" \
  --document-name AWS-RunPowerShellScript \
  --parameters "file://$PARAMS" --timeout-seconds "$TIMEOUT" \
  --query Command.CommandId --output text)
while true; do
  ST=$(aws ssm get-command-invocation --command-id "$CID" --instance-id "$IID" --query Status --output text 2>/dev/null || echo Pending)
  case "$ST" in Success|Failed|Cancelled|TimedOut) break;; esac; sleep 6
done
echo "STATUS: $ST"
aws ssm get-command-invocation --command-id "$CID" --instance-id "$IID" --query StandardOutputContent --output text
aws ssm get-command-invocation --command-id "$CID" --instance-id "$IID" --query StandardErrorContent --output text
```

### Toolchain (Chocolatey)

```powershell
Set-ExecutionPolicy Bypass -Scope Process -Force
[Net.ServicePointManager]::SecurityProtocol = 3072
iex ((New-Object Net.WebClient).DownloadString('https://community.chocolatey.org/install.ps1'))
choco install -y --no-progress golang mingw git 7zip
# add Rust later for the workspace: choco install -y rust protoc, or rustup-init
```

Installs land at `C:\Program Files\Go\bin` (go) and
`C:\ProgramData\mingw64\mingw64\bin` (gcc). Machine PATH updates don't
reach the current SSM session — prepend them explicitly each script.

### Get source onto the box without a private clone

The box has no GitHub creds. For small trees (`go/smooth-dolt` is ~59 KB
tarred) ship it **inline as base64** in a PowerShell script — decode with
`[IO.File]::WriteAllBytes(...,[Convert]::FromBase64String($b64))` then
`tar -xzf` (tar ships with Windows). SSM's command payload cap is ~100 KB,
so this covers the Go binary source; for the full Rust workspace use S3
(a presigned URL curl'd on the box) or `git clone` with a short-lived PAT.

### Build smooth-dolt

```powershell
$env:Path = "C:\Program Files\Go\bin;C:\ProgramData\mingw64\mingw64\bin;$env:Path"
$env:CGO_ENABLED = "1"; $env:CC = "gcc"
cd C:\src\smooth-dolt
go build -tags gms_pure_go -o smooth-dolt.exe .
```

First build downloads + compiles the entire Dolt module (+ gozstd CGO) —
budget 10–20 min and a generous `send-command --timeout-seconds`.

## Teardown — ALWAYS do this

The box is billed per hour (~$0.40–0.55/hr for m5.2xlarge Windows). Kill
it the moment the iteration loop is done:

```bash
aws ec2 terminate-instances --instance-ids <iid>
# once terminated, remove the throwaway IAM:
aws iam remove-role-from-instance-profile --instance-profile-name smooth-win-build-ssm --role-name smooth-win-build-ssm
aws iam delete-instance-profile --instance-profile-name smooth-win-build-ssm
aws iam detach-role-policy --role-name smooth-win-build-ssm --policy-arn arn:aws:iam::aws:policy/AmazonSSMManagedInstanceCore
aws iam delete-role --role-name smooth-win-build-ssm
```

Everything is tagged `Temporary=true` / `Project=smooth-win-build` so a
sweep can find leftovers:
`aws ec2 describe-instances --filters Name=tag:Project,Values=smooth-win-build`.
