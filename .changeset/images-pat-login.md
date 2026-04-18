---
"@smooai/smooth": patch
---

GHCR images job: log in with `GH_PAT` instead of the default
`GITHUB_TOKEN`. The initial image pushes were done from a local
docker login, so the packages are tied to the user account rather
than the workflow — GITHUB_TOKEN hits `denied: permission_denied:
write_package` on subsequent CI pushes. `GH_PAT` has write:packages
on the SmooAI org so the workflow can keep updating the existing
packages.
