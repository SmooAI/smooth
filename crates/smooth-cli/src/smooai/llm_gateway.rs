//! `th llm …` — org LLM gateway keys (the `api.smoo.ai`
//! `/organizations/{org_id}/llm-gateway/*` surface).
//!
//! Mint and rotate an org's persistent `llm.smoo.ai` key, manage
//! additional named keys, and inspect spend. These routes authenticate
//! as the **user** (Supabase JWT) and are org-admin-gated — they 401
//! under an M2M token, so this surface uses [`UserClient`], not the
//! M2M-capable `SmoothApiClient`. A master/super admin can target a
//! child org with `--org-id <child>` (the user JWT acts cross-org).
//!
//! Keys are LiteLLM virtual keys scoped to the org's team/budget; the
//! key VALUE is returned exactly once at mint/rotate time.

use anstream::println;
use anyhow::{Context, Result};
use clap::Subcommand;
use owo_colors::OwoColorize;
use serde_json::{json, Value};

use super::print_json;
use crate::smooai::user_client::UserClient;

#[derive(Subcommand)]
pub enum Cmd {
    /// Show the org's LLM gateway status — the masked key (with
    /// create/rotate timestamps) and month-to-date spend. Run this
    /// before `create-key` to see whether the org already has one.
    Overview {
        /// Override the active org. Falls back to `SMOOAI_ORG_ID` then
        /// the credentials file's `active_org_id`. A master admin can
        /// target a child org here.
        #[arg(long, visible_alias = "org")]
        org_id: Option<String>,
        /// Emit the raw JSON response instead of the pretty summary.
        #[arg(long)]
        json: bool,
    },
    /// Show LLM spend broken down by model and by day over a window
    /// (default 30 days, clamped to 1–90).
    Usage {
        /// Override the active org (see `overview` for the fallback chain).
        #[arg(long, visible_alias = "org")]
        org_id: Option<String>,
        /// Window length in days (clamped to 1–90).
        #[arg(long, default_value_t = 30)]
        days: u32,
        /// Emit raw JSON (default — the breakdown is a timeseries).
        #[arg(long)]
        json: bool,
    },
    /// Mint the org's persistent ("default") LLM gateway key. Provisions
    /// a LiteLLM team + virtual key scoped to the org's budget and
    /// prints the key VALUE exactly once — store it immediately. Fails
    /// with 409 if the org already has a key (use `rotate-key`).
    CreateKey {
        /// Override the active org (see `overview` for the fallback chain).
        #[arg(long, visible_alias = "org")]
        org_id: Option<String>,
        /// Emit the raw JSON response (still contains the key once).
        #[arg(long)]
        json: bool,
    },
    /// Rotate the org's persistent key — the old key is invalidated and
    /// a new value is printed once. Use after a suspected leak.
    RotateKey {
        /// Override the active org (see `overview` for the fallback chain).
        #[arg(long, visible_alias = "org")]
        org_id: Option<String>,
        /// Emit the raw JSON response (still contains the key once).
        #[arg(long)]
        json: bool,
    },
    /// One step: mint a named gateway key and write it straight into
    /// `~/.smooth/providers.json`, so Big Smooth (`th code`, `th up`,
    /// the daemon) can use it without a human ever seeing the value.
    /// Signs in first if there's no Smoo AI session.
    ///
    /// ISOLATION CAVEAT — read this before assuming a fresh key is a
    /// blast shield. LiteLLM enforces budget at the TEAM level, and the
    /// team IS the org, so every key minted into one org shares one
    /// budget and one fate: exhaust it and every other key on that org
    /// dies with yours. On 2026-08 Big Smooth spent 95% of the master
    /// org's lifetime budget and took smoo.ai's public chat agent down
    /// with it — a separate key would NOT have prevented that.
    ///
    /// A new key gives ATTRIBUTION (per-key spend in
    /// `LiteLLM_SpendLogs`). Only a DIFFERENT ORG gives ISOLATION —
    /// pass `--org-id <other-org>` if this workload must not be able to
    /// starve the rest.
    Provision {
        /// Key name, unique per org. Defaults to `big-smooth-<hostname>`
        /// so spend is attributable to the machine that spent it.
        #[arg(long)]
        name: Option<String>,
        /// Override the active org (see `overview` for the fallback
        /// chain). This is the ONLY isolation boundary — see the
        /// command description.
        #[arg(long, visible_alias = "org")]
        org_id: Option<String>,
        #[command(flatten)]
        flags: ProvisionFlags,
    },
    /// Manage additional named keys beyond the default — e.g. one per
    /// service or environment.
    #[command(visible_alias = "key")]
    Keys {
        #[command(subcommand)]
        cmd: KeysCmd,
    },
}

