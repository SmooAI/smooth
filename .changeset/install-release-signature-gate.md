---
'@smooai/smooth': patch
---

install-release.sh: fix the signature gate that refused every legitimate release

Two independent bugs made `install-release.sh` reject the official signed build; it was caught installing 0.2.3 by hand.

- `codesign -dv` alone prints no `Authority=` lines; they require `--verbose=2`.
- Under `set -o pipefail`, `producer | grep -q` returns 141 — `grep -q` exits on the first match and SIGPIPEs the producer, so a _match_ read as a failure. This affected every verification in the script, not just `codesign`: the Gatekeeper check, the Mach-O filter and the dylib scan had the same shape.

All six check sites now capture output first and grep from a here-string. `install-release.test.sh` covers both causes, including a behavioural proof that an ad-hoc signed bundle is rejected, and refuses any future `| grep -q` while pipefail is on.
