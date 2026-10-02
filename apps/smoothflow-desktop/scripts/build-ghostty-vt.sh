#!/usr/bin/env bash
# Build libghostty-vt for SmoothFlow Desktop (th-872ea8): the static library
# src/vt/ffi.rs links, plus its C headers. build.rs runs this when the library
# for the target being compiled is missing; CI runs it first so the result can
# be cached.
#
# Output (gitignored): apps/smoothflow-desktop/.ghostty-vt/out/<rust-target>/
#   include/ghostty/vt.h …
#   lib/libghostty-vt.a          (macOS, Linux)
#   lib/ghostty-vt-static.lib    (Windows MSVC)
#
# Why from source: Ghostty publishes no libghostty-vt binaries. It is
# zero-dependency Zig, and Ghostty's own CI builds it for macOS, Linux and
# Windows, so this is one `zig build -Demit-lib-vt`. The Android app
# (smooai apps/smoothflow-mobile/android/scripts/build-ghostty-vt.sh) does the
# same thing with the NDK.
#
# Everything is pinned in ../ghostty-vt.lock: the ghostty commit (the same one
# the Mac, iOS and Android apps link) and the Zig release (sha256-verified,
# fetched from community mirrors first). Idempotent: a stamp of the lock and
# this script short-circuits. Needs bash, git, curl, tar (and unzip on Windows).
#
#   scripts/build-ghostty-vt.sh                          # the host's Rust target
#   scripts/build-ghostty-vt.sh x86_64-apple-darwin      # an explicit one
#   GHOSTTY_VT_WORK=/elsewhere scripts/build-ghostty-vt.sh
set -euo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
APP_DIR="$(dirname "$HERE")"
LOCK="$APP_DIR/ghostty-vt.lock"
WORK="${GHOSTTY_VT_WORK:-$APP_DIR/.ghostty-vt}"
mkdir -p "$WORK"
WORK="$(cd "$WORK" && pwd)"

lockval() { awk -F= -v k="$1" '$1 == k { print $2; exit }' "$LOCK" | tr -d '\r'; }
ghostty_repo="$(lockval ghostty_repo)"
ghostty_sha="$(lockval ghostty_sha)"
zig_version="$(lockval zig_version)"
[[ -n "$ghostty_repo" && -n "$ghostty_sha" && -n "$zig_version" ]] || { echo "error: $LOCK is incomplete" >&2; exit 1; }

sha256() { if command -v sha256sum >/dev/null; then sha256sum "$@"; else shasum -a 256 "$@"; fi | awk '{print $1}'; }

# --- host ---
windows=0
case "$(uname -s)-$(uname -m)" in
  Linux-x86_64) zig_host=x86_64-linux host_triple=x86_64-unknown-linux-gnu ;;
  Linux-aarch64) zig_host=aarch64-linux host_triple=aarch64-unknown-linux-gnu ;;
  Darwin-arm64) zig_host=aarch64-macos host_triple=aarch64-apple-darwin ;;
  Darwin-x86_64) zig_host=x86_64-macos host_triple=x86_64-apple-darwin ;;
  MINGW*-x86_64 | MSYS*-x86_64 | CYGWIN*-x86_64) zig_host=x86_64-windows host_triple=x86_64-pc-windows-msvc windows=1 ;;
  *) echo "error: unsupported build host $(uname -s)-$(uname -m)" >&2; exit 1 ;;
esac

# --- target: a Rust triple → a Zig target ---
target="${1:-${GHOSTTY_VT_TARGET:-$host_triple}}"
# On Windows the static lib bundles no SIMD dependencies (Ghostty's
# CMakeLists says consumers must link highway/simdutf themselves), so build it
# without SIMD there: the archive then needs only ntdll + kernel32.
simd=true
case "$target" in
  aarch64-apple-darwin) zig_target=aarch64-macos lib=libghostty-vt.a ;;
  x86_64-apple-darwin) zig_target=x86_64-macos lib=libghostty-vt.a ;;
  x86_64-unknown-linux-gnu) zig_target=x86_64-linux-gnu lib=libghostty-vt.a ;;
  aarch64-unknown-linux-gnu) zig_target=aarch64-linux-gnu lib=libghostty-vt.a ;;
  x86_64-pc-windows-msvc) zig_target=x86_64-windows-msvc lib=ghostty-vt-static.lib simd=false ;;
  *) echo "error: no libghostty-vt mapping for Rust target $target" >&2; exit 1 ;;
