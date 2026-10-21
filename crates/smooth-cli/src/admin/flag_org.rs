//! `th smoo admin config flag-org add|remove <flagKey> <orgId>` — turn a
//! per-org feature flag on or off for ONE org without retyping the rest of
//! its rules (SMOODEV-4052).
//!
//! The per-org call switches (`callDepartmentRoutingEnabled`,
//! `callContactOwnerRoutingEnabled`, `callVoicemailEnabled`, …) are
//! `feature_flag`-tier values in the platform's own config (the Smoo AI
//! master org). Each one is either a plain boolean or a segment envelope:
//!
//! ```json
//! { "$segment": true, "default": false,
//!   "rules": [{ "id": "orgs", "when": { "orgId": { "op": "in", "value": ["<org>", "…"] } }, "value": true }] }
//! ```
//!
//! `th config set` / `smoo config set` REPLACES the whole value, so adding
//! one org meant copying every other org id back in by hand, and a typo
//! dropped a customer's switch. This command reads the value stored in that
//! exact environment (no fallback chain), changes only the one org id in the
//! org allowlist rule, prints before and after, confirms on a TTY (`--yes`
//! in scripts), re-reads it (a best-effort check — the API has no
//! compare-and-swap) and writes it back. Every other rule, the default and
//! the rule order are kept byte for byte. The key is matched canonically
//! (`CALL_VOICEMAIL_ENABLED` finds `callVoicemailEnabled`) and written under
//! the stored or schema-declared name, never the typed spelling.
//!
//! The pure merge ([`plan`]) refuses any shape it does not understand (a
//! string, a number, an object that is not a segment envelope, a malformed
//! rule) rather than guessing — fix those with `th config set` deliberately.

use anstream::println;
use anyhow::{bail, Context, Result};
use clap::Subcommand;
use owo_colors::OwoColorize;
use serde_json::{json, Map, Value};

use crate::config::{canonical_key, declared_feature_flag_key, derive_tier_from_schemas, ConfigClient, Tier};
use crate::destructive::{gate_with, Confirm, Severity, Target};

/// Id given to a newly created org allowlist rule.
const ORG_RULE_ID: &str = "orgs";

#[derive(Debug, Subcommand)]
pub enum FlagOrgCmd {
    /// Turn a feature flag ON for one org: add it to the flag's orgId rule
    /// (created when missing), keeping every other rule.
    Add(FlagOrgArgs),
    /// Turn a feature flag OFF for one org: take it out of the flag's orgId
    /// rule(s), keeping every other rule.
    #[command(visible_alias = "rm")]
    Remove(FlagOrgArgs),
}

#[derive(Debug, clap::Args)]
pub struct FlagOrgArgs {
    /// The feature flag key, e.g. `callDepartmentRoutingEnabled`.
    pub flag_key: String,
    /// The customer org id to add or remove.
    pub org_id: String,
    /// Config environment name. No default: say which one you mean.
    #[arg(long, alias = "env")]
    pub environment: String,
    /// The org that OWNS the config (the platform/master org). Defaults to
    /// the active org.
    #[arg(long = "config-org-id", value_name = "ORG_ID")]
    pub config_org_id: Option<String>,
    /// Schema to read/write under when the flag has no value in this
    /// environment yet, or when it has one under several schemas.
    #[arg(long)]
    pub schema_name: Option<String>,
    #[command(flatten)]
    pub confirm: Confirm,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Op {
    Add,
    Remove,
}

/// The result of [`plan`]: the value to write, and what to tell the operator.
#[derive(Debug, PartialEq, Eq)]
pub struct Plan {
    pub after: Value,
    pub changed: bool,
    pub notes: Vec<String>,
    /// Add only: an earlier deny rule names the org, so the flag stays off for it.
    pub still_off: bool,
}

/// A canonical (lowercase, hyphenated) org UUID.
///
/// # Errors
/// When `raw` is not a UUID.
pub fn parse_org(raw: &str) -> Result<String> {
    uuid::Uuid::parse_str(raw.trim())
        .map(|u| u.hyphenated().to_string())
        .map_err(|_| anyhow::anyhow!("org `{raw}` is not a UUID"))
}

/// A flag key as the config server names it: letters, digits, `_`.
///
/// # Errors
/// On an empty key or one with any other character.
pub fn parse_flag_key(raw: &str) -> Result<String> {
    let key = raw.trim();
    if key.is_empty() || !key.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
        bail!("flag key `{raw}` is not valid — use the key as declared in .smooai-config (e.g. `callDepartmentRoutingEnabled`)");
    }
    Ok(key.to_string())
}