#[derive(Subcommand)]
pub enum KeysCmd {
    /// List the org's named keys (masked).
    List {
        /// Override the active org (see `llm overview` for the fallback chain).
        #[arg(long, visible_alias = "org")]
        org_id: Option<String>,
        /// Emit the raw JSON response instead of the pretty list.
        #[arg(long)]
        json: bool,
    },
    /// Create a new named key — prints the value exactly once.
    Create {
        /// Key name, unique per org (lowercase letters, digits, dashes).
        name: String,
        /// Override the active org (see `llm overview` for the fallback chain).
        #[arg(long, visible_alias = "org")]
        org_id: Option<String>,
        /// Emit the raw JSON response (still contains the key once).
        #[arg(long)]
        json: bool,
    },
    /// Rotate a named key — invalidates the old value, prints the new one once.
    Rotate {
        /// Name of the key to rotate.
        name: String,
        /// Override the active org (see `llm overview` for the fallback chain).
        #[arg(long, visible_alias = "org")]
        org_id: Option<String>,
        /// Emit the raw JSON response (still contains the key once).
        #[arg(long)]
        json: bool,
    },
    /// Revoke (soft-delete) a named key. It stops working at the
    /// provider immediately; the name can be re-minted later.
    Delete {
        /// Name of the key to revoke.
        name: String,
        /// Override the active org (see `llm overview` for the fallback chain).
        #[arg(long, visible_alias = "org")]
        org_id: Option<String>,
        /// Emit the raw JSON response instead of the confirmation line.
        #[arg(long)]
        json: bool,
        /// Print the target and exit without deleting.
        #[arg(long)]
        dry_run: bool,
        /// Skip the interactive confirmation. Required in scripts/CI.
        #[arg(long)]
        yes: bool,
    },
}

pub async fn cmd(cmd: Cmd) -> Result<()> {
    // Provision is the one subcommand that may have to sign in first, so
    // it builds its own client rather than inheriting the strict one.
    if let Cmd::Provision { name, org_id, flags } = cmd {
        return provision(name, org_id, flags).await;
    }
    let client = UserClient::from_user_session().await?;
    match cmd {
        Cmd::Overview { org_id, json } => {
            let org = crate::active_org::resolve(org_id)?;
            let resp = client
                .get(&format!("/organizations/{org}/llm-gateway/overview"))
                .await
                .context("GET llm-gateway overview")?;
            if json {
                print_json(&resp);
            } else {
                print_overview(&resp);
            }
        }
        Cmd::Usage { org_id, days, json: _ } => {
            let org = crate::active_org::resolve(org_id)?;
            let days = days.clamp(1, 90);
            let resp = client
                .get(&format!("/organizations/{org}/llm-gateway/usage?days={days}"))
                .await
                .context("GET llm-gateway usage")?;
            // The usage payload is a per-model + per-day timeseries; JSON is
            // the useful form, so it always prints as JSON.
            print_json(&resp);
        }
        Cmd::CreateKey { org_id, json } => {
            let org = crate::active_org::resolve(org_id)?;
            let resp = client
                .post(&format!("/organizations/{org}/llm-gateway/create-key"), &json!({}))
                .await
                .context("POST llm-gateway create-key")?;
            print_minted_key(&resp, json);
        }
        Cmd::RotateKey { org_id, json } => {
            let org = crate::active_org::resolve(org_id)?;
            let resp = client
                .post(&format!("/organizations/{org}/llm-gateway/rotate-key"), &json!({}))
                .await
                .context("POST llm-gateway rotate-key")?;
            print_minted_key(&resp, json);
        }
        Cmd::Keys { cmd } => keys(cmd, &client).await?,
        // Handled above — it owns its own client.
        Cmd::Provision { .. } => unreachable!("provision is dispatched before the client is built"),
    }
    Ok(())
}

