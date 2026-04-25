---
"smooai-smooth-web": minor
---

Make smooth-web a PWA. Adds `vite-plugin-pwa` with auto-update SW, generated `manifest.webmanifest`, and the new `th` icon as both favicon (16/32 multi-res ICO + PNG variants) and PWA icon set (192/512 + maskable). Adds iOS apple-touch-icon variants (180/167/152/120) and meta tags for Add-to-Home-Screen. The axum static handler now serves `.webmanifest` with the spec'd `application/manifest+json` MIME (mime_guess doesn't know about it).