/// The org ids an `orgId` clause names, when the clause is one this command
/// can edit: `{op: "in", value: [ids…]}` or `{op: "eq", value: id}`.
fn org_clause_ids(clause: &Value) -> Option<Vec<String>> {
    let op = clause.get("op")?.as_str()?;
    let value = clause.get("value")?;
    match op {
        "in" => value.as_array()?.iter().map(|v| v.as_str().map(str::to_ascii_lowercase)).collect(),
        "eq" => Some(vec![value.as_str()?.to_ascii_lowercase()]),
        _ => None,
    }
}

/// Is this rule an org allowlist — `when` is ONLY an editable `orgId` clause
/// and the rule grants (`value: true`)? Rules with more clauses (orgId AND
/// email) are left alone: editing them would change a different audience.
fn org_allowlist(rule: &Value) -> Option<Vec<String>> {
    if rule.get("value") != Some(&Value::Bool(true)) {
        return None;
    }
    let when = rule.get("when")?.as_object()?;
    if when.len() != 1 {
        return None;
    }
    org_clause_ids(when.get("orgId")?)
}

/// A rule id not already used in `rules`.
fn fresh_rule_id(rules: &[Value]) -> String {
    let taken = |id: &str| rules.iter().any(|r| r.get("id").and_then(Value::as_str) == Some(id));
    if !taken(ORG_RULE_ID) {
        return ORG_RULE_ID.to_string();
    }
    // n rules take at most n ids, so one of 2..=n+2 is always free.
    (2..=rules.len() + 2)
        .map(|n| format!("{ORG_RULE_ID}-{n}"))
        .find(|id| !taken(id))
        .unwrap_or_default()
}

fn new_org_rule(id: &str, org: &str) -> Value {
    json!({ "id": id, "when": { "orgId": { "op": "in", "value": [org] } }, "value": true })
}

/// Add or remove `org` (already canonical) in the flag's org allowlist rule.
///
/// `current` is the value stored in the environment, `None` when there is
/// none. Pure: no I/O, so every shape is unit-tested.
///
/// # Errors
/// On any shape this command will not edit in place.
pub fn plan(current: Option<&Value>, org: &str, op: Op) -> Result<Plan> {
    let unchanged = |v: Value, note: String| Plan {
        after: v,
        changed: false,
        notes: vec![note],
        still_off: false,
    };
    match current {
        None | Some(Value::Null | Value::Bool(false)) => match op {
            Op::Add => Ok(Plan {
                after: json!({ "$segment": true, "default": false, "rules": [new_org_rule(ORG_RULE_ID, org)] }),
                changed: true,
                notes: vec!["the flag was off for every org; it becomes a segment that is on for this org only".to_string()],
                still_off: false,
            }),
            Op::Remove => Ok(unchanged(
                current.cloned().unwrap_or(Value::Null),
                "the flag is off for every org — nothing to remove".to_string(),
            )),
        },
        Some(Value::Bool(true)) => match op {
            Op::Add => Ok(unchanged(
                Value::Bool(true),
                "the flag is on for EVERY org already — nothing to add".to_string(),
            )),
            Op::Remove => bail!(
                "the flag is `true` for every org, so there is no org rule to take this org out of. Turning it off for one org \
                 means a segment with `default: true` and a deny rule — set that deliberately with `th config set`."
            ),
        },
        Some(Value::Object(envelope)) if envelope.get("$segment") == Some(&Value::Bool(true)) => plan_envelope(envelope, org, op),
        Some(other) => bail!(
            "the stored value is not a boolean or a segment envelope ({}), so it cannot be edited in place — fix it with `th config set`",
            compact(other)
        ),
    }
}

