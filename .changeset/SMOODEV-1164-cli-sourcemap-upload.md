---
'@smooai/smooth': minor
---

SMOODEV-1164: `th observability sourcemaps upload <dir>` — bulk source map upload.

New CLI surface for the Error Tracking dashboard's symbolication path.
Walks a build directory (`.next/`, `dist/`, `.open-next/`, etc.), finds
every `.js{,mjs,cjs}` paired with a `.map`, registers each map against
a (release, environment) pair via the Smoo Observability API, then
PUTs the bytes to the presigned S3 URL the API returns.

Companion `th observability sourcemaps list` prints currently
registered maps for a release.

Backend half ships as SMOODEV-1164 in the smooai monorepo.
