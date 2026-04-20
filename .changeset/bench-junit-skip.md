---
"smooai-smooth-bench": patch
---

Java skip-strip. Polyglot Java tasks ship with `@Disabled` / `@Ignore` on 30-of-31 tests (same pattern as Rust `#[ignore]` and JS `xtest`/`test.skip`). Without the strip, a Java bowling run scored 1/32. Harness now rewrites `@Disabled` / `@Disabled("reason")` / `@Ignore` / `@Ignore("reason")` annotations out of test files in the scratch work dir (only test files, not production code — avoids clobbering unrelated annotations like `@DisabledInNativeImage`).