fn plan_envelope(envelope: &Map<String, Value>, org: &str, op: Op) -> Result<Plan> {
    let rules: Vec<Value> = match envelope.get("rules") {
        None => Vec::new(),
        Some(Value::Array(rules)) => rules.clone(),
        Some(other) => bail!("the segment's `rules` is not a list ({}) — fix it with `th config set`", compact(other)),
    };
    for (i, rule) in rules.iter().enumerate() {
        if !rule.is_object() || !rule.get("when").is_some_and(Value::is_object) || rule.get("value").is_none() {
            bail!("rule #{} of the segment is malformed ({}) — fix it with `th config set`", i + 1, compact(rule));
        }
    }
    let mut notes = Vec::new();
    let default_on = envelope.get("default") == Some(&Value::Bool(true));
    let allowlists: Vec<usize> = rules.iter().enumerate().filter(|(_, r)| org_allowlist(r).is_some()).map(|(i, _)| i).collect();
    let listed = |i: usize| org_allowlist(&rules[i]).unwrap_or_default().iter().any(|id| id == org);

    let mut out = rules.clone();
    let changed = match op {
        Op::Add => {
            if allowlists.iter().any(|&i| listed(i)) {
                notes.push("this org is already in the flag's org rule — nothing to add".to_string());
                false
            } else if let Some(&first) = allowlists.iter().find(|&&i| rules[i]["when"]["orgId"]["op"] == "in") {
                if let Some(Value::Array(ids)) = out[first].pointer_mut("/when/orgId/value") {
                    ids.push(json!(org));
                }
                true
            } else {
                out.push(new_org_rule(&fresh_rule_id(&rules), org));
                if !rules.is_empty() {
                    notes.push(
                        "no org rule existed, so one was added LAST — rules are first-match-wins, so an earlier rule still decides for this org if it matches"
                            .to_string(),
                    );
                }
                true
            }
        }
        Op::Remove => {
            let mut removed = false;
            let mut kept = Vec::with_capacity(out.len());
            for (i, mut rule) in out.into_iter().enumerate() {
                if allowlists.contains(&i) && listed(i) {
                    removed = true;
                    if rules[i]["when"]["orgId"]["op"] == "eq" {
                        continue;
                    }
                    if let Some(Value::Array(ids)) = rule.pointer_mut("/when/orgId/value") {
                        ids.retain(|v| v.as_str().map(str::to_ascii_lowercase).as_deref() != Some(org));
                        if ids.is_empty() {
                            notes.push(format!(
                                "rule `{}` named only this org, so it was dropped",
                                rule.get("id").and_then(Value::as_str).unwrap_or("?")
                            ));
                            continue;
                        }
                    }
                }
                kept.push(rule);
            }
            out = kept;
            if !removed {
                notes.push("this org is not in the flag's org rule — nothing to remove".to_string());
            }
            removed
        }
    };
    let other_rules = out.iter().filter(|r| org_allowlist(r).is_none()).count();
    if op == Op::Remove && default_on {
        notes.push("the segment's default is `true`, so this org still gets the flag unless another rule denies it".to_string());
    }
    if op == Op::Remove && other_rules > 0 {
        notes.push(format!(
            "{other_rules} other rule(s) (email, multi-clause, deny) are untouched and may still match this org"
        ));
    }
    let deny = if op == Op::Add { earlier_deny(&out, org) } else { None };
    if let Some(id) = &deny {
        notes.push(format!(
            "rule `{id}` comes earlier, denies (value false) and names this org — rules are first-match-wins, so the flag stays OFF for this org until that rule changes"
        ));
    }
    let mut after = envelope.clone();
    after.insert("rules".to_string(), Value::Array(out));
    Ok(Plan {
        after: Value::Object(after),
        changed,
        notes,
        still_off: deny.is_some(),
    })
}

