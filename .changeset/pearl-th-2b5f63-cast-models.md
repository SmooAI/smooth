---
'@smooai/smooth': patch
---

Pearl th-2b5f63: add `th cast models` — list live model groups
exposed by the configured LiteLLM provider (e.g. llm.smoo.ai) via
`GET /v1/models`.

Useful for confirming deploys, debugging routing, and copying
alias names. The default provider is the one backing the `default`
routing slot (what `th routing show` highlights); pass
`--provider NAME` to override on multi-provider setups.

Flags:

- `--provider NAME` — override the provider id (default: the
  provider backing the `default` routing slot).
- `--filter PATTERN` — case-insensitive substring filter on
  model ids.
- `--json` — emit `{"data":[{"id":"..."}]}` for scripting.

The parser is tolerant of LiteLLM responses with embedded ASCII
control bytes (we strip 0x00-0x1F before strict JSON parsing) and
of truncated responses (a byte-scan fallback recovers any
complete `"id":"NAME"` entries). When the strict and lossy
counts disagree, the footer surfaces a `!` warning so deploys
returning partial bodies don't fail silently.

Exits 2 if no provider is configured (`run th auth login`), and
prints the status code + first 200 chars of the body when the
provider responds non-200.
