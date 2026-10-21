# Big Smooth Onboarding

#architecture

> [!info] SMOODEV-3918
> A new Big Smooth user is guided to a working model on first run: the **Family plan** ($9/month, 14-day free trial, hosted models through the Smoo AI gateway) or **their own key** (any OpenAI-compatible or Anthropic endpoint). Server side of the Family plan: smooai SMOODEV-3916.

## The two doors

| Door                      | What happens                                                                                                                                                                                                                                                                                                          |
| ------------------------- | --------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| Family plan (recommended) | Sign in with Smoo, then mint this machine's gateway key, `big-smooth-<host>`. With no plan the mint returns 402 and a checkout URL. We open it, retry the mint (every 5 s for the first minute, then every 15 s, for about 10 minutes) until the trial is active, then save the key as the `smooai-gateway` provider. |
| Your own key              | An OpenAI-compatible or Anthropic endpoint (base URL, key, model), or any catalog provider through `th model login`. One small test request runs before anything is saved, then every routing slot points at the new provider.                                                                                        |

## Where it runs

- **`th up`.** When no model is configured, the picker runs before the daemon starts, because the daemon reads its gateway once at boot. This needs an interactive terminal. Without one, `th up` prints the two commands and starts anyway. `SMOOTH_SKIP_ONBOARD=1` opts out.
- **`th model setup`.** The same picker, on demand.
- **`th doctor --onboard`.** Its Providers step runs the picker instead of printing a command. `th doctor` now uses the daemon's own rule to decide whether a model is configured: an existing `providers.json` is not enough.
- **`th harness add --agentic`.** Its provider gate uses the same picker.
- **Web SPA.** A first-run card is shown when `GET /api/llm/provider` reports `configured: false`, and can be dismissed. It offers the same two doors:
    - The Family plan door reuses `useSmooAuth` (from `SmooSignIn.tsx`) for the device-code sign-in. It then calls `POST /api/llm/provider/family`. A `plan_required` answer shows a "Start the 14-day free trial" button that opens the checkout, and the card polls until the answer is `saved`.
    - The own-key form posts to `POST /api/llm/provider`.
- **In chat.** The `configure_provider` tool lets Big Smooth add or switch an OpenAI-compatible or Anthropic endpoint conversationally once some model works. It is on the daemon's confirmation floor (`CONFIRM_TOOLS`), so the user approves every call, and Plan mode drops it.

The persona has one rule about the plan. On a user's own key, when they ask for something the Family plan covers (hosted models, cross-device sync), Big Smooth mentions the plan once per conversation at most and never nags.

## Code

| Piece                                                      | Where                                                                                         |
| ---------------------------------------------------------- | --------------------------------------------------------------------------------------------- |
| Own-key validation, probe, field-preserving save           | `crates/smooth-cast/src/provider_setup.rs`                                                    |
| Key mint, 402 handling, rotate-on-409, save gateway key    | `crates/smooth-cast/src/family_plan.rs`                                                       |
| "Is a model configured" (shared by `th up` and the daemon) | `smooth_cast::providers::route_at` (the daemon's `gateway_from_providers_at` delegates to it) |
| CLI picker, checkout polling, `up_gate`                    | `crates/smooth-cli/src/onboard_provider.rs`                                                   |
| Chat tool                                                  | `crates/smooth-daemon/src/configure_provider.rs`                                              |
| HTTP routes                                                | `crates/smooth-daemon/src/llm_provider.rs`                                                    |
| Web card                                                   | `crates/smooth-web/web/src/FirstRunCard.tsx`, `provider-setup.ts`                             |

### Routes

| Route                           | Auth        | Body / answer                                                                                                                                                       |
| ------------------------------- | ----------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `GET /api/llm/provider`         | ungated     | `{configured, source, model, gateway_host, restart_required, options}`. Never includes a key.                                                                       |
| `POST /api/llm/provider`        | local token | `{format, base_url, api_key, model, id?}`. Returns 200 with `status: configured`, or 422 with `invalid`, `probe_failed` or `write_failed`.                          |
| `POST /api/llm/provider/family` | local token | Returns `{status: saved \| plan_required \| signed_out \| error}`. `plan_required` also carries `upgradeUrl` and `message`. The key never goes back to the browser. |

### Server contract

`POST /organizations/{org}/llm-gateway/keys` with `{"name": "big-smooth-<host>"}`:

- **201** `{key}`: the key is shown once.
- **409**: the name already exists, so we rotate it with `POST …/keys/{name}/rotate`. The key belongs to this machine and nothing else uses it.
- **402** `{error: "family_plan_required", message, upgradeUrl}`: the Family plan is needed. A body we can't parse, or a non-https `upgradeUrl`, falls back to `https://smoo.ai/apps/start/product?plan=Family%20plan&term=monthly`.

Both create and rotate are gated (`big-smooth` or `big-smooth-*`, case-insensitive). The plan check runs before the 409 and 404 checks. A role that excludes Big Smooth gets 403, not 402. Until SMOODEV-3916 ships the 402, the mint simply succeeds.

A brand-new user has no org. Starting the checkout creates a personal one, so the org is re-resolved on every mint attempt. A signed-in user with no org is sent straight to the checkout.

**Top-ups (SMOODEV-3969).** The trial's AI allowance is small on purpose, so every Family plan surface also names prepaid AI credits: `family_plan::top_up_note()` ("Top up $5 or $10 anytime during the trial; auto-reload optional — `smoo credits buy`"), built from `smooth_cast::ai_credits::PACKS_CENTS`. The picker prints it under the checkout link and after the key is saved. The daemon's `options()` description carries it, and `POST /api/llm/provider/family` returns it as `topUp` (on `plan_required` and `saved`), which the first-run card shows under the trial button. When the allowance does run out, the gateway's `Budget has been exceeded` is shown as the top-up link (see `smoo credits` in [[Using-th-CLI]]).

## Decision: no hot reload (yet)

A provider change takes effect on the **next daemon start**. Every surface says so: the CLI prints `th down && th up`, `GET /api/llm/provider` reports `restart_required`, and both the tool and the web card say a restart is needed.

Why hot reload isn't possible yet:

- `LocalServer::builder().config(..)` moves the `ServerConfig` into an `Arc` that is fixed for the server's lifetime. Each turn builds its LLM config from `state.config.gateway_url` and `state.config.model`.
- The engine does have a per-turn `GatewayKeyResolver` seam, but the `LocalServer` builder doesn't expose it, and the URL and model would still be fixed even with it.

Rebuilding the server in-process would drop live WebSocket sessions and parked confirmations.

What `restart_required` covers:

- The daemon booted with no key and a key now exists. This was already reported.
- The configured host differs from the one the daemon booted with. This is new: a BYO switch from `llm.smoo.ai` to `api.openai.com` is flagged.

To make it live later, add a `.gateway_key_resolver()` and a reloadable URL/model to the `LocalServer` builder upstream (smooth-operator). Then `POST /api/llm/provider` can swap without a restart.

## Limits

- **Anthropic format in chat.** Big Smooth's chat engine (`smooth-operator-server`) only speaks OpenAI-compatible chat completions. Anthropic's own API also serves that shape at `https://api.anthropic.com/v1`, so it works. Any other Anthropic-format endpoint is saved, and works for `th code`, but every surface warns that chat can't run on it.
- **Local servers.** Keyless local servers (Ollama, LM Studio) are saved with the placeholder key `local`, because the daemon treats a provider without a key as unconfigured.
- **App-owned daemon.** When Big Smooth.app owns the daemon, `th up` still runs the picker. The app's daemon picks up the change the next time it starts.