/// The id of a deny rule (`value: false`) whose `orgId` clause names `org` and
/// that sits before the first allowlist naming it, so it wins for this org.
fn earlier_deny(rules: &[Value], org: &str) -> Option<String> {
    let allowed_at = rules.iter().position(|r| org_allowlist(r).is_some_and(|ids| ids.iter().any(|id| id == org)))?;
    rules[..allowed_at]
        .iter()
        .find(|r| {
            r.get("value") == Some(&Value::Bool(false))
                && r.pointer("/when/orgId")
                    .and_then(org_clause_ids)
                    .is_some_and(|ids| ids.iter().any(|id| id == org))
        })
        .map(|r| r.get("id").and_then(Value::as_str).unwrap_or("?").to_string())
}

/// One-line JSON, cut so a huge value cannot flood an error.
fn compact(v: &Value) -> String {
    let s = v.to_string();
    if s.chars().count() > 120 {
        format!("{}…", s.chars().take(120).collect::<String>())
    } else {
        s
    }
}

/// The rows of a direct schema+environment read (a bare array, or `{data}`).
fn rows(body: &Value) -> &[Value] {
    body.as_array()
        .or_else(|| body.get("data").and_then(Value::as_array))
        .map_or(&[], Vec::as_slice)
}

/// `[{id, name}]` lists come back bare or under `data`.
fn list(body: &Value) -> &[Value] {
    rows(body)
}

/// Where the flag's value lives in this environment.
struct Located {
    schema_id: String,
    /// The key as stored (or, with no value yet, as the schema declares it) —
    /// never the operator's spelling, so a write cannot fork a second row.
    key: String,
    value: Option<Value>,
}

/// The one row of a schema+environment read whose key is `flag`, compared
/// canonically (`CALL_VOICEMAIL_ENABLED` ≡ `callVoicemailEnabled`), as
/// `(stored key, value)`.
///
/// # Errors
/// When two stored keys collapse to the same canonical key.
fn match_row(rows: &[Value], flag: &str) -> Result<Option<(String, Option<Value>)>> {
    let want = canonical_key(flag);
    let mut hits = rows.iter().filter_map(|r| {
        let key = r.get("key").and_then(Value::as_str)?;
        (canonical_key(key) == want).then(|| (key.to_string(), r.get("value").cloned()))
    });
    let first = hits.next();
    if let Some((other, _)) = hits.next() {
        bail!(
            "`{flag}` matches more than one stored key (`{}` and `{other}`) — fix the duplicate with `th config`",
            first.as_ref().map_or("", |(k, _)| k.as_str())
        );
    }
    Ok(first)
}

/// The candidate schemas that declare `flag` as a feature flag, as
/// `(schema id, declared key)`.
fn declaring_schemas(candidates: &[&Value], flag: &str) -> Vec<(String, String)> {
    candidates
        .iter()
        .filter_map(|s| {
            let id = s.get("id").and_then(Value::as_str)?;
            let doc = s.get("jsonSchema").unwrap_or(s);
            declared_feature_flag_key(doc, flag).map(|key| (id.to_string(), key))
        })
        .collect()
}

/// Read the flag's value in `env_id` directly (no fallback chain) across the
/// candidate schemas.
async fn locate(cfg: &ConfigClient, config_org: &str, env_id: &str, schemas: &[Value], flag: &str, schema_name: Option<&str>) -> Result<Located> {
    let candidates: Vec<&Value> = schemas
        .iter()
        .filter(|s| schema_name.is_none_or(|n| s.get("name").and_then(Value::as_str) == Some(n)))
        .collect();
    if candidates.is_empty() {
        match schema_name {
            Some(n) => bail!("schema `{n}` not found in org {config_org}"),
            None => bail!("org {config_org} has no config schemas"),
        }
    }
    let mut found: Vec<Located> = Vec::new();
    for schema in &candidates {
        let sid = schema.get("id").and_then(Value::as_str).context("a schema has no id")?;
        let body = cfg
            .get(&format!("/organizations/{config_org}/config/schemas/{sid}/environments/{env_id}/values"))
            .await
            .with_context(|| format!("read values of schema {sid}"))?;
        if let Some((key, value)) = match_row(rows(&body), flag)? {
            found.push(Located {
                schema_id: sid.to_string(),
                key,
                value,
            });
        }
    }
    match found.len() {
        0 => {
            // No value yet: write under the schema that declares the key, by its declared name.
            let mut declaring = declaring_schemas(&candidates, flag);
            match declaring.len() {
                1 => {
                    let (schema_id, key) = declaring.remove(0);
                    Ok(Located { schema_id, key, value: None })
                }
                0 => bail!("no candidate schema declares `{flag}` as a feature flag — check `--schema-name`"),
                n => bail!("`{flag}` is declared by {n} schemas and has no value yet — pass `--schema-name` to pick one"),
            }
        }
        1 => Ok(found.remove(0)),
        n => bail!("`{flag}` has a value under {n} schemas in this environment — pass `--schema-name` to pick one"),
    }
}

