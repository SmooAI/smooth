//! Smoo AI platform CLI commands. All go through `smooth_api_client`
//! against `api.smoo.ai`. Resources are split into submodules; this
//! file keeps the auth flow (`login` / `logout` / `whoami`) and the
//! org commands because every other command needs an authenticated
//! client + active org id and that machinery lives here.
//!
//! Helper: `require_active_org(&client)` resolves the `--org` flag
//! → `SMOOAI_ORG_ID` env → `active_org_id` in credentials, in that
//! order. Most resource commands take an `Option<String>` for `--org`
//! and call this helper.

pub mod agents;
pub mod booking;
pub mod branding;
pub mod crawl;
pub mod crm;
pub mod dashboard;
pub mod files;
pub mod heypage;
pub mod integrations;
pub mod jobs;
pub mod keys;
pub mod knowledge;
pub mod llm_gateway;
pub mod members;
pub mod notify;
pub mod observability;
pub mod products;
pub mod profile;
pub mod referrals;
pub mod roles;
pub mod smooth_operator;
pub mod smooth_operator_ws;
pub mod teams;
pub mod testing;
pub mod user_client;
pub mod websearch;
pub mod widgets;

use std::io::IsTerminal;

use anstream::println;
use anyhow::{bail, Context, Result};
use dialoguer::{theme::ColorfulTheme, Select};
use owo_colors::OwoColorize;
use smooth_api_client::{CredentialsStore, SmoothApiClient};

/// Build an authenticated client or fail with the standard "run
/// `th api login`" message. Every resource command starts with this.
///
/// Triggers a silent token refresh first if creds exist but are
/// expired AND we have stored client_id/client_secret. So a stale
/// session re-mints transparently — the user doesn't see the
/// expiry unless their stored M2M credentials were rotated.
pub async fn require_authed() -> Result<SmoothApiClient> {
    // Prefer a valid USER session (`~/.smooth/auth/smooai-user.json`, written by
    // `th auth login` and used by `th config`) so `th api`/`th admin` honor a
    // logged-in user — the platform API accepts user JWTs
    // (`assertMachineTokenAuthorizedForOrg` passes Supabase auth). Fall back to
    // the M2M session (`smooai.json`) otherwise. `SmoothApiClient::from_disk`'s
    // own store is hard-wired to the M2M file, hence the explicit user-store load
    // here. An expired user JWT IS Supabase-refreshed (pearl th-2273b8) — it only
    // falls through to M2M when there's no session or no refresh material.
    if let Some(client) = try_user_session().await {
        return Ok(client);
    }

    let client = SmoothApiClient::from_disk().context("load credentials")?;
    if client.credentials().is_none() {
        anyhow::bail!("not logged in — run `th auth login` (user) or `th api login` (M2M) first");
    }
    // Try to refresh if expired. ensure_fresh_token is a no-op when
    // the token is still valid or when no client_credentials are on
    // disk to re-exchange with.
    client.ensure_fresh_token().await.ok();
    if !client.is_authenticated() {
        anyhow::bail!(
            "session expired and no stored client credentials to auto-refresh — run `th auth login` again \
             (or set SMOOAI_CONFIG_CLIENT_ID + SMOOAI_CONFIG_CLIENT_SECRET so the next call refreshes silently)"
        );
    }
    Ok(client)
}

/// Build an authed client from the signed-in **user** session only — no M2M
/// fallback.
///
/// For platform surfaces scoped to a person rather than an org (agent mail,
/// th-b02f63): an org machine token carries no user identity, so those routes
/// 403 it. Failing here with the real fix beats a server error the caller has
/// to decode.
///
/// # Errors
/// Returns an error naming `th auth login` when there is no usable user session.
pub(crate) async fn require_user_session() -> Result<SmoothApiClient> {
    try_user_session()
        .await
        .context("not signed in as a Smoo user — run `th auth login` (an org M2M key won't do: this surface is user-scoped)")
}

