---
"@smooai/smooth": patch
---

Make `smooth-web` actually usable on phones. Chat now stacks vertically on mobile (single-pane: Chats list when no active chat, Conversation when one is selected, with a back button). The Send button collapses to icon-only under `sm:`. Pearls page now renders an inline project picker (cards, with open/in-progress/closed counts) instead of just printing "Select a project to view pearls" — the existing picker lived in the sidebar drawer which is hidden by default on mobile, so users couldn't find it. Layout `<main>` padding drops from `p-6` to `p-4` on mobile to reclaim ~16px on each side, and chat heights use `100dvh` instead of `100vh` so iOS browser chrome doesn't eat the input row. Inputs all set explicit `font-size: 16px` to prevent iOS Safari's tap-to-zoom behavior.
