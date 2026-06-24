---
'@smooai/smooth': patch
---

Add `pnpm install:th:brew` — installs the latest released `th` via Homebrew (`brew upgrade SmooAI/tools/th || brew install SmooAI/tools/th`) for anyone who just wants the published binary without a full source build. `install:th` and `install:th:full` are unchanged and still build from local source (the dev test loop). (pearl th-2bd1c8)