/// Build an authed client from the user JWT at `~/.smooth/auth/smooai-user.json`,
/// or `None` if it's absent/unreadable, or expired with no way to refresh — in
/// which case the caller falls back to M2M.
///
/// An expired session is refreshed through [`crate::auth::refresh`] (the shared
/// choke point), which persists the rotated refresh token via the client-shared
/// store. That store — not `smooth_api_client`'s — has to own the write: its
/// `Credentials` carries the `kind` discriminator, and round-tripping a user
/// session through the api-client's narrower type would silently downgrade it
/// to `M2m` on disk. Pearl th-2273b8.
async fn try_user_session() -> Option<SmoothApiClient> {
    let http = reqwest::Client::builder()
        .user_agent(format!("th/{}", env!("CARGO_PKG_VERSION")))
        .build()
        .ok()?;
    for path in user_jwt_candidates() {
        if !path.exists() {
            continue;
        }
        let shared_store = smooai_client_shared::auth::storage::CredentialsStore::at(&path);
        let Ok(creds) = crate::auth::refresh::fresh_user_credentials_from(&http, &shared_store).await else {
            continue;
        };
        let Ok(client) = SmoothApiClient::new(smooth_api_client::base_url(), Some(to_api_credentials(&creds)), CredentialsStore::at(&path)) else {
            continue;
        };
        if client.is_authenticated() {
            return Some(client);
        }
    }
    None
}

/// Narrow a client-shared session to the `smooth_api_client` shape. Read-only
/// direction only — see [`try_user_session`] for why the reverse is unsafe.
fn to_api_credentials(creds: &smooai_client_shared::auth::storage::Credentials) -> smooth_api_client::Credentials {
    smooth_api_client::Credentials {
        access_token: creds.access_token.clone(),
        refresh_token: creds.refresh_token.clone(),
        expires_at: creds.expires_at,
        user: creds.user.clone(),
        active_org_id: creds.active_org_id.clone(),
        client_id: creds.client_id.clone(),
        client_secret: creds.client_secret.clone(),
        created_at: creds.created_at,
    }
}

/// Candidate paths for the user JWT, in priority order. `th auth login` writes the
/// session under the active profile (`~/.config/smooth/auth/profiles/<name>/`),
/// while older builds + `SMOOAI_USER_AUTH_FILE` use the flat legacy path — so we
/// try them all and use the first that holds a valid session.
///   1. `$SMOOAI_USER_AUTH_FILE` (explicit override)
///   2. the active profile's session file
///   3. the default (unnamed) profile's session file
///   4. legacy `~/.smooth/auth/smooai-user.json`
///
/// Steps 2–4 delegate to `smooth_policy::auth_paths` — the single source of
/// truth both `th` and `smooth-daemon` resolve through (th-16b0ca). This used
/// to re-derive them from `$XDG_CONFIG_HOME`/`$HOME` by hand, and the copy
/// drifted twice over: it skipped the default profile entirely, and `$HOME` is
/// unset on Windows (it's `%USERPROFILE%`), so BOTH non-override candidates
/// evaporated there and `th api` reported "not logged in" against a perfectly
/// good session.
fn user_jwt_candidates() -> Vec<std::path::PathBuf> {
    use smooth_policy::auth_paths;
    let mut paths = Vec::new();

    if let Some(explicit) = std::env::var_os("SMOOAI_USER_AUTH_FILE") {
        paths.push(std::path::PathBuf::from(explicit));
    }
    if let Some(name) = auth_paths::resolve_profile(None) {
        paths.push(auth_paths::user_file(Some(&name)));
    }
    paths.push(auth_paths::user_file(None));
    if let Some(home) = dirs_next::home_dir() {
        paths.push(home.join(".smooth").join("auth").join("smooai-user.json"));
    }

    paths
}

/// Resolve the active org id. Delegates to
/// [`crate::active_org::resolve`] so every `th api` subcommand reads
/// from the same source `th config` and `th auth whoami` do.
///
/// The `_client` parameter is retained for API stability (callers
/// across this crate pass it through), but is unused — the shared
/// helper reads directly from the credential stores on disk.
///
/// Order:
///   1. `--org` flag (the `override_org` argument)
///   2. `SMOOAI_ORG_ID` env (handy for CI scripts)
///   3. `active_org_id` from any of: legacy `smooth-api-client`
///      store, client-shared M2M store, client-shared User store
pub fn require_active_org(_client: &SmoothApiClient, override_org: Option<String>) -> Result<String> {
    crate::active_org::resolve(override_org)
}

