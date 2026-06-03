#!/usr/bin/env bash
# Materialize a fake pnpm workspace with 3 ACTIVE packages and 3
# ORPHANED paths. Each node_modules/ holds 1-2 MB of filler so the
# byte-budget signal is meaningful. Pearl th-96a3de.

set -euo pipefail
: "${WORKSPACE:?WORKSPACE env required}"

# Root workspace config — names the active packages.
cat > "$WORKSPACE/package.json" <<'JSON'
{
  "name": "fake-workspace-root",
  "version": "0.0.0",
  "private": true
}
JSON

cat > "$WORKSPACE/pnpm-workspace.yaml" <<'YAML'
packages:
  - "apps/web"
  - "packages/db"
  - "packages/ui"
YAML

# Three ACTIVE packages — each has package.json + node_modules/ that
# MUST survive.
for pkg in apps/web packages/db packages/ui; do
  mkdir -p "$WORKSPACE/$pkg/node_modules"
  cat > "$WORKSPACE/$pkg/package.json" <<JSON
{
  "name": "@smooai/${pkg##*/}",
  "version": "1.0.0"
}
JSON
  # Stuff each active node_modules with ~1.2 MB of filler so the
  # workspace looks realistic and the agent has something to du.
  dd if=/dev/zero of="$WORKSPACE/$pkg/node_modules/index.js" bs=1024 count=1200 2>/dev/null
done

# Three ORPHANED paths — these still have stale package.json + bloated
# node_modules but are NOT in pnpm-workspace.yaml.
for orphan in tools/legacy-codegen apps/old-admin packages/spike-feature; do
  mkdir -p "$WORKSPACE/$orphan/node_modules"
  cat > "$WORKSPACE/$orphan/package.json" <<JSON
{
  "name": "@smooai/${orphan##*/}",
  "version": "0.1.0",
  "description": "ORPHANED — not in pnpm-workspace.yaml"
}
JSON
  dd if=/dev/zero of="$WORKSPACE/$orphan/node_modules/index.js" bs=1024 count=1200 2>/dev/null
done

echo "setup.sh: workspace materialized at $WORKSPACE"
echo "  active   : apps/web, packages/db, packages/ui  (must preserve)"
echo "  orphans  : tools/legacy-codegen, apps/old-admin, packages/spike-feature"
echo "  total    : ~7.2 MB; orphan share ≈ 3.6 MB"
