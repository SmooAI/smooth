---
'@smooai/smooth': patch
---

Glow up the `smoo auth login` browser callback page in the Presence design
language (th-40745f). The success and error tabs were unstyled default HTML;
they now render on Smooth's warm near-black ground with the same dual teal/blue
radial glow as the web SPA, the teal→blue `th` gradient as the single focal mark
(Big Smooth's face, used once — nowhere near chrome), an online-green
"connected" dot on success, and a calm dimmed-face + warm-red variant on error
(deliberately not amber — amber means only "Big Smooth needs you"). Self-contained
inline CSS with a `prefers-reduced-motion` guard; wording and the auto-close
script are unchanged.
