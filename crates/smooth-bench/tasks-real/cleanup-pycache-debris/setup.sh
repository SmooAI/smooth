#!/usr/bin/env bash
# Materialize a polluted Python project layout under $WORKSPACE.
# Invoked by the score-cleanup harness. Idempotent for re-runs.
set -euo pipefail

: "${WORKSPACE:?WORKSPACE must be set by the harness}"
mkdir -p "$WORKSPACE"
cd "$WORKSPACE"

# Real files the agent must NOT delete.
cat > pyproject.toml <<'EOF'
[project]
name = "pkg"
version = "0.1.0"
EOF
cat > setup.py <<'EOF'
from setuptools import setup
setup(name="pkg", version="0.1.0")
EOF

mkdir -p src/pkg tests vendored
cat > src/pkg/__init__.py <<'EOF'
__version__ = "0.1.0"
EOF
cat > src/pkg/util.py <<'EOF'
def add(a, b):
    return a + b
EOF
cat > tests/conftest.py <<'EOF'
import pytest
EOF
cat > vendored/six.py <<'EOF'
# vendored from PSF — DO NOT TOUCH
PY3 = True
EOF

# Debris #1 — __pycache__ dirs with fake .pyc content.
# 50 dirs × ~24KB each ≈ 1.2MB.
for i in $(seq 1 50); do
    dir="src/pkg/sub_${i}/__pycache__"
    mkdir -p "$dir"
    # Three .pyc files per cache dir, mixed sizes adding to ~24KB.
    dd if=/dev/urandom of="$dir/util.cpython-313.pyc" bs=1024 count=12 status=none
    dd if=/dev/urandom of="$dir/core.cpython-313.pyc" bs=1024 count=8 status=none
    dd if=/dev/urandom of="$dir/helper.cpython-313.pyc" bs=1024 count=4 status=none
    # Plus a real source so the package is plausible.
    echo "x = $i" > "src/pkg/sub_${i}/__init__.py"
done

# Debris #2 — .pytest_cache.
mkdir -p .pytest_cache/v/cache
echo '{"lastfailed": {}}' > .pytest_cache/v/cache/lastfailed
dd if=/dev/urandom of=.pytest_cache/v/cache/nodeids bs=1024 count=20 status=none

# Debris #3 — egg-info dir.
mkdir -p src/pkg.egg-info
cat > src/pkg.egg-info/PKG-INFO <<'EOF'
Metadata-Version: 2.1
Name: pkg
Version: 0.1.0
EOF
dd if=/dev/urandom of=src/pkg.egg-info/SOURCES.txt bs=1024 count=10 status=none

# Debris #4 — orphaned top-level .pyc files (pre-Python-3.2 layout).
for i in 1 2 3 4 5; do
    dd if=/dev/urandom of="src/pkg/legacy_${i}.pyc" bs=1024 count=8 status=none
done

echo "setup.sh: workspace polluted at $WORKSPACE" >&2
