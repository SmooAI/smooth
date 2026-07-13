---
"@smooai/smooth": minor
---

Add `th crm` image commands (SMOODEV-2605), reaching parity with the web/mobile
CRM-image support from SMOODEV-2589:

- `th crm contacts set-image <id> <path>` / `remove-image <id>` — upload/clear a contact avatar
- `th crm companies set-image <id> <path>` / `resolve-logo <id>` / `remove-image <id>` — upload, server-side favicon resolve, or clear a company logo
- `th crm deals set-image <id> <path>` / `remove-image <id>` — upload/clear a deal image (remove is a no-op when the shown image is inherited from the linked company/contact)

Uploads mirror `th files upload`: presign via `/media/upload-url`, PUT bytes to
S3 with a bearer-less client, then link the durable `mediaUrl` under
`/crm/images`. Removal resolves the image UUID from the entity read
(`avatarImageId` / `logoImageId` / `imageId`).
