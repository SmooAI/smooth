---
'@smooai/smooth': patch
---

Local-model ergonomics: bring-your-own OpenAI-compatible providers. New `th providers add/list/remove/detect` manages local inference servers (Ollama, LM Studio, llama.cpp, …) in `~/.smooth/providers.json` — `detect` probes the common local ports (Ollama :11434, LM Studio :1234) via `GET /v1/models` and, with `--yes`, adds what answers. Writes go through a field-preserving raw-JSON path so per-provider `max_tokens` and any unknown keys survive (the typed registry serializer drops them). Local providers' live models are folded into `th cast models` and the `th code` `/model` picker (both tolerate the server being down). A per-provider `--max-tokens` cap is plumbed through Big Smooth (`SMOOTH_MAX_TOKENS`) into the operative so small local-model context windows aren't blown by the hardcoded 32768 default.