async fn keys(cmd: KeysCmd, client: &UserClient) -> Result<()> {
    match cmd {
        KeysCmd::List { org_id, json } => {
            let org = crate::active_org::resolve(org_id)?;
            let resp = client
                .get(&format!("/organizations/{org}/llm-gateway/keys"))
                .await
                .context("GET llm-gateway keys")?;
            if json {
                print_json(&resp);
            } else {
                print_keys(&resp);
            }
        }
        KeysCmd::Create { name, org_id, json } => {
            let org = crate::active_org::resolve(org_id)?;
            let resp = client
                .post(&format!("/organizations/{org}/llm-gateway/keys"), &json!({ "name": name }))
                .await
                .context("POST llm-gateway named key")?;
            print_minted_key(&resp, json);
        }
        KeysCmd::Rotate { name, org_id, json } => {
            let org = crate::active_org::resolve(org_id)?;
            let resp = client
                .post(&format!("/organizations/{org}/llm-gateway/keys/{name}/rotate"), &json!({}))
                .await
                .context("POST llm-gateway named key rotate")?;
            print_minted_key(&resp, json);
        }
        KeysCmd::Delete {
            name,
            org_id,
            json,
            dry_run,
            yes,
        } => {
            let org = crate::active_org::resolve(org_id)?;
            let proceed = crate::destructive::gate(
                &crate::destructive::Target {
                    verb: "revoke",
                    noun: "LLM gateway key",
                    id: &name,
                    org: &org,
                    severity: crate::destructive::Severity::Standard,
                },
                dry_run,
                yes,
            )?;
            if proceed {
                let resp = client
                    .delete(&format!("/organizations/{org}/llm-gateway/keys/{name}"))
                    .await
                    .context("DELETE llm-gateway named key")?;
                if json {
                    print_json(&resp);
                } else {
                    println!();
                    println!("  {} revoked key {}", "✓".green(), name.bold());
                    println!();
                }
            }
        }
    }
    Ok(())
}

/// Print a freshly-minted key result. The key VALUE is returned by the
/// API exactly once, so make it impossible to miss and remind the user
/// to store it now.
fn print_minted_key(resp: &Value, json: bool) {
    if json {
        print_json(resp);
        return;
    }
    let Some(key) = resp.get("key").and_then(Value::as_str) else {
        // Shape we didn't expect — fall back to raw JSON rather than
        // swallow the response.
        print_json(resp);
        return;
    };
    let mask = resp.get("mask").and_then(Value::as_str).unwrap_or("");
    println!();
    println!("  {} LLM gateway key — shown once, store it now:", "✓".green().bold());
    println!();
    println!("    {}", key.bold());
    if !mask.is_empty() {
        println!("    {} {}", "mask".dimmed(), mask.dimmed());
    }
    println!();
    println!(
        "  {} wire it into the gateway provider: {}",
        "→".dimmed(),
        "th model login smooai-gateway".bold()
    );
    println!();
}

/// Pretty-print the `overview` payload (`{ key, spendMtd }`).
fn print_overview(resp: &Value) {
    println!();
    match resp.get("key") {
        Some(Value::Object(k)) => {
            let mask = k.get("mask").and_then(Value::as_str).unwrap_or("?");
            println!("  {} {}", "key".dimmed(), mask);
            if let Some(c) = k.get("createdAt").and_then(Value::as_str) {
                println!("  {} {}", "created".dimmed(), c);
            }
            if let Some(r) = k.get("rotatedAt").and_then(Value::as_str) {
                println!("  {} {}", "rotated".dimmed(), r);
            }
        }
        _ => {
            println!("  {} no key yet — run {} to mint one", "●".dimmed(), "th llm create-key".bold());
        }
    }
    if let Some(s) = resp.get("spendMtd") {
        let spend = s.get("totalSpendUsd").and_then(Value::as_f64).unwrap_or(0.0);
        let tokens = s.get("totalTokens").and_then(Value::as_u64).unwrap_or(0);
        let reqs = s.get("requestCount").and_then(Value::as_u64).unwrap_or(0);
        println!("  {} ${spend:.2} · {tokens} tokens · {reqs} requests (MTD)", "spend".dimmed());
    }
    println!();
}

