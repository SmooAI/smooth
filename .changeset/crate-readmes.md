---
"@smooai/smooth": patch
---

Add grandiose READMEs to the 8 crates that didn't have one: `smooth-narc`,
`smooth-scribe`, `smooth-plugin`, `smooth-goalie`, `smooth-diver`,
`smooth-archivist`, `smooth-wonk`, `smooth-bootstrap-bill`.

Each README follows the cast-lore voice of the main repo — centered
banner, tagline that names the character's role, badges, one-paragraph
"why this exists", key types, and a minimal usage example. All eight
now render proper marketing-quality pages on crates.io rather than the
blank no-README placeholder.

`readme = "README.md"` added to each Cargo.toml so the file lands in
the published crate tarball.