/// Read a JSON body from `path` (or stdin when `path == "-"`).
pub fn read_body(path: &str) -> Result<serde_json::Value> {
    let raw = if path == "-" {
        use std::io::Read;
        let mut s = String::new();
        std::io::stdin().read_to_string(&mut s).context("read stdin")?;
        s
    } else {
        std::fs::read_to_string(path).with_context(|| format!("read {path}"))?
    };
    serde_json::from_str(&raw).with_context(|| format!("parse JSON from {path}"))
}

/// Pretty-print a JSON value to stdout with a leading + trailing
/// blank line so command output looks consistent with the rest of
/// the CLI.
pub fn print_json(body: &serde_json::Value) {
    println!();
    println!("{}", serde_json::to_string_pretty(body).unwrap_or_default());
    println!();
}

/// Pretty-print a `{"data": [...]}` collection envelope as a compact
/// list. Each entry shows whichever of `id`, `name`, `email`,
/// `status` are present. Falls back to full JSON when the shape
/// doesn't match the envelope.
/// Trailing discriminator for one row of a list envelope: `[status]` when the
/// item has one, else `(slug)`. Orgs carry a slug and no status, which is the
/// only reason `th orgs list` used to need its own printer.
fn envelope_suffix(item: &serde_json::Value) -> String {
    match item.get("status").and_then(|v| v.as_str()) {
        Some(s) if !s.is_empty() => format!(" [{s}]"),
        _ => match item.get("slug").and_then(|v| v.as_str()) {
            Some(s) if !s.is_empty() => format!(" ({s})"),
            _ => String::new(),
        },
    }
}

pub fn print_list_envelope(body: &serde_json::Value, item_label: &str) {
    let items = body.get("data").and_then(|v| v.as_array()).or_else(|| body.as_array());
    let Some(items) = items else {
        print_json(body);
        return;
    };
    println!();
    if items.is_empty() {
        println!("  {} {}", "●".dimmed(), format!("no {item_label}").dimmed());
        println!();
        return;
    }
    for item in items {
        let id = item.get("id").and_then(|v| v.as_str()).unwrap_or("?");
        let name = item
            .get("name")
            .and_then(|v| v.as_str())
            .or_else(|| item.get("email").and_then(|v| v.as_str()))
            .unwrap_or("");
        let suffix = envelope_suffix(item);
        println!("  {} {} {}{}", "○".dimmed(), id.cyan(), name.bold(), suffix.dimmed());
    }
    println!();
}

/// `th orgs *` dispatch — list / show / switch.
///
/// SMOODEV-1937: `/organizations*` are user-kind routes — they 401 under an
/// M2M token ("auth kind does not satisfy route requirement"). Use the user
/// session (`th auth login`) via [`UserClient`], same as the CRM commands.
pub async fn cmd_orgs(cmd: super::OrgsCommands) -> Result<()> {
    let client = user_client::UserClient::from_user_session().await?;
    match cmd {
        super::OrgsCommands::List => {
            let body = client.get("/organizations").await.context("GET /organizations")?;
            print_list_envelope(&body, "organizations");
        }
        super::OrgsCommands::Show { org_id } => {
            // Use the shared resolver so `th api orgs show` honors
            // the same active-org contract as the rest of the CLI.
            let resolved =
                crate::active_org::resolve(org_id).context("no org id specified and no active org set — pass <org_id> or run `th api orgs switch <id>`")?;
            print_json(&client.get(&format!("/organizations/{resolved}")).await.context("GET /organizations/{org_id}")?);
        }
        super::OrgsCommands::Switch { org_id } => {
            // Resolve the target org: a UUID is used directly; a name/slug
            // substring is matched against the orgs you belong to; an omitted
            // arg opens an interactive picker on a TTY.
            let target = resolve_switch_org(&client, org_id).await?;
            // Persist to every credential store we know about so the
            // active org is visible to `th config`, `th auth whoami`,
            // and any other subcommand that reads a different store.
            // See `crates/smooth-cli/src/active_org.rs` for the
            // cross-subcommand contract this enforces.
            let updated = crate::active_org::set(&target.id).context("save active org")?;
            println!();
            let label = if target.name.is_empty() || target.name == target.id {
                target.id.cyan().bold().to_string()
            } else {
                format!("{} {}", target.name.bold(), format!("({})", target.id).dimmed())
            };
            println!("  {} Active org set to {label}", "✓".green().bold());
            if updated > 1 {
                println!("    {} updated {} credential stores", "●".dimmed(), updated.to_string().dimmed());
            }
            println!();
        }
    }
    Ok(())
}