/// Pretty-print the `keys` list payload (`{ keys: [...] }`).
fn print_keys(resp: &Value) {
    println!();
    match resp.get("keys").and_then(Value::as_array) {
        Some(arr) if !arr.is_empty() => {
            for k in arr {
                let name = k.get("name").and_then(Value::as_str).unwrap_or("?");
                let mask = k.get("mask").and_then(Value::as_str).unwrap_or("");
                println!("  {} {}  {}", "○".dimmed(), name.bold(), mask.dimmed());
            }
        }
        _ => println!("  {} {}", "●".dimmed(), "no named keys".dimmed()),
    }
    println!();
}

// ─── `th llm provision` ──────────────────────────────────────────────────
//
// The whole point of this command is that the minted key value goes from
// the API into `~/.smooth/providers.json` without a human seeing it.
// LiteLLM returns it exactly once; if we can't store it we print it
// loudly, because a lost value costs a rotation.

/// Cheapest concrete model on the gateway — used only for the one-token
/// verification call. Sourced from the canonical slot map rather than a
/// literal so the retired `smooth-*` aliases can't come back here.
fn verify_model() -> &'static str {
    smooth_policy::smooth_alias::SmoothSlot::Fast.concrete_default()
}

/// `~/.smooth/providers.json` — the file `th model login` writes.
fn providers_path() -> Result<std::path::PathBuf> {
    Ok(dirs_next::home_dir().context("cannot determine home directory")?.join(".smooth/providers.json"))
}

/// Default key name: `big-smooth-<hostname>`, so per-key spend in
/// `LiteLLM_SpendLogs` names the machine that spent it.
fn default_key_name() -> String {
    sanitize_key_name(&format!("big-smooth-{}", smooth_pearls::mail_store::short_hostname()))
}

/// Coerce a name into the API's rule
/// (`^[a-zA-Z0-9][a-zA-Z0-9_-]{0,63}$`) — a Mac's hostname is routinely
/// `Brent's MacBook Pro`, which the route rejects with a 400.
fn sanitize_key_name(raw: &str) -> String {
    let mut out: String = raw
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() || c == '-' || c == '_' { c } else { '-' })
        .collect();
    // Must start alphanumeric.
    while out.chars().next().is_some_and(|c| !c.is_ascii_alphanumeric()) {
        out.remove(0);
    }
    out.truncate(64);
    if out.is_empty() {
        "big-smooth".to_string()
    } else {
        out
    }
}

/// Is a key of this name already active on the org? Drives the
/// idempotency check — the API 409s on a duplicate, but checking first
/// makes the skip explicit instead of an error the user has to read.
fn key_exists(list: &Value, name: &str) -> bool {
    list.get("keys")
        .and_then(Value::as_array)
        .is_some_and(|arr| arr.iter().any(|k| k.get("name").and_then(Value::as_str) == Some(name)))
}

/// Copy `providers.json` aside before we rewrite it. Naming matches the
/// `providers.json.bak*` convention `th reclaim` already sweeps up.
/// `Ok(None)` when there was no file to back up.
fn back_up(path: &std::path::Path) -> Result<Option<std::path::PathBuf>> {
    if !path.exists() {
        return Ok(None);
    }
    let stamp = chrono::Local::now().format("%Y%m%d-%H%M%S");
    let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("providers.json");
    let backup = path.with_file_name(format!("{name}.bak-{stamp}"));
    std::fs::copy(path, &backup).with_context(|| format!("backing up {} to {}", path.display(), backup.display()))?;
    Ok(Some(backup))
}

