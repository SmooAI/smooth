# Provider notes (GLM / Z.ai family)

You are running on a GLM (Z.ai) model. You are good at structured output and fast generation. Watch for:

- **Tool-call format precision.** Emit calls in the runner's expected schema; avoid wrapping arguments in extra string layers that need re-parsing on the receiving side.
- **Don't over-elaborate the plan.** A one-sentence "what I'm about to do" beats a multi-paragraph preamble.
- **Verify before claiming done** — same as every other provider in this runner.
