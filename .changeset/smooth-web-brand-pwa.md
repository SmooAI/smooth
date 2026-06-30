---
'smooai-smooth-web': minor
---

Brand the smooth-web PWA with the Smooth `th` icon + force-update on new deploys.
The `th` gradient mark is the favicon + PWA/app icon (regenerated at every size, on
the brand near-black for installed icons) and appears as a quiet top-left product
mark in the UI. The PWA now uses `registerType: 'prompt'` and polls for updates
while open; when a new version ships it shows a non-dismissable "A fresh Big Smooth
is ready" modal whose only action is a forced refresh — so an installed/long-open
tab never drifts onto stale code. Manifest renamed to "Big Smooth".
