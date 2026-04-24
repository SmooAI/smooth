#!/usr/bin/env bash
# Build a release-notes.md for `gh release edit --notes-file` by taking the
# existing release body and either (a) replacing an existing `<!-- THE-LINE -->`
# block with the freshly rendered summary, or (b) appending the summary at the
# end under a "The Line" section if no placeholder/prior block exists.
#
# Inputs:
#   $1  path to the rendered summary markdown (from render-markdown.sh)
#   $2  release tag (e.g. v0.42.1) — used to fetch existing body via `gh`
#   $3  output path for the combined release-notes.md
#
# Env:
#   DRY_RUN=1  skip the `gh release view` network call; treat existing body
#              as empty. Used by the workflow dispatch test path.
#
# The placeholder convention is a fenced marker pair:
#   <!-- THE-LINE:START -->
#   ...auto content...
#   <!-- THE-LINE:END -->
#
# If only `<!-- THE-LINE -->` (the single tag) appears, we replace that one
# line with the fenced block. This lets authors drop a single tag into their
# release description and have the bench result land in the right spot.

set -euo pipefail

summary_path="${1:?usage: update-release-notes.sh <summary.md> <tag> <output.md>}"
tag="${2:?usage: update-release-notes.sh <summary.md> <tag> <output.md>}"
output_path="${3:?usage: update-release-notes.sh <summary.md> <tag> <output.md>}"

if [[ ! -f "$summary_path" ]]; then
    echo "update-release-notes: summary not found: $summary_path" >&2
    exit 1
fi

summary=$(cat "$summary_path")

existing_body=""
if [[ "${DRY_RUN:-0}" != "1" ]]; then
    if ! existing_body=$(gh release view "$tag" --json body --jq .body 2>/dev/null); then
        echo "update-release-notes: no release found for $tag, starting with empty body" >&2
        existing_body=""
    fi
fi

fenced=$(printf '%s\n%s\n%s\n' \
    '<!-- THE-LINE:START -->' \
    "$summary" \
    '<!-- THE-LINE:END -->')

EXISTING_BODY="$existing_body" FENCED="$fenced" python3 - "$output_path" <<'PY'
import os, re, sys

existing = os.environ["EXISTING_BODY"]
fenced = os.environ["FENCED"]
out_path = sys.argv[1]

fenced_re = re.compile(
    r"<!--\s*THE-LINE:START\s*-->.*?<!--\s*THE-LINE:END\s*-->",
    re.DOTALL,
)
single_tag_re = re.compile(r"<!--\s*THE-LINE\s*-->")

if fenced_re.search(existing):
    new_body = fenced_re.sub(fenced, existing, count=1)
elif single_tag_re.search(existing):
    new_body = single_tag_re.sub(fenced, existing, count=1)
elif existing.strip() == "":
    new_body = fenced
else:
    new_body = existing.rstrip() + "\n\n" + fenced + "\n"

with open(out_path, "w", encoding="utf-8") as f:
    f.write(new_body)
    if not new_body.endswith("\n"):
        f.write("\n")
PY

echo "update-release-notes: wrote $output_path"
