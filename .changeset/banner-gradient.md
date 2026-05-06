---
"@smooai/smooth": patch
---

TUI welcome banner: paint the SMOOTH wordmark with the brand gradient (Smoo orange→pink + th teal→blue)

The welcome banner used `theme::gradient_row(i, total_rows)` which
paints each pixel-row uniformly top-to-bottom (yellow→green). Doesn't
match the brand pattern — `Smoo` is orange→pink, `th` is teal→blue
(see `theme::smooth_wordmark()`).

New `theme::smooth_banner_color(col, total)` returns the right color
for column `col` of a `total`-wide rendering, with the 6-letter
split mapped to a 2/3 column split (4 of 6 letters in the Smoo zone).
The banner now styles each character independently, so the brand
gradient runs HORIZONTALLY across the wordmark the way it reads
everywhere else in the product.
