#!/usr/bin/env python3
"""Fetch the gateway's model catalogue, and PROBE what actually works.

The catalogue alone is not enough. Pearl th-c127d1: `gpt-5.5` was in the
list, priced, and advertised `supports_function_calling: true` — and was
completely unusable through Big Smooth, because it 400s on
`temperature: 0` and the failure surfaces as an assistant that silently
says nothing. A "model update" that only refreshes strings would have
happily shipped it into the Settings picker again.

So: list, price, then actually call each model the way the agent does.

Usage:
    probe-models.py                     # catalogue + pricing, no calls (free)
    probe-models.py --probe             # + one real call per model (costs $)
    probe-models.py --probe --only gpt-5.5 --only claude-sonnet-5
    probe-models.py --json out.json     # machine-readable

Credentials: SMOOAI_GATEWAY_KEY, else the OpenAI-compatible provider in
~/.smooth/providers.json (the same store the daemon reads).
"""

from __future__ import annotations

import argparse
import json
import os
import sys
import urllib.error
import urllib.request

DEFAULT_BASE = "https://llm.smoo.ai"
TIMEOUT = 90
# The gateway 403s urllib's default "Python-urllib/3.x" agent. Any real
# UA is accepted; this one just says who is calling.
HEADERS = {"User-Agent": "smooth-update-models/1.0"}


def credentials() -> tuple[str, str]:
    """(base_url, api_key) from the env, else providers.json."""
    key = os.environ.get("SMOOAI_GATEWAY_KEY")
    base = os.environ.get("SMOOAI_GATEWAY_URL", DEFAULT_BASE)
    if key:
        return base.rstrip("/").removesuffix("/v1"), key
    path = os.path.expanduser("~/.smooth/providers.json")
    try:
        with open(path) as fh:
            doc = json.load(fh)
    except OSError:
        sys.exit(f"no SMOOAI_GATEWAY_KEY and no {path} — cannot reach the gateway")
    # Prefer the Smoo gateway explicitly. Matching on "/v1" alone picks up
    # the Anthropic provider, whose base has no /model/info — the endpoints
    # this script needs are LiteLLM's, not any OpenAI-shaped API's.
    providers = [p for p in doc.get("providers", []) if p.get("api_key")]
    for p in providers:
        url = p.get("api_url") or p.get("baseUrl") or ""
        if "llm.smoo.ai" in url:
            return url.rstrip("/").removesuffix("/v1"), p["api_key"]
    for p in providers:
        url = p.get("api_url") or p.get("baseUrl") or ""
        if p.get("api_format") == "OpenAiCompat" and url:
            return url.rstrip("/").removesuffix("/v1"), p["api_key"]
    sys.exit(f"no LiteLLM/OpenAI-compatible provider with an api_key in {path}")


def get(base: str, path: str, key: str) -> dict:
    req = urllib.request.Request(f"{base}{path}", headers={"Authorization": f"Bearer {key}", **HEADERS})
    with urllib.request.urlopen(req, timeout=TIMEOUT) as resp:
        return json.load(resp)


def catalogue(base: str, key: str) -> dict[str, dict]:
    """model name -> {input, output, tier, tools, max_input} in $/M tokens."""
    out: dict[str, dict] = {}
    for m in get(base, "/model/info", key).get("data", []):
        info = m.get("model_info") or {}
        out[m["model_name"]] = {
            "input_per_m": (info.get("input_cost_per_token") or 0.0) * 1e6,
            "output_per_m": (info.get("output_cost_per_token") or 0.0) * 1e6,
            "tier": info.get("model_tier"),
            "tools": info.get("supports_function_calling"),
            "max_input": info.get("max_input_tokens"),
            "use_cases": info.get("use_cases") or [],
        }
    return out


def probe(base: str, key: str, model: str) -> dict:
    """Call `model` the way the agent does, and report what breaks.

    Three things are checked, because all three have bitten us:

    - `temperature: 0` — a growing set of models accept ONLY their
      default and 400 the whole request (th-c127d1). The daemon sends a
      fixed temperature, so a model that rejects it is unusable.
    - tool calling — the agent binds tools on every turn.
    - a non-empty result — a 200 that returns neither content nor a tool
      call is the "Big Smooth says nothing" failure.
    """
    tool = {
        "type": "function",
        "function": {
            "name": "list_files",
            "description": "List files in a directory",
            "parameters": {"type": "object", "properties": {"path": {"type": "string"}}, "required": ["path"]},
        },
    }
    result = {"temp0_ok": None, "tools_ok": None, "replied": None, "error": None}

    for temp, field in ((0, "temp0_ok"), (1, None)):
        body = {
            "model": model,
            "messages": [{"role": "user", "content": "List the files in the current directory."}],
            "tools": [tool],
            "temperature": temp,
        }
        req = urllib.request.Request(
            f"{base}/v1/chat/completions",
            data=json.dumps(body).encode(),
            headers={"Authorization": f"Bearer {key}", "Content-Type": "application/json", **HEADERS},
        )
        try:
            with urllib.request.urlopen(req, timeout=TIMEOUT) as resp:
                doc = json.load(resp)
        except urllib.error.HTTPError as e:
            detail = e.read().decode()[:200]
            if field:
                result[field] = False
                # temperature 0 failing is expected for some models; keep
                # going and judge the model on its default temperature.
                continue
            result["error"] = detail
            return result
        except Exception as e:  # noqa: BLE001 — network shapes vary
            result["error"] = str(e)[:200]
            return result

        if field:
            result[field] = True
        msg = (doc.get("choices") or [{}])[0].get("message") or {}
        result["tools_ok"] = bool(msg.get("tool_calls"))
        result["replied"] = bool(msg.get("tool_calls") or (msg.get("content") or "").strip())
        if field:  # temp 0 worked; no need to re-test at temp 1
            return result
    return result


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--probe", action="store_true", help="make one real call per model (costs money)")
    ap.add_argument("--only", action="append", default=[], help="restrict to these model names (repeatable)")
    ap.add_argument("--json", dest="json_out", help="write the full result here")
    args = ap.parse_args()

    base, key = credentials()
    models = catalogue(base, key)
    names = [n for n in sorted(models) if not args.only or n in args.only]
    if args.only:
        missing = set(args.only) - set(models)
        for m in sorted(missing):
            print(f"!! {m} is NOT in the gateway catalogue", file=sys.stderr)

    if args.probe:
        for n in names:
            models[n]["probe"] = probe(base, key, n)

    print(f"{'model':<28} {'in $/M':>8} {'out $/M':>8}  {'tier':<10} {'tools':<6}" + ("  status" if args.probe else ""))
    for n in names:
        m = models[n]
        line = f"{n:<28} {m['input_per_m']:>8.2f} {m['output_per_m']:>8.2f}  {str(m['tier'] or '-'):<10} {str(m['tools']):<6}"
        if args.probe:
            p = m["probe"]
            if p["error"]:
                status = f"BROKEN ({p['error'][:60]})"
            elif not p["replied"]:
                status = "EMPTY REPLY"
            elif p["temp0_ok"] is False:
                status = "ok (rejects temperature 0)"
            else:
                status = "ok"
            line += f"  {status}"
        print(line)

    if args.json_out:
        with open(args.json_out, "w") as fh:
            json.dump(models, fh, indent=2, sort_keys=True)
        print(f"\nwrote {args.json_out}", file=sys.stderr)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
