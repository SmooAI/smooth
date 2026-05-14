---
'@smooai/smooth': patch
---

tests: add `sandbox_security.rs` integration suite exercising the
Decision::Ask → AccessStore → human resolution → BoardroomNarc replay
chain end-to-end. Covers: unknown domain holds-for-approve and
holds-for-deny, dangerous CLI patterns refused by the rule engine
before the Ask path runs, dangerous domains likewise, persistent
wonk-allow.toml grants short-circuiting without prompts, glob
matching against subdomains (and the adjacent-label safety guard),
rule-engine safe domains, decision cache dedup, hold timeout failing
closed, concurrent pending requests resolving independently, runtime
merge_in taking effect without a Narc restart, glob_override flowing
back through the resolution. 12 tests, in-process — the real-microVM
gold standard from th-9dcc40's description is still on deck but
needs a separate fixture investment. Pearl th-9dcc40.
