---
'@smooai/smooth': patch
---

Big Smooth: sidekick tool calls now run the permission gate (with the DenyPolicy) and Narc, in the same order as a top-level turn. The engine gives each sidekick a fresh tool registry with no host hooks, so before this a sidekick's `bash` and file calls skipped both checks. That mattered more once the kernel sandbox became opt-in. A sidekick call that needs approval asks the user over the parent turn, and is denied if there is no way to ask.
