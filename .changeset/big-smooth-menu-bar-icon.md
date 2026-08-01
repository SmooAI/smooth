---
'@smooai/smooth': patch
---

Big Smooth menu bar: use the `th` mark icon instead of text

The menu-bar item now shows the `th` glyph as a template image (black shape + alpha, so macOS tints it for the light/dark menu bar) instead of the "Big Smooth" text. Rendered from `images/smooth-icon.svg` to a 36px PNG and embedded in `smooth-menubar` at build time; falls back to the text title if the image can't be decoded.
