# How to Update These Docs

#meta

> [!info] Single source of truth
> This vault is the authoritative Smooth architecture documentation. The README's architecture section is a marketing surface; the white paper is the security pitch. This vault is the truth.

## Workflow

1. Open the vault in Obsidian: `obsidian://open?path=<repo>/docs`. Or just edit Markdown in your usual editor — wikilinks, tags, and frontmatter render fine in any tool.
2. Edit the relevant page. Bias toward short bullets, ASCII diagrams, and `> [!arch]` / `> [!info]` / `> [!warn]` / `> [!todo]` callouts.
3. Cross-link liberally with `[[Page-Name]]`. Anchors for cast roles use `[[The-Cast#Wonk]]` form so they land at the H2.
4. Commit on a feature branch with a changeset if the docs change implies a behavior change — purely doc-only PRs don't need a changeset.

## Style

- **One tagline + one callout per page.** Open with what the page is and what callout level it deserves.
- **ASCII over Mermaid.** Renders identically in Obsidian, GitHub, and editor preview.
- **No fluff.** Bullets, tables, definitions. Narrative only when explaining a flow.
- **Wikilinks for everything internal.** `[[../Architecture/The-Cast]]` works; relative paths render in both Obsidian and GitHub.
- **Tags via frontmatter or inline.** `#moc` for hubs, `#architecture`, `#cast`, `#operations`, `#engineering`, `#decision`, `#start-here`, `#meta`.

## Templates

- [[../_templates/Cast-Role-Template]] — for new cast members.
- [[../_templates/ADR-Template]] — for architecture decisions.

## When to add an ADR

Whenever a decision:

- Affects the system architecture or a major subsystem
- Is difficult or expensive to reverse
- Was debated and a clear choice was made
- Adopts or replaces a significant technology

See [[../Decisions/ADR-Index]] for the format.

## When code disagrees with the docs

Open an issue, then either fix the docs or fix the code — whichever represents the desired state. The doc is the spec; the code is the implementation. If they disagree, one of them is a bug.

If you find ambiguity (the code does X but the docs say Y and both seem reasonable), drop a `> [!todo]` callout naming the unknown and link the relevant source file. Don't speculate; the user / reviewer will resolve.

## Related

- [[../Home]]
- [[../Architecture/Architecture-Overview]]