esac

OUT="$WORK/out/$target"
stamp_want="$(cat "$LOCK" "$HERE/build-ghostty-vt.sh" | sha256 /dev/stdin) $target"
if [[ -f "$OUT/.stamp" && "$(cat "$OUT/.stamp")" == "$stamp_want" && -f "$OUT/lib/$lib" && -f "$OUT/include/ghostty/vt.h" ]]; then
  echo "==> libghostty-vt already built for $target (ghostty ${ghostty_sha:0:12})"
  exit 0
fi
mkdir -p "$WORK"

# --- zig (pinned, sha256-verified) ---
ZIG_DIR="$WORK/zig-$zig_host-$zig_version"
ZIG="$ZIG_DIR/zig"
[[ $windows == 1 ]] && ZIG="$ZIG_DIR/zig.exe"
if [[ ! -x "$ZIG" ]]; then
  want="$(lockval "zig_sha256_$zig_host")"
  [[ -n "$want" ]] || { echo "error: no zig sha256 for $zig_host in $LOCK" >&2; exit 1; }
  ext=tar.xz
  [[ $windows == 1 ]] && ext=zip
  echo "==> Fetching Zig $zig_version ($zig_host)"
  tmp="$(mktemp -d)"
  # ziglang.org asks automated downloads to prefer community mirrors (and it can
  # crawl: 11 minutes on a CI runner). Any mirror is fine — the sha256 is pinned.
  got=""
  for base in ${ZIG_MIRRORS:-https://pkg.machengine.org/zig https://zigmirror.hryx.net/zig https://ziglang.org/download}; do
    if curl -fsSL --retry 2 --retry-delay 2 --connect-timeout 20 --speed-limit 50000 --speed-time 30 --max-time 600 -o "$tmp/zig.$ext" "$base/$zig_version/zig-$zig_host-$zig_version.$ext"; then
      got="$(sha256 "$tmp/zig.$ext")"
      [[ "$got" == "$want" ]] && break
      echo "warning: zig from $base failed its checksum ($got); trying the next mirror" >&2
    fi
  done
  [[ "$got" == "$want" ]] || { echo "error: no mirror served Zig $zig_version with sha256 $want" >&2; exit 1; }
  rm -rf "$ZIG_DIR"
  if [[ $windows == 1 ]]; then
    unzip -q "$tmp/zig.$ext" -d "$WORK"
  else
    tar -xJf "$tmp/zig.$ext" -C "$WORK"
  fi
  rm -rf "$tmp"
fi
[[ "$("$ZIG" version | tr -d '\r')" == "$zig_version" ]] || { echo "error: $ZIG is not Zig $zig_version" >&2; exit 1; }
# ghostty's build.zig shells out to `zig env`: the pinned zig must be first on PATH.
export PATH="$ZIG_DIR:$PATH"

# Zig's caches (and the packages build.zig.zon fetches) stay inside $WORK.
native() { if [[ $windows == 1 ]]; then cygpath -w "$1"; else printf '%s' "$1"; fi; }
export ZIG_GLOBAL_CACHE_DIR="$(native "$WORK/zig-cache/global")"
export ZIG_LOCAL_CACHE_DIR="$(native "$WORK/zig-cache/local")"

# --- ghostty source at the pinned commit ---
SRC="$WORK/ghostty"
if [[ "$(git -C "$SRC" rev-parse HEAD 2>/dev/null || true)" != "$ghostty_sha" ]]; then
  echo "==> Fetching ghostty ${ghostty_sha:0:12}"
  rm -rf "$SRC"
  git init -q "$SRC"
  git -C "$SRC" remote add origin "$ghostty_repo"
  git -C "$SRC" fetch -q --depth 1 origin "$ghostty_sha"
  git -C "$SRC" -c core.autocrlf=false -c advice.detachedHead=false checkout -q FETCH_HEAD
fi

# --- zig build lib-vt ---
echo "==> libghostty-vt for $target (zig -Dtarget=$zig_target, simd=$simd)"
rm -rf "$OUT"
extra=()
# The xcframework is for Xcode consumers; Cargo links the plain archive.
[[ "$zig_target" == *macos ]] && extra+=(-Demit-xcframework=false)
(cd "$SRC" && "$ZIG" build -Demit-lib-vt "-Dtarget=$zig_target" "-Dsimd=$simd" -Doptimize=ReleaseFast ${extra[@]+"${extra[@]}"} --prefix "$(native "$OUT")")
[[ -f "$OUT/lib/$lib" ]] || { echo "error: zig build produced no $OUT/lib/$lib" >&2; ls -R "$OUT" >&2 || true; exit 1; }

# --- keep libghostty-vt's memset to itself ---
# Ghostty exports its own vectorized `memset` (src/quirks_memset.zig: weak in a
# static lib, to beat Zig 0.16's scalar compiler_rt one). Linked into a Rust
# binary, that weak definition still wins over libSystem's/glibc's for the
# WHOLE program, and Rust code then misbehaves: hashbrown's table inserts trip
# their own bounds assertion. Localize it, so libghostty-vt keeps using it and
# everything else binds the platform memset. (COFF never emits it.)
archive="$OUT/lib/$lib"
case "$zig_target" in
  *macos)
    sym=_memset
    members="$(nm -A -g "$archive" 2>/dev/null | awk -v s="$sym" '$NF == s && $(NF-1) ~ /^[TW]$/ { m = $1; sub(/^.*\.a:/, "", m); sub(/:$/, "", m); print m }' | sort -u)"
    if [[ -n "$members" ]]; then
      tmp="$(mktemp -d)"
      for m in $members; do
        (cd "$tmp" && xcrun ar x "$archive" "$m" && chmod 644 "$m" &&
          xcrun nm -g "$m" | awk '$2 ~ /^[A-TV-Z]$/ {print $3}' | grep -vx "$sym" | sort -u >keep.txt &&
          xcrun nmedit -s keep.txt "$m" && xcrun ar r "$archive" "$m" 2>/dev/null)
      done
      xcrun ranlib "$archive" 2>/dev/null || true
      rm -rf "$tmp"
    fi
    ;;
  *linux*)
    sym=memset
    objcopy="$(command -v objcopy || command -v llvm-objcopy || command -v /opt/homebrew/opt/llvm/bin/llvm-objcopy || true)"
    [[ -n "$objcopy" ]] || { echo "error: objcopy (binutils) or llvm-objcopy is needed to localize memset" >&2; exit 1; }
    "$objcopy" --localize-symbol=memset "$archive"
    ;;
  *) sym="" ;;
esac
if [[ -n "$sym" ]] && nm -g "$archive" 2>/dev/null | awk -v s="$sym" '$NF == s && $(NF-1) ~ /^[TW]$/ { found = 1 } END { exit !found }'; then
  echo "error: $archive still exports $sym" >&2
  exit 1
fi

# Only the static archive is linked; drop the shared library so nothing picks it up.
find "$OUT/lib" -maxdepth 1 \( -name '*.so*' -o -name '*.dylib' -o -name '*.dll' -o -name 'ghostty-vt.lib' -o -name '*.pdb' \) -exec rm -rf {} + 2>/dev/null || true
ls -l "$OUT/lib"
echo "$stamp_want" >"$OUT/.stamp"