fn pretty(v: Option<&Value>) -> String {
    v.map_or_else(|| "(unset)".to_string(), |v| serde_json::to_string_pretty(v).unwrap_or_default())
}

/// Run one `flag-org add|remove`.
///
/// # Errors
/// Bad args, an unknown environment/schema, a flag that is not
/// `feature_flag`-tier, a shape [`plan`] refuses, a value that changed while
/// the operator was confirming, or an API failure.
pub async fn dispatch(cmd: FlagOrgCmd) -> Result<()> {
    let (op, args) = match cmd {
        FlagOrgCmd::Add(a) => (Op::Add, a),
        FlagOrgCmd::Remove(a) => (Op::Remove, a),
    };
    let flag = parse_flag_key(&args.flag_key)?;
    let org = parse_org(&args.org_id)?;
    let cfg = ConfigClient::load(false).await?;
    let config_org = cfg.resolve_org(args.config_org_id.clone())?;

    let envs = cfg
        .get(&format!("/organizations/{config_org}/config/environments"))
        .await
        .context("list config environments")?;
    let env_id = list(&envs)
        .iter()
        .find(|e| e.get("name").and_then(Value::as_str) == Some(args.environment.as_str()))
        .and_then(|e| e.get("id").and_then(Value::as_str))
        .map(str::to_string)
        .with_context(|| format!("environment `{}` not found in org {config_org}", args.environment))?;

    let schemas_body = cfg
        .get(&format!("/organizations/{config_org}/config/schemas"))
        .await
        .context("list config schemas")?;
    let schemas = list(&schemas_body);
    match derive_tier_from_schemas(schemas, &flag) {
        Some(Tier::FeatureFlag) => {}
        Some(other) => bail!("`{flag}` is declared as a {} key, not a feature flag", other.as_wire()),
        None => bail!("`{flag}` is not declared in any config schema of org {config_org} — check the key"),
    }

    let located = locate(&cfg, &config_org, &env_id, schemas, &flag, args.schema_name.as_deref()).await?;
    let p = plan(located.value.as_ref(), &org, op)?;
    // Everything below names the key as stored/declared, not as typed.
    let flag = located.key.clone();

    println!();
    println!("  {}  {}", "flag   ".dimmed(), flag.cyan().bold());
    println!("  {}  {} ({})", "env    ".dimmed(), args.environment.yellow(), env_id.dimmed());
    println!("  {}  {}", "target ".dimmed(), org.yellow());
    println!("  {}\n{}", "before ".dimmed(), pretty(located.value.as_ref()));
    println!("  {}\n{}", "after  ".dimmed(), pretty(Some(&p.after)).bold());
    for note in &p.notes {
        println!("  {} {note}", "note".dimmed());
    }
    if !p.changed {
        println!();
        println!("  {} nothing to change", "✓".green().bold());
        return Ok(());
    }

    let id = format!("{flag} ({})", args.environment);
    let target = Target {
        verb: "update",
        noun: "feature flag",
        id: &id,
        org: &config_org,
        severity: Severity::Standard,
    };
    if !gate_with(&target, args.confirm)? {
        return Ok(());
    }

    // Best-effort optimistic check: the PUT replaces the whole value, so refuse
    // when it changed while the operator was reading the diff. A write landing
    // between this re-read and the PUT is still possible (the API has no
    // compare-and-swap).
    let again = locate(&cfg, &config_org, &env_id, schemas, &flag, args.schema_name.as_deref()).await?;
    if again.value != located.value || again.key != located.key {
        bail!(
            "`{flag}` changed in {} while you were confirming — nothing was written; run the command again",
            args.environment
        );
    }

    let body = json!({
        "schemaId": located.schema_id,
        "environmentId": env_id,
        "key": flag,
        "value": p.after,
        "tier": Tier::FeatureFlag.as_wire(),
    });
    cfg.put(&format!("/organizations/{config_org}/config/values"), &body)
        .await
        .context("PUT /config/values")?;
    println!();
    println!(
        "  {} {} {} {} in {}",
        "✓".green().bold(),
        if op == Op::Add { "added" } else { "removed" },
        org.cyan(),
        if op == Op::Add { "to" } else { "from" },
        format!("{flag} ({})", args.environment).cyan()
    );
    if p.still_off {
        println!(
            "  {} an earlier deny rule still names this org, so the flag stays OFF for it — see the note above",
            "!".yellow().bold()
        );
    }
    println!("    {}", "the voice pod and api-prime cache flags for up to 60 s".dimmed());
    Ok(())
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, reason = "test idiom")]
mod tests {
    use super::*;