/// A single org as far as the switcher cares.
#[derive(Debug)]
struct OrgRef {
    id: String,
    name: String,
    slug: String,
}

/// True for a 36-char `8-4-4-4-12` hyphenated UUID. Loose on purpose — we
/// only need to tell "looks like an id" from "looks like a name" so we know
/// whether to allow a direct set (e.g. a managed child org the caller isn't a
/// direct member of) vs. a name/slug match.
fn looks_like_uuid(s: &str) -> bool {
    let bytes = s.as_bytes();
    bytes.len() == 36
        && bytes.iter().enumerate().all(|(i, &b)| {
            if matches!(i, 8 | 13 | 18 | 23) {
                b == b'-'
            } else {
                (b as char).is_ascii_hexdigit()
            }
        })
}

/// Parse a `{data:[...]}` or bare-array orgs payload into `OrgRef`s.
fn org_refs_from_body(body: &serde_json::Value) -> Vec<OrgRef> {
    let items = body
        .get("data")
        .and_then(|v| v.as_array())
        .or_else(|| body.as_array())
        .cloned()
        .unwrap_or_default();
    items
        .iter()
        .filter_map(|org| {
            let id = org.get("id").and_then(|v| v.as_str())?.to_string();
            let name = org.get("name").and_then(|v| v.as_str()).unwrap_or("").to_string();
            let slug = org.get("slug").and_then(|v| v.as_str()).unwrap_or("").to_string();
            Some(OrgRef { id, name, slug })
        })
        .collect()
}

/// Fetch the orgs the logged-in user belongs to.
async fn fetch_user_orgs(client: &user_client::UserClient) -> Result<Vec<OrgRef>> {
    let body = client.get("/organizations").await.context("GET /organizations")?;
    Ok(org_refs_from_body(&body))
}

/// For each org the user belongs to, fetch its ACTIVE child orgs (where
/// that org is the *parent*) via the relationships API — these are the
/// child orgs the caller can act on as a parent-org admin. Best-effort:
/// a failing relationships call for one parent is skipped, not fatal
/// (so `th org list` never breaks just because one org has no
/// relationships endpoint access). Deduped against the parent set.
/// Returns `(child, parent_name)`.
async fn fetch_child_orgs(client: &user_client::UserClient, parents: &[OrgRef]) -> Vec<(OrgRef, String)> {
    use std::collections::HashSet;
    let mut seen: HashSet<String> = parents.iter().map(|p| p.id.clone()).collect();
    let mut out: Vec<(OrgRef, String)> = Vec::new();
    for p in parents {
        let Ok(body) = client.get(&format!("/organizations/{}/relationships", p.id)).await else {
            continue;
        };
        let rels = body.as_array().cloned().unwrap_or_default();
        for r in rels {
            let is_parent_side = r.get("parentOrgId").and_then(|v| v.as_str()) == Some(p.id.as_str());
            let active = r.get("status").and_then(|v| v.as_str()) == Some("active");
            if !is_parent_side || !active {
                continue;
            }
            let Some(child) = r.get("childOrganization") else { continue };
            let Some(id) = child.get("id").and_then(|v| v.as_str()) else { continue };
            if !seen.insert(id.to_string()) {
                continue;
            }
            out.push((
                OrgRef {
                    id: id.to_string(),
                    name: child.get("name").and_then(|v| v.as_str()).unwrap_or("").to_string(),
                    slug: child.get("slug").and_then(|v| v.as_str()).unwrap_or("").to_string(),
                },
                p.name.clone(),
            ));
        }
    }
    out
}