/// Write `key` into the `smooai-gateway` provider, preserving every
/// other provider already configured. With `wire_routing`, the gateway
/// also becomes the default provider and takes over the routing slots —
/// which is what makes Big Smooth actually use it.
///
/// Returns the backup path (if a file was there to back up).
fn write_gateway_key(path: &std::path::Path, key: &str, wire_routing: bool) -> Result<Option<std::path::PathBuf>> {
    use smooth_operator::providers::{Preset, ProviderRegistry};

    let backup = back_up(path)?;
    let mut registry = if path.exists() {
        smooth_cast::provider_migration::load_providers_with_migration(path).unwrap_or_default()
    } else {
        ProviderRegistry::default()
    };

    // The preset carries the canonical slot→model map, but still spells
    // it with the `smooth-*` aliases retired at the gateway
    // (SMOODEV-1793) — migrate before any of it reaches disk.
    let mut preset = ProviderRegistry::from_preset(Preset::SmoaiGateway, key);
    smooth_cast::provider_migration::migrate_in_memory(&mut preset);
    let mut cfg = preset
        .get_provider("smooai-gateway")
        .cloned()
        .context("the smooai-gateway preset registers no gateway provider")?;
    // `migrate_in_memory` rewrites routing slots but not the provider's own
    // `default_model`, which the preset still spells `smooth-default` — pin it
    // to the same concrete model the migrated default slot resolved to.
    cfg.default_model.clone_from(&preset.routing.default.model);
    registry.register_provider(cfg);
    if wire_routing {
        registry.routing = preset.routing;
        registry.set_default_provider("smooai-gateway");
    }
    registry.save_to_file(path)?;
    Ok(backup)
}

/// What to print when the key was minted but could NOT be stored. The
/// value is live and unrecoverable, so it has to hit the terminal —
/// silently swallowing it is the one failure mode worth being loud about.
fn key_lost_message(name: &str, key: &str, err: &anyhow::Error) -> String {
    format!(
        "MINTED BUT NOT STORED — the key `{name}` exists at the gateway and is billable, but writing providers.json failed: {err}\n\
         This value is shown ONCE. Save it now, or revoke the key with `th llm keys delete {name}`:\n\
         {key}"
    )
}

/// One real (tiny) call through the gateway with the key we just wrote.
/// A key in a file is not evidence that it works.
async fn verify_gateway(api_url: &str, key: &str) -> Result<String> {
    let llm = smooth_operator::llm::LlmClient::new(smooth_operator::llm::LlmConfig {
        api_url: api_url.to_string(),
        api_key: key.to_string(),
        model: verify_model().to_string(),
        max_tokens: 16,
        temperature: smooth_policy::llm_params::AGENT_TEMPERATURE,
        retry_policy: smooth_operator::llm::RetryPolicy::default(),
        api_format: smooth_operator::llm::ApiFormat::OpenAiCompat,
    });
    let msg = smooth_operator::conversation::Message::user("Say 'ok' and nothing else.");
    let resp = llm.chat(&[&msg], &[]).await?;
    Ok(resp.content.trim().chars().take(40).collect())
}

/// The switches on `th llm provision`. A flattened clap group rather
/// than four positional bools on `provision(..)` — that is how a
/// `--json` run silently becomes a `--no-verify` one.
#[derive(clap::Args)]
#[allow(clippy::struct_excessive_bools, reason = "one field per CLI switch — they are flags, not state")]
pub struct ProvisionFlags {
    /// Rotate an existing key of this name instead of stopping.
    /// Without this, an existing name is a visible skip — the old value
    /// was shown once and is not recoverable, so a re-run can't write
    /// it anywhere.
    #[arg(long)]
    rotate: bool,
    /// Store the credential only: don't make the gateway the default
    /// provider and don't rewrite the routing slots.
    #[arg(long)]
    credential_only: bool,
    /// Skip the post-write verification call to the gateway.
    #[arg(long)]
    no_verify: bool,
    /// Emit a JSON summary. Never contains the key value.
    #[arg(long)]
    json: bool,
}