    const A: &str = "11111111-1111-4111-8111-111111111111";
    const B: &str = "22222222-2222-4222-8222-222222222222";
    const C: &str = "33333333-3333-4333-8333-333333333333";

    fn env(rules: Value) -> Value {
        let mut envelope = json!({ "$segment": true, "default": false });
        envelope["rules"] = rules;
        envelope
    }

    fn org_rule(id: &str, ids: &[&str]) -> Value {
        json!({ "id": id, "when": { "orgId": { "op": "in", "value": ids } }, "value": true })
    }

    fn staff_rule() -> Value {
        json!({ "id": "smoo-staff", "when": { "email": { "op": "matches", "value": "@smoo\\.ai$" } }, "value": true })
    }

    #[test]
    fn args_are_validated() {
        assert_eq!(parse_org(&A.to_uppercase()).unwrap(), A);
        assert!(parse_org("acme").is_err());
        assert_eq!(parse_flag_key(" callVoicemailEnabled ").unwrap(), "callVoicemailEnabled");
        for bad in ["", "call-x", "a b", "x/../y"] {
            assert!(parse_flag_key(bad).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn add_to_an_unset_or_false_flag_creates_the_org_rule() {
        for current in [None, Some(Value::Null), Some(json!(false))] {
            let p = plan(current.as_ref(), A, Op::Add).unwrap();
            assert!(p.changed);
            assert_eq!(p.after, env(json!([org_rule("orgs", &[A])])));
        }
    }

    #[test]
    fn add_appends_to_the_existing_org_rule_and_keeps_everything_else() {
        let current = json!({ "$segment": true, "default": false, "description": "keep me",
            "rules": [org_rule("orgs", &[B]), staff_rule()] });
        let p = plan(Some(&current), A, Op::Add).unwrap();
        assert!(p.changed);
        assert_eq!(
            p.after,
            json!({ "$segment": true, "default": false, "description": "keep me", "rules": [org_rule("orgs", &[B, A]), staff_rule()] })
        );
    }

    #[test]
    fn add_when_the_org_is_already_there_changes_nothing() {
        let current = env(json!([org_rule("orgs", &[&A.to_uppercase(), B])]));
        let p = plan(Some(&current), A, Op::Add).unwrap();
        assert!(!p.changed);
        assert_eq!(p.after, current);
        assert!(p.notes[0].contains("already"));
    }

    #[test]
    fn add_with_only_other_rules_appends_a_new_org_rule_last_and_says_so() {
        let current = env(json!([staff_rule(), { "id": "orgs", "when": { "email": { "op": "eq", "value": "x@y.z" } }, "value": true }]));
        let p = plan(Some(&current), A, Op::Add).unwrap();
        let rules = p.after["rules"].as_array().unwrap();
        assert_eq!(rules.len(), 3);
        assert_eq!(rules[2], org_rule("orgs-2", &[A]), "a fresh id when `orgs` is taken");
        assert!(p.notes.iter().any(|n| n.contains("LAST")));
    }

    #[test]
    fn add_ignores_deny_and_multi_clause_org_rules() {
        let deny = json!({ "id": "deny", "when": { "orgId": { "op": "in", "value": [A] } }, "value": false });
        let both = json!({ "id": "both", "when": { "orgId": { "op": "in", "value": [C] }, "email": { "op": "eq", "value": "a@b.c" } }, "value": true });
        let p = plan(Some(&env(json!([deny, both]))), A, Op::Add).unwrap();
        assert_eq!(p.after["rules"], json!([deny, both, org_rule("orgs", &[A])]));
        // …but says the earlier deny still wins for this org.
        assert!(p.still_off);
        assert!(p.notes.iter().any(|n| n.contains("rule `deny`") && n.contains("stays OFF")), "{:?}", p.notes);
    }

    #[test]
    fn add_warns_only_about_a_deny_that_comes_first_and_names_the_org() {
        let deny_a = json!({ "id": "deny", "when": { "orgId": { "op": "eq", "value": A } }, "value": false });
        let deny_b = json!({ "id": "deny-b", "when": { "orgId": { "op": "in", "value": [B] } }, "value": false });
        // Deny after the allowlist: the allowlist wins, no warning.
        let p = plan(Some(&env(json!([org_rule("orgs", &[C]), deny_a]))), A, Op::Add).unwrap();
        assert!(!p.still_off, "{:?}", p.notes);
        // A deny for a different org: no warning.
        let p = plan(Some(&env(json!([deny_b, org_rule("orgs", &[C])]))), A, Op::Add).unwrap();
        assert!(!p.still_off);
        // Already listed but shadowed: unchanged, still warned.
        let p = plan(Some(&env(json!([deny_a, org_rule("orgs", &[A])]))), A, Op::Add).unwrap();
        assert!(!p.changed && p.still_off);
        // Remove never warns about it.
        let p = plan(Some(&env(json!([deny_a, org_rule("orgs", &[A, B])]))), A, Op::Remove).unwrap();
        assert!(!p.still_off);
    }

    #[test]
    fn rows_match_the_key_canonically_and_report_the_stored_spelling() {
        let rows = [
            json!({ "key": "callVoicemailEnabled", "value": true }),
            json!({ "key": "otherFlag", "value": false }),
        ];
        for typed in ["callVoicemailEnabled", "CALL_VOICEMAIL_ENABLED", "callvoicemailenabled"] {
            assert_eq!(
                match_row(&rows, typed).unwrap(),
                Some(("callVoicemailEnabled".to_string(), Some(json!(true)))),
                "{typed}"
            );
        }
        assert_eq!(match_row(&rows, "callDepartmentRoutingEnabled").unwrap(), None);
        let dup = [
            json!({ "key": "callVoicemailEnabled", "value": true }),
            json!({ "key": "CALL_VOICEMAIL_ENABLED", "value": false }),
        ];
        assert!(match_row(&dup, "callVoicemailEnabled")
            .unwrap_err()
            .to_string()
            .contains("more than one stored key"));
    }

    #[test]
    fn a_new_value_goes_under_the_schema_that_declares_the_flag() {
        let declares = |id: &str, flags: Value| json!({ "id": id, "jsonSchema": { "properties": { "featureFlagSchema": { "properties": flags } } } });
        let public = json!({ "id": "pub", "jsonSchema": { "properties": { "publicConfigSchema": { "properties": { "callVoicemailEnabled": {} } } } } });
        let other = declares("s1", json!({ "otherFlag": {} }));
        let calls = declares("s2", json!({ "callVoicemailEnabled": {} }));
        let schemas = [public.clone(), other, calls.clone()];
        let candidates: Vec<&Value> = schemas.iter().collect();
        assert_eq!(
            declaring_schemas(&candidates, "CALL_VOICEMAIL_ENABLED"),
            vec![("s2".to_string(), "callVoicemailEnabled".to_string())],
            "only a feature-flag declaration counts, and the declared name wins"
        );
        assert!(declaring_schemas(&[&public], "callVoicemailEnabled").is_empty());
        let twice = declares("s3", json!({ "callVoicemailEnabled": {} }));
        assert_eq!(declaring_schemas(&[&calls, &twice], "callVoicemailEnabled").len(), 2);
    }

    #[test]
    fn add_to_a_true_flag_changes_nothing() {
        let p = plan(Some(&json!(true)), A, Op::Add).unwrap();
        assert!(!p.changed);
        assert_eq!(p.after, json!(true));
    }

    #[test]
    fn remove_takes_the_org_out_and_keeps_the_rest() {
        let current = env(json!([org_rule("orgs", &[B, A, C]), staff_rule()]));
        let p = plan(Some(&current), A, Op::Remove).unwrap();
        assert!(p.changed);
        assert_eq!(p.after, env(json!([org_rule("orgs", &[B, C]), staff_rule()])));
        assert!(p.notes.iter().any(|n| n.contains("1 other rule")));
    }

    #[test]
    fn remove_of_the_last_org_drops_the_rule() {
        let p = plan(Some(&env(json!([org_rule("orgs", &[A])]))), A, Op::Remove).unwrap();
        assert!(p.changed);
        assert_eq!(p.after, env(json!([])));
        assert!(p.notes.iter().any(|n| n.contains("dropped")));
    }

    #[test]
    fn remove_handles_eq_rules_and_every_allowlist_naming_the_org() {
        let eq = json!({ "id": "one", "when": { "orgId": { "op": "eq", "value": A } }, "value": true });
        let p = plan(Some(&env(json!([eq, org_rule("orgs", &[A, B])]))), A, Op::Remove).unwrap();
        assert_eq!(p.after, env(json!([org_rule("orgs", &[B])])));
    }

    #[test]
    fn remove_of_an_absent_org_or_an_off_flag_changes_nothing() {
        let current = env(json!([org_rule("orgs", &[B])]));
        let p = plan(Some(&current), A, Op::Remove).unwrap();
        assert!(!p.changed);
        assert_eq!(p.after, current);
        for off in [None, Some(json!(false))] {
            assert!(!plan(off.as_ref(), A, Op::Remove).unwrap().changed);
        }
    }

    #[test]
    fn remove_warns_when_the_default_still_grants() {
        let current = json!({ "$segment": true, "default": true, "rules": [org_rule("orgs", &[A])] });
        let p = plan(Some(&current), A, Op::Remove).unwrap();
        assert!(p.notes.iter().any(|n| n.contains("default is `true`")));
    }

    #[test]
    fn remove_from_a_true_flag_is_refused() {
        let err = plan(Some(&json!(true)), A, Op::Remove).unwrap_err().to_string();
        assert!(err.contains("every org"), "{err}");
    }

    #[test]
    fn unknown_shapes_are_refused() {
        for bad in [
            json!("true"),
            json!(1),
            json!([A]),
            json!({ "rules": [org_rule("orgs", &[B])] }),
            json!({ "$segment": "yes", "rules": [] }),
            json!({ "$segment": true, "rules": "x" }),
            json!({ "$segment": true, "rules": ["x"] }),
            json!({ "$segment": true, "rules": [{ "id": "r", "value": true }] }),
            json!({ "$segment": true, "rules": [{ "id": "r", "when": {} }] }),
        ] {
            for op in [Op::Add, Op::Remove] {
                assert!(plan(Some(&bad), A, op).is_err(), "{bad} / {op:?} must be refused");
            }
        }
    }

    #[test]
    fn an_envelope_without_rules_gets_one() {
        let p = plan(Some(&json!({ "$segment": true, "default": false })), A, Op::Add).unwrap();
        assert_eq!(p.after, env(json!([org_rule("orgs", &[A])])));
    }

    #[test]
    fn rows_and_lists_accept_bare_and_data_envelopes() {
        assert_eq!(rows(&json!([{ "key": "a" }])).len(), 1);
        assert_eq!(list(&json!({ "data": [{ "id": "x" }, { "id": "y" }] })).len(), 2);
        assert!(rows(&json!({})).is_empty());
    }
}
