---
'@smooai/smooth': patch
---

`install-release.sh` now unregisters the bundles **nested inside** a stray SmoothFlow build, not just the outer `.app`. A debug build registers its own Sparkle `Updater.app` and `SmoothFlowUITests-Runner.app` as separate LaunchServices entries, and a single `lsregister -u` on the outer bundle does not take them with it — so a machine that looked cleaned still carried rows pointing at deleted files. The matcher now covers any `SmoothFlow*.app` path and excludes the official install by **prefix**, so `/Applications/SmoothFlow.app`'s own nested Updater is deliberately left alone (unregistering it would break Sparkle on a healthy machine). `install-release.test.sh` pins both directions, including a guard against the test's copy of the regex drifting from the real one. th-9c3f4e.