async fn provision(name: Option<String>, org_id: Option<String>, flags: ProvisionFlags) -> Result<()> {
    let ProvisionFlags {
        rotate,
        credential_only,
        no_verify,
        json,
    } = flags;
    let client = match UserClient::from_user_session().await {
        Ok(c) => c,
        Err(e) => {
            // No usable session — run the same sign-in `th auth login`
            // does, then retry. This is the "one step" the command is for.
            println!();
            println!("  {} no Smoo AI session ({e}) — signing in", "→".dimmed());
            crate::auth::login::cmd_login_user(None, None, None).await?;
            UserClient::from_user_session().await?
        }
    };
    let org = crate::active_org::resolve(org_id)?;
    let name = name.map_or_else(default_key_name, |n| sanitize_key_name(&n));
    let path = providers_path()?;

    let list = client
        .get(&format!("/organizations/{org}/llm-gateway/keys"))
        .await
        .context("GET llm-gateway keys")?;
    let exists = key_exists(&list, &name);

    if exists && !rotate {
        // Idempotent: never stack a second key behind the same name. The
        // old value was shown once, so there is nothing to write either.
        if json {
            print_json(&json!({ "org": org, "name": name, "action": "skipped", "reason": "key already exists" }));
        } else {
            println!();
            println!("  {} key {} already exists on org {}", "○".yellow().bold(), name.bold(), org.dimmed());
            println!("    {}", "skipped — its value was shown once and cannot be re-read".dimmed());
            println!(
                "    {} to mint a fresh value and write it: {}",
                "→".dimmed(),
                "th llm provision --rotate".bold()
            );
            println!();
        }
        return Ok(());
    }

    let resp = if exists {
        client
            .post(&format!("/organizations/{org}/llm-gateway/keys/{name}/rotate"), &json!({}))
            .await
            .context("POST llm-gateway named key rotate")?
    } else {
        client
            .post(&format!("/organizations/{org}/llm-gateway/keys"), &json!({ "name": name }))
            .await
            .context("POST llm-gateway named key")?
    };
    let key = resp.get("key").and_then(Value::as_str).context("gateway response carried no key value")?;
    let mask = resp.get("mask").and_then(Value::as_str).unwrap_or("");

    let backup = match write_gateway_key(&path, key, !credential_only) {
        Ok(b) => b,
        Err(e) => {
            // Loud on purpose: the key is live and this is its only showing.
            println!();
            println!("  {} {}", "✗".red().bold(), key_lost_message(&name, key, &e).red());
            println!();
            return Err(e);
        }
    };

    let verified = if no_verify {
        None
    } else {
        let api_url = smooth_operator::providers::ProviderConfig::smooai_gateway("").api_url;
        Some(verify_gateway(&api_url, key).await)
    };

    if json {
        print_json(&json!({
            "org": org,
            "name": name,
            "mask": mask,
            "action": if exists { "rotated" } else { "created" },
            "providersFile": path.display().to_string(),
            "backup": backup.as_ref().map(|b| b.display().to_string()),
            "routingWired": !credential_only,
            "verified": verified.as_ref().map(|v| json!({ "ok": v.is_ok(), "detail": v.as_ref().map_or_else(std::string::ToString::to_string, Clone::clone) })),
        }));
        return Ok(());
    }

    print_provision_summary(&Provisioned {
        exists,
        name: &name,
        org: &org,
        mask,
        path: &path,
        backup: backup.as_deref(),
        credential_only,
        verified,
    });
    Ok(())
}

/// Everything the human-readable summary needs. Grouped so the print
/// lives outside `provision`, which is already doing enough.
struct Provisioned<'a> {
    exists: bool,
    name: &'a str,
    org: &'a str,
    mask: &'a str,
    path: &'a std::path::Path,
    backup: Option<&'a std::path::Path>,
    credential_only: bool,
    verified: Option<Result<String>>,
}

