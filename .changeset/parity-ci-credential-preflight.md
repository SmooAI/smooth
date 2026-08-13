---
'@smooai/smooth': patch
---

Engine Parity CI: fail on the real cause instead of five false regressions.

The nightly gate had been red for days reporting "5 engine(s) below baseline". The boards say
every engine scored 0% with zero tool calls, and the logs say why: the Go server reports
`no chat engine configured` and the .NET server gets `HTTP 401` from the gateway.
`SMOOAI_GATEWAY_API_KEY` does not exist as a repo secret or an org secret, so both bench
workflows have been running with an empty key. Five implementations in five languages do not
regress on the same night — one shared input did.

- Both bench workflows now preflight the key (present, and accepted by `llm.smoo.ai`) and fail
  in seconds with an unambiguous message rather than after an hour of meaningless scoring.
- `check-engine-parity.sh` calls an all-zero board what it is — a harness or credential fault —
  and names the key to check first.
- `bench-engines.yml` builds `th` and puts it on PATH. The rust engine's LocalServer is spawned
  as bare `th daemon`, which nothing in that job built, so its log was empty and its 0% board
  read exactly like a broken engine.
- `bench-models.yml` defaulted to `deepseek-v4-flash gpt-5.5`, both retired in #420; it now
  scores the routed lineup.

The gate still cannot go green until the secret exists — but it now says so.
