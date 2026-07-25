---
name: windows-build-box
description: Spin up a throwaway Windows EC2 build box driven over SSM (no RDP, no inbound ports) to build/test SmooAI code on Windows faster than GitHub Actions round-trips — then tear it down. Use when a Windows build/toolchain needs iterating (CGO/mingw, cross-platform Rust/Go), when CI's windows-latest job is red and the push→queue→log loop is too slow, or on "windows build box", "test on windows", "windows CI is failing". Always tears the box down after.
---

# windows-build-box — throwaway Windows EC2 over SSM

A red `windows-latest` CI job is a ~10-minute push → queue → run → read-log
loop. An SSM-driven EC2 is a ~10-second `send-command` → poll loop against a
warm machine with the toolchain already installed. Use **Actions for the gate**
(the CI matrix) and **the box for iteration**.

Everything runs over **SSM** — no RDP, no inbound security-group rules. The box
is tagged `Temporary=true`; **always tear it down when done** (it bills per hour).

## Provision (assume the right AWS account, e.g. `assume smooai.dev`)

One-time IAM (SSM needs an instance profile):

```bash
ROLE=smooth-win-build-ssm
aws iam create-role --role-name $ROLE \
  --assume-role-policy-document '{"Version":"2012-10-17","Statement":[{"Effect":"Allow","Principal":{"Service":"ec2.amazonaws.com"},"Action":"sts:AssumeRole"}]}'
aws iam attach-role-policy --role-name $ROLE --policy-arn arn:aws:iam::aws:policy/AmazonSSMManagedInstanceCore
aws iam create-instance-profile --instance-profile-name $ROLE
aws iam add-role-to-instance-profile --instance-profile-name $ROLE --role-name $ROLE
```

Launch (Win Server 2022, public subnet, m5.2xlarge, 120 GB — Go+Rust builds are
heavy and the base 30 GB disk is too small). No inbound rules needed; the SSM
agent (default on AWS Windows AMIs) dials out.

```bash
AMI=$(aws ssm get-parameter --name /aws/service/ami-windows-latest/Windows_Server-2022-English-Full-Base --query Parameter.Value --output text)
SUBNET=$(aws ec2 describe-subnets --filters Name=map-public-ip-on-launch,Values=true --query 'Subnets[0].SubnetId' --output text)
aws ec2 run-instances --image-id $AMI --instance-type m5.2xlarge \
  --iam-instance-profile Name=smooth-win-build-ssm \
  --subnet-id $SUBNET --associate-public-ip-address \
  --block-device-mappings 'DeviceName=/dev/sda1,Ebs={VolumeSize=120,VolumeType=gp3}' \
  --tag-specifications 'ResourceType=instance,Tags=[{Key=Name,Value=smooth-win-build},{Key=Project,Value=smooth-win-build},{Key=Temporary,Value=true}]' \
  --query 'Instances[0].InstanceId' --output text
```

Windows registers with SSM a few minutes after boot:

```bash
aws ssm describe-instance-information --filters Key=InstanceIds,Values=<iid> \
  --query 'InstanceInformationList[0].PingStatus'   # -> Online
```

## Drive it

Use `winrun.sh` (next to this SKILL.md) — it sends a PowerShell script and polls
for output. **Gotcha it already handles:** the SSM `--parameters` *shorthand*
mangles newlines, so it passes a JSON file instead.

```bash
WIN_IID=<iid> ${CLAUDE_PLUGIN_ROOT}/skills/windows-build-box/winrun.sh my-script.ps1 1200
```

Toolchain (Chocolatey → Go, mingw gcc, git). Installs land at
`C:\Program Files\Go\bin` and `C:\ProgramData\mingw64\mingw64\bin`; machine PATH
edits don't reach the current SSM session, so prepend them in each script:

```powershell
Set-ExecutionPolicy Bypass -Scope Process -Force
[Net.ServicePointManager]::SecurityProtocol = 3072
iex ((New-Object Net.WebClient).DownloadString('https://community.chocolatey.org/install.ps1'))
choco install -y --no-progress golang mingw git 7zip
$env:Path = "C:\Program Files\Go\bin;C:\ProgramData\mingw64\mingw64\bin;$env:Path"
$env:CGO_ENABLED = "1"
```

**Get private source onto the box without GitHub creds:** small trees ship inline
as base64 in a PowerShell script — `[IO.File]::WriteAllBytes(...,[Convert]::FromBase64String($b64))`
then `tar -xzf` (tar ships with Windows). SSM's payload cap is ~100 KB; for larger
trees use a presigned S3 URL curl'd on the box, or `git clone` with a short-lived PAT.

**Known result (pearl th-5f35a5):** `smooth-dolt` (embedded Dolt, CGO) builds on
Windows with `-tags gms_pure_go` — no ICU needed, only mingw gcc. If a Go build
fails with `undefined: syscall.SIG…`, that's a Unix-only signal needing a
`//go:build !windows` split, not a toolchain problem.

## Teardown — ALWAYS

```bash
aws ec2 terminate-instances --instance-ids <iid>
aws iam remove-role-from-instance-profile --instance-profile-name smooth-win-build-ssm --role-name smooth-win-build-ssm
aws iam delete-instance-profile --instance-profile-name smooth-win-build-ssm
aws iam detach-role-policy --role-name smooth-win-build-ssm --policy-arn arn:aws:iam::aws:policy/AmazonSSMManagedInstanceCore
aws iam delete-role --role-name smooth-win-build-ssm
```

Find leftovers:
`aws ec2 describe-instances --filters Name=tag:Project,Values=smooth-win-build Name=instance-state-name,Values=running,pending,stopped`.

The smooth repo also keeps a fuller writeup at
`docs/Operations/Windows-Build-Box-Runbook.md`.