/// Resolve a `th api orgs switch` target. See the enum doc for the contract:
/// UUID → direct; name/slug substring → matched against your orgs; omitted →
/// interactive picker on a TTY.
async fn resolve_switch_org(client: &user_client::UserClient, arg: Option<String>) -> Result<OrgRef> {
    let orgs = fetch_user_orgs(client).await?;

    let Some(query) = arg else {
        // Interactive picker.
        if !std::io::stdin().is_terminal() || !std::io::stdout().is_terminal() {
            bail!("no org id given and no TTY for the interactive picker — pass a UUID or a name/slug substring");
        }
        if orgs.is_empty() {
            bail!("you don't belong to any organizations");
        }
        let labels: Vec<String> = orgs
            .iter()
            .map(|o| {
                let slug = if o.slug.is_empty() { String::new() } else { format!(" ({})", o.slug) };
                let name = if o.name.is_empty() { "(unnamed)" } else { &o.name };
                format!("{name}{slug}  —  {}", o.id)
            })
            .collect();
        let picked = Select::with_theme(&ColorfulTheme::default())
            .with_prompt("Select an organization")
            .items(&labels)
            .default(0)
            .interact()
            .context("org picker")?;
        // `picked` is a valid index into `orgs` by construction.
        return Ok(orgs.into_iter().nth(picked).expect("picker index in range"));
    };

    // Members first (fast). A UUID is handled directly by resolve_org_query.
    match resolve_org_query(&orgs, &query) {
        Ok(found) => Ok(found),
        // Name miss (non-UUID): fall back to child orgs you manage as a
        // parent-org admin — fetched ONLY on a miss so the common path
        // (member match, or a UUID) never pays for the relationships scan.
        Err(_) if !looks_like_uuid(&query) => {
            let children: Vec<OrgRef> = fetch_child_orgs(client, &orgs).await.into_iter().map(|(c, _)| c).collect();
            resolve_org_query(&children, &query).map_err(|_| {
                anyhow::anyhow!("no organization (member or managed child) matches \"{query}\" — run `th org list`, or pass the child org's UUID directly")
            })
        }
        Err(e) => Err(e),
    }
}

