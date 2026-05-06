---
"@smooai/smooth": patch
---

TUI banner: match the actual SVG-defined brand gradient (3 stops + leading solid bands)

The previous pearl swapped vertical for horizontal coloring but used
a 2-stop linear gradient (orange → pink, teal → blue). The brand
gradient in `crates/smooth-web/web/public/logo.svg` is richer:

- **Smoo zone**: 30 % solid orange (`#f49f0a`), then orange → coral
  (`#fb7a4d`) up to 79 %, then coral → pink (`#ff6b6c`) to 100 %
- **th zone**: 43 % solid teal (`#00a6a6`), then teal → blue
  (`#1238dd`) to 100 %

`theme::smooth_banner_color` now mirrors those stops. Leading solid
bands give the wordmark its hold-then-fade shape — without them the
banner read as a flat rainbow.
