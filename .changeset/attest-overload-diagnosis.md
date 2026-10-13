---
'@smooai/smooth': patch
---

`th attest` now says why a check could not run on a busy machine and retries it once the load falls. A failure distrusted for load (above 2× cores), or a check that waited out the ci-queue, prints the repo's `scripts/ci/_pressure.sh` diagnosis when there is one (load, disk, top processes, long-running local stack, one remedy line). A distrusted failure is re-run once when the load drops below the threshold, waiting up to `SMOOAI_CI_OVERLOAD_RETRY_WAIT` seconds (default 600), and that re-run is a real verdict. The smooth-agent push hint now leads with the local remedy and `th attest <named rows>`, and calls `ci:full` the last resort (SMOODEV-3883).