/// Pure resolution of a non-empty switch query against the caller's orgs.
/// Order: exact id → bare UUID (allow direct set for a managed child org the
/// caller isn't a direct member of) → case-insensitive name/slug substring.
/// Split out from [`resolve_switch_org`] so it's unit-testable without a client.
fn resolve_org_query(orgs: &[OrgRef], query: &str) -> Result<OrgRef> {
    let clone = |o: &OrgRef| OrgRef {
        id: o.id.clone(),
        name: o.name.clone(),
        slug: o.slug.clone(),
    };

    // Exact id match against the list (preferred — gives us the display name).
    if let Some(found) = orgs.iter().find(|o| o.id == query) {
        return Ok(clone(found));
    }

    // A UUID we don't belong to (e.g. a managed child org) — allow direct set.
    if looks_like_uuid(query) {
        return Ok(OrgRef {
            id: query.to_string(),
            name: String::new(),
            slug: String::new(),
        });
    }

    // Otherwise treat it as a case-insensitive name/slug substring.
    let needle = query.to_lowercase();
    let matches: Vec<&OrgRef> = orgs
        .iter()
        .filter(|o| o.name.to_lowercase().contains(&needle) || o.slug.to_lowercase().contains(&needle))
        .collect();
    match matches.as_slice() {
        [one] => Ok(clone(one)),
        [] => bail!("no organization matches \"{query}\" — run `th api orgs list` to see your orgs"),
        many => {
            let names: Vec<String> = many.iter().map(|o| format!("{} ({})", o.name, o.id)).collect();
            bail!("\"{query}\" matches {} orgs — be more specific:\n  {}", many.len(), names.join("\n  "))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn org(id: &str, name: &str, slug: &str) -> OrgRef {
        OrgRef {
            id: id.to_string(),
            name: name.to_string(),
            slug: slug.to_string(),
        }
    }

    fn sample() -> Vec<OrgRef> {
        vec![
            org("8be5f5fd-cf71-43ba-9df9-01e15acdaf8e", "Smoo AI", "smoo-ai"),
            org("11111111-1111-4111-8111-111111111111", "ATS", "ats"),
            org("22222222-2222-4222-8222-222222222222", "Amplified Tech Solutions", "amplified"),
        ]
    }

    /// The candidate list must never be empty, on any platform. It used to be
    /// derived from `$XDG_CONFIG_HOME`/`$HOME`; `HOME` is unset on Windows, so
    /// every non-override candidate vanished and `th api` reported "not logged
    /// in" against a perfectly good session. Both surviving candidates now come
    /// from `smooth_policy::auth_paths` / `dirs_next`.
    ///
    /// Reads no env and touches no disk, so it's safe to run alongside the
    /// env-mutating tests elsewhere in the binary (th-129eda).
    #[test]
    fn user_jwt_candidates_resolve_on_every_platform() {
        let paths = user_jwt_candidates();
        assert!(!paths.is_empty(), "at least the default profile + legacy paths must resolve");
        assert!(paths.iter().all(|p| p.is_absolute()), "candidates must be absolute, got {paths:?}");
        assert!(
            paths.iter().any(|p| p.ends_with("auth/smooai-user.json")),
            "the default profile's session file must be a candidate: {paths:?}"
        );
        assert!(
            paths.iter().any(|p| p.ends_with(".smooth/auth/smooai-user.json")),
            "the legacy path must still be a candidate: {paths:?}"
        );
    }

    #[test]
    fn uuid_shape_detection() {
        assert!(looks_like_uuid("8be5f5fd-cf71-43ba-9df9-01e15acdaf8e"));
        assert!(!looks_like_uuid("ats"));
        assert!(!looks_like_uuid("8be5f5fd-cf71-43ba-9df9-01e15acdaf8")); // 35 chars
        assert!(!looks_like_uuid("8be5f5fdXcf71X43baX9df9X01e15acdaf8e")); // no hyphens
        assert!(!looks_like_uuid("zzzzzzzz-cf71-43ba-9df9-01e15acdaf8e")); // non-hex
    }

    #[test]
    fn exact_id_match_returns_with_display_name() {
        let got = resolve_org_query(&sample(), "11111111-1111-4111-8111-111111111111").expect("match");
        assert_eq!(got.id, "11111111-1111-4111-8111-111111111111");
        assert_eq!(got.name, "ATS");
    }

    #[test]
    fn unknown_uuid_is_allowed_as_direct_set() {
        // A managed child org the caller isn't a direct member of.
        let got = resolve_org_query(&sample(), "99999999-9999-4999-8999-999999999999").expect("direct");
        assert_eq!(got.id, "99999999-9999-4999-8999-999999999999");
        assert!(got.name.is_empty());
    }

    #[test]
    fn name_substring_is_case_insensitive() {
        let got = resolve_org_query(&sample(), "ATS").expect("match");
        assert_eq!(got.name, "ATS");
        // slug match, different case
        let got2 = resolve_org_query(&sample(), "SMOO").expect("match");
        assert_eq!(got2.name, "Smoo AI");
    }

    #[test]
    fn no_match_errors() {
        let err = resolve_org_query(&sample(), "nope").expect_err("should fail");
        assert!(format!("{err}").contains("no organization matches"), "{err}");
    }

    #[test]
    fn ambiguous_name_lists_candidates() {
        // "a" is a substring of "Smoo AI"? no — but "ats"/"amplified"/"ATS" all
        // contain 'a'. Use a needle that hits more than one: "a".
        let err = resolve_org_query(&sample(), "a").expect_err("ambiguous");
        let msg = format!("{err}");
        assert!(msg.contains("matches"), "{msg}");
        assert!(msg.contains("be more specific"), "{msg}");
    }

    /// Pearl th-91de11: `print_orgs_list` was a near-copy of
    /// `print_list_envelope` that existed only to show a `slug` where the
    /// shared helper showed a `status`. Teaching the helper to fall back to
    /// `slug` let the copy go — this pins the fallback so deleting it is a
    /// test failure, not a silent regression in `th orgs list`.
    #[test]
    fn envelope_suffix_prefers_status_then_falls_back_to_slug() {
        assert_eq!(envelope_suffix(&serde_json::json!({ "status": "active", "slug": "acme" })), " [active]");
        assert_eq!(envelope_suffix(&serde_json::json!({ "slug": "acme" })), " (acme)");
        assert_eq!(envelope_suffix(&serde_json::json!({ "status": "", "slug": "acme" })), " (acme)");
        assert_eq!(envelope_suffix(&serde_json::json!({ "slug": "" })), "");
        assert_eq!(envelope_suffix(&serde_json::json!({ "id": "x" })), "");
    }
}
