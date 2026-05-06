---
"@smooai/smooth": patch
---

TUI banner: structural split into (smoo, th) chunks — no more boundary math

Previous boundary-as-fraction approach (`smoo_end = total * 17/25`)
was fragile because the ANSI-Shadow letter widths drift between
rows — some rows would land the boundary inside a glyph, leaving
teal artifacts on the 2nd O's right edge.

Refactor:

- `BANNER_ROWS: [(&str, &str); 6]` — each row is now an explicit
  `(smoo_chunk, th_chunk)` tuple, split at the actual letter
  boundary in source.
- `theme::smoo_gradient_color(i, total)` — the orange→coral→pink
  3-stop gradient, applied across only the smoo chunk's own length.
- `theme::th_gradient_color(i, total)` — the teal→blue 2-stop
  gradient, applied across only the th chunk's own length.
- `theme::smooth_banner_color` removed (was the fraction-based
  helper).

Each half's gradient now fills exactly its half — `Smoo` is solid
orange→coral→pink, `th` is solid teal→blue, and the boundary lands
where it's supposed to, on T's left edge, not partway through it.
