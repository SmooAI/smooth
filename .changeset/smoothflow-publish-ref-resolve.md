---
"@smooai/smooth": patch
---

smoothflow-publish.yml resolves its `tag` input to a full SHA before checkout, so a short commit id works and an unknown ref fails in seconds instead of after the 20-minute build.
