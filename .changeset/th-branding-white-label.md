---
'@smooai/smooth': minor
---

`th branding` — white-label a Smoo AI org, logo included, from the CLI (SMOODEV-2820)

New top-level command (alias `th brand`): `show` / `from-url` / `set` / `enable` /
`disable` / `preview` / `clear`. It wraps the org's white-label row and, unlike
pasting a URL into the dashboard, actually re-hosts the logo — `--logo`,
`--logo-dark` and `--favicon` each take a local path or a remote URL, and a
remote one is fetched and uploaded to the org's brand assets so a partner's own
server is never left as the source of truth for their mark.

Three things it refuses to do, on purpose:

- **Go live on an unreadable theme.** `enable` (and `from-url --enable`) computes
  WCAG contrast for foreground/background, primaryForeground/primary and
  mutedForeground/background and stops at anything under 4.5:1. `--force`
  overrides. Shipping an illegible dashboard to a partner is the failure mode
  the whole gate exists for.
- **Fetch a private host.** Remote logo URLs are vetted the way the server's
  `vetUrl` does — http(s) only, no loopback / RFC1918 / `169.254.` (the cloud
  metadata endpoint), no redirect following, 5 MB cap — and the bytes are
  magic-byte sniffed against the platform's allowlist before upload.
- **Silently wipe a theme.** The server's PUT replaces the whole `themeJson`
  column, so every partial `set` is a read-modify-write over the current row.

`from-url` is a dry run by default: it prints the derived swatch table, the logo
candidates and the contrast verdict, and writes nothing. `--apply` stages
(`enabled` stays false, previewable via `?brandPreview=1`); `--enable` goes live.

The Aurora meaning tokens (`--color-heat-0..5`, `--color-ai`, `--gradient-aurora`,
ok/warn/crit) are never white-labeled and the command exposes no flags for them.

Two server-side gaps are surfaced as diagnoses rather than bare errors: the
platform's write validator is still Phase 1, so the surface tokens
(`--background`, `--card`, `--sidebar`, …) 400 today; and `from-url` 404s until
the propose endpoint deploys.