fn print_provision_summary(p: &Provisioned) {
    let &Provisioned {
        exists,
        name,
        org,
        mask,
        path,
        backup,
        credential_only,
        ref verified,
    } = p;
    println!();
    println!(
        "  {} {} key {} on org {}",
        "✓".green().bold(),
        if exists { "rotated" } else { "minted" },
        name.bold(),
        org.dimmed()
    );
    if !mask.is_empty() {
        println!("    {} {}", "mask".dimmed(), mask.dimmed());
    }
    println!("    {} {}", "stored".dimmed(), path.display());
    if let Some(b) = backup {
        println!("    {} {}", "backup".dimmed(), b.display().to_string().dimmed());
    }
    if credential_only {
        println!("    {}", "credential only — default provider and routing left alone".dimmed());
    } else {
        println!("    {}", "smooai-gateway is now the default provider for every routing slot".dimmed());
    }
    match verified {
        None => println!(
            "    {} {}",
            "!".yellow().bold(),
            "not verified (--no-verify) — nothing has proved this key works".yellow()
        ),
        Some(Ok(reply)) => println!("    {} verified against {} ({reply})", "✓".green(), verify_model().dimmed()),
        Some(Err(e)) => {
            println!(
                "    {} {}",
                "✗".red().bold(),
                "written, but the gateway rejected it — it is NOT usable yet".red()
            );
            println!("      {}", e.to_string().dimmed());
        }
    }
    println!();
    println!(
        "  {} a new key gives {}, not isolation. Budget is enforced per {} — every key on org {} shares one budget.",
        "!".yellow(),
        "attribution".bold(),
        "team = org".bold(),
        org.dimmed()
    );
    println!(
        "    {} to isolate this workload, provision it into a different org: {}",
        "→".dimmed(),
        "th llm provision --org-id <other-org>".bold()
    );
    println!();
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, reason = "unwrap/expect are the idiom for test assertions")]
mod provision_tests {
    use super::{back_up, key_exists, key_lost_message, sanitize_key_name, verify_gateway, verify_model, write_gateway_key};

    /// The API rejects anything outside `^[a-zA-Z0-9][a-zA-Z0-9_-]{0,63}$`
    /// with a 400, and a Mac's hostname routinely contains spaces and
    /// apostrophes. Every input has to come out mintable.
    #[test]
    fn sanitized_names_always_satisfy_the_api_rule() {
        for raw in ["big-smooth-Brent's MacBook Pro", "  ", "---", "ok_name-1", "ünïcødé", &"x".repeat(200), ""] {
            let out = sanitize_key_name(raw);
            assert!(!out.is_empty(), "`{raw}` sanitized to empty");
            assert!(out.len() <= 64, "`{raw}` sanitized to {} chars", out.len());
            assert!(
                out.chars().next().is_some_and(|c| c.is_ascii_alphanumeric()),
                "`{raw}` → `{out}` must start alphanumeric"
            );
            assert!(
                out.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_'),
                "`{raw}` → `{out}` has a character the API rejects"
            );
        }
    }

    /// Idempotency hinges on this: a name already on the org must be
    /// detected, or a re-run stacks a second billable key behind the
    /// same label.
    #[test]
    fn key_exists_matches_by_name_only() {
        let list = serde_json::json!({ "keys": [
            { "name": "big-smooth-laptop", "mask": "sk-…abc" },
            { "name": "ci", "mask": "sk-…def" },
        ]});
        assert!(key_exists(&list, "big-smooth-laptop"));
        assert!(key_exists(&list, "ci"));
        assert!(!key_exists(&list, "big-smooth-other"));
        // A mask is not a name.
        assert!(!key_exists(&list, "sk-…abc"));
        // Empty / malformed payloads are "not present", never a panic.
        assert!(!key_exists(&serde_json::json!({ "keys": [] }), "ci"));
        assert!(!key_exists(&serde_json::json!({}), "ci"));
    }

    #[test]
    fn write_stores_the_key_wires_routing_and_keeps_other_providers() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("providers.json");

        // Pre-existing config from some other provider — provisioning
        // must not evict it.
        let mut existing = smooth_operator::providers::ProviderRegistry::new();
        existing.register_provider(smooth_operator::providers::ProviderConfig::openai("openai-key"));
        existing.set_default_provider("openai");
        existing.save_to_file(&path).unwrap();

        let backup = write_gateway_key(&path, "sk-minted-value", true).unwrap();
        assert!(backup.expect("an existing file must be backed up").exists());

