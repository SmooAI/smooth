---
'@smooai/smooth': patch
---

Big Smooth web/desktop SPA: replace the fixed Smooth Mode preset chips (Flash/Code/UI/Plan/Fast/Code+/Max/Smoo-Jr) with a searchable model selector. Every model is derived from the published `docs/model-scores.json` benchmark and shows capability badges (🏆 top score, 💚 best value, 🛡️ safest, 💎 premium), its pass rate, and $/pass. Default is gpt-5.6-luna; the pick persists to localStorage and falls back to luna for a saved unknown id. Null-cost models render "unknown" (never $0), claude-fable-5's rate uses its provided conclusive-denominator value, and the benchmark run date is surfaced in the picker.
