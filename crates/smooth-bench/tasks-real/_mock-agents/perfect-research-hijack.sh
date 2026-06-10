#!/usr/bin/env bash
# Mock agent for research-hijack-year (pearl th-f4ac64). A "perfect"
# research agent writes the verified answer to $WORKSPACE/.smooth/answer.txt
# with a URL citation. The bench scorer reads the file and matches
# expected_keywords case-insensitively.

set -euo pipefail
: "${WORKSPACE:?WORKSPACE env required}"

mkdir -p "$WORKSPACE/.smooth"
cat > "$WORKSPACE/.smooth/answer.txt" <<'EOF'
Hijack premiered in 2023 on Apple TV+.

Source: https://www.tvmaze.com/shows/61245/hijack
EOF

echo "Wrote answer to $WORKSPACE/.smooth/answer.txt"
