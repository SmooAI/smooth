#!/usr/bin/env bash
# Append a single pre-rendered history row to docs/bench-history.md.
#
# If the file does not exist, we create it with the standard header + table
# skeleton before appending.
#
# Inputs:
#   $1  path to docs/bench-history.md (will be created if absent)
#   $2  path to a file containing the one-line history row (from render-markdown.sh)

set -euo pipefail

history_path="${1:?usage: append-history.sh <bench-history.md> <row-file>}"
row_file="${2:?usage: append-history.sh <bench-history.md> <row-file>}"

if [[ ! -f "$row_file" ]]; then
    echo "append-history: row file not found: $row_file" >&2
    exit 1
fi

row_content=$(cat "$row_file")
if [[ -z "${row_content// /}" ]]; then
    echo "append-history: row file is empty: $row_file" >&2
    exit 1
fi

if [[ ! -f "$history_path" ]]; then
    mkdir -p "$(dirname "$history_path")"
    cat >"$history_path" <<'HEADER'
# Smooth — The Line History

This file is auto-maintained by `.github/workflows/the-line.yml`. Do not edit
by hand — the release workflow appends a row per tagged version.

Each row is the output of a `smooth-bench score --release` sweep published
alongside the corresponding GitHub Release. "overall%" is the single-number
"The Line" Smoo AI publishes; the per-language columns are the 20-task
sub-sweeps.

| version | date | overall% | cpp% | go% | java% | javascript% | python% | rust% | cost_usd | commit_sha |
| --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- |
HEADER
fi

# Append without trailing blank line shenanigans: ensure file ends with a
# newline, then append exactly the row (which already ends in \n).
if [[ -s "$history_path" ]]; then
    tail -c 1 "$history_path" | od -An -c | grep -q '\\n' || printf '\n' >>"$history_path"
fi

printf '%s\n' "$row_content" >>"$history_path"

echo "append-history: appended row to $history_path"