        let saved = smooth_cast::provider_migration::load_providers_with_migration(&path).unwrap();
        assert_eq!(saved.get_provider("smooai-gateway").unwrap().api_key, "sk-minted-value");
        assert!(saved.get_provider("openai").is_some(), "existing providers must survive");
        // Routing points at the gateway with CONCRETE models — the
        // `smooth-*` aliases are dead at the gateway (SMOODEV-1793).
        assert_eq!(saved.routing.coding.provider, "smooai-gateway");
        assert!(
            !saved.routing.coding.model.starts_with("smooth-"),
            "wrote a retired alias: {}",
            saved.routing.coding.model
        );
        assert!(!saved.get_provider("smooai-gateway").unwrap().default_model.starts_with("smooth-"));
    }

    /// `--credential-only`: store the key, leave the user's routing and
    /// default provider exactly as they were.
    #[test]
    fn credential_only_leaves_routing_alone() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("providers.json");
        let mut existing = smooth_operator::providers::ProviderRegistry::new();
        existing.register_provider(smooth_operator::providers::ProviderConfig::openai("openai-key"));
        existing.routing.coding = smooth_operator::providers::ModelSlot::new("openai", "gpt-4o");
        existing.save_to_file(&path).unwrap();

        write_gateway_key(&path, "sk-minted-value", false).unwrap();

        let saved = smooth_cast::provider_migration::load_providers_with_migration(&path).unwrap();
        assert_eq!(saved.get_provider("smooai-gateway").unwrap().api_key, "sk-minted-value");
        assert_eq!(saved.routing.coding.provider, "openai");
    }

    #[test]
    fn first_run_with_no_file_needs_no_backup() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("providers.json");
        assert!(back_up(&path).unwrap().is_none());
        assert!(write_gateway_key(&path, "sk-first", true).unwrap().is_none());
        assert!(path.exists());
    }

    /// A failed write must surface as an error — the caller prints the
    /// key rather than losing a live, billable credential.
    #[test]
    fn a_failed_write_errors_and_the_message_carries_the_key() {
        let dir = tempfile::tempdir().unwrap();
        // A directory where the file should be: every write fails.
        let path = dir.path().join("providers.json");
        std::fs::create_dir(&path).unwrap();

        let err = write_gateway_key(&path, "sk-would-be-lost", true).expect_err("writing over a directory must fail");
        let msg = key_lost_message("big-smooth-test", "sk-would-be-lost", &err);
        assert!(
            msg.contains("sk-would-be-lost"),
            "the key value must reach the terminal when it cannot be stored"
        );
        assert!(msg.contains("big-smooth-test"), "the message must name the key so it can be revoked");
    }

    /// The backup is not the only thing that can fail — a save into a
    /// read-only directory must propagate too. Without this the error
    /// path is only ever exercised by `back_up`, and a swallowed
    /// `save_to_file` would look tested.
    #[cfg(unix)]
    #[test]
    fn a_failed_save_errors_too() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempfile::tempdir().unwrap();
        let locked = dir.path().join("locked");
        std::fs::create_dir(&locked).unwrap();
        std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o555)).unwrap();

        let result = write_gateway_key(&locked.join("providers.json"), "sk-would-be-lost", true);

        // Restore before asserting so the tempdir can always clean up.
        std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o755)).unwrap();
        let err = result.expect_err("a save into a read-only directory must fail");
        assert!(key_lost_message("k", "sk-would-be-lost", &err).contains("sk-would-be-lost"));
    }

    /// Verification has to actually fail when the gateway rejects the
    /// key. A key written into a file is not evidence that it works —
    /// removing this check is how "provisioned" starts meaning nothing.
    #[tokio::test]
    async fn verification_fails_when_the_gateway_rejects_the_key() {
        let server = tiny_http::Server::http("127.0.0.1:0").expect("bind stub gateway");
        let url = format!("http://{}/v1", server.server_addr());
        let handle = std::thread::spawn(move || {
            if let Ok(req) = server.recv() {
                let _ = req.respond(tiny_http::Response::from_string(r#"{"error":{"message":"Invalid proxy server token passed"}}"#).with_status_code(401));
            }
        });

        let result = verify_gateway(&url, "sk-rejected").await;
        handle.join().ok();
        assert!(result.is_err(), "a 401 from the gateway must not read as verified");
    }

    /// The verification call must never resurrect a retired `smooth-*`
    /// alias — those 400 at the gateway, which would look like a bad key.
    #[test]
    fn verify_model_is_concrete() {
        assert!(!verify_model().starts_with("smooth-"), "verify model is a retired alias: {}", verify_model());
    }
}
