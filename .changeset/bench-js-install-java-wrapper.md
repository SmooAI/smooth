---
"smooai-smooth-bench": patch
---

JS bench command now runs `npm install` before `npm test` — tasks ship only a `package.json` with devDependencies (jest/babel), no `node_modules`. Java bench uses the bundled `./gradlew --no-daemon` wrapper so version drift between the task and the sandbox doesn't matter.
