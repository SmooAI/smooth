//! `th branding …` — white-label a Smoo AI org, logo included, from the CLI.
//!
//! The platform keeps one white-label row per org at
//! `/organizations/{org}/branding`:
//!
//! | verb   | mode                        | notes                                    |
//! |--------|-----------------------------|------------------------------------------|
//! | GET    | native (api-prime)          | returns defaults when no row exists       |
//! | PUT    | native (api-prime)          | upsert; **absent field = column untouched** |
//! | DELETE | native (api-prime)          | drops the row, 204                        |
//! | PATCH  | 404                         | manifest routes it, no handler exists     |
//!
//! So PUT is the only write verb, and it is already partial-update — except
//! for `themeJson`, which is a single jsonb column that PUT **replaces
//! wholesale**. Every partial theme edit here is therefore a read-modify-write
//! over the current row (see [`merge_theme`]).
//!
//! `enabled` is the live switch. A row existing does NOT mean white-label is
//! on: `--apply` stages, `enable` goes live, and staged branding is viewable
//! at `…/apps?brandPreview=1`.
//!
//! # The Aurora tokens are not white-labelable
//!
//! `--color-heat-0..5`, `--color-ai`, `--gradient-aurora` and the ok/warn/crit
//! semantics encode MEANING, not chrome. They are structurally absent from the
//! theme schema and this command deliberately exposes no flags for them.
//!
//! # Known server-side gap (SMOODEV-2820)
//!
//! The write validators are still Phase 1. Both
//! `rust/api-prime/src/handlers/organization_branding.rs` (`ThemeOverride`,
//! `deny_unknown_fields`) and `packages/backend/src/routes/organization-branding.ts`
//! (`ThemeOverrideOpenApi`, `.strict()`) accept only the four accent tokens
//! plus the two deprecated `brand*` ones — so PUTting any Phase-2 surface
//! token (`background`, `card`, `sidebar`, …) is a 400 today, even though the
//! canonical Zod and the dashboard read path both support them. This module
//! sends the canonical set and [`explain_put_failure`] turns that 400 into a
//! diagnosis instead of a bare "bad request".

use std::io::IsTerminal;

// Every printer in this crate goes through `anstream` — owo-colors styles
// unconditionally, so a bare `print!` would leak escape soup into pipes (see
// `tests/no_ansi_when_piped.rs`).
use anstream::{eprintln, print, println};
use anyhow::{bail, Context, Result};
use clap::{Args, Subcommand};
use owo_colors::OwoColorize;
use serde::Deserialize;
use serde_json::{json, Map, Value};
use smooth_api_client::SmoothApiClient;

use super::{print_json, require_active_org, require_authed};

// ---------------------------------------------------------------------------
// Theme tokens
// ---------------------------------------------------------------------------

/// Phase-2 surface tokens — the ones the live write validators still reject.
/// Used only to explain a 400; the canonical schema has had them since
/// SMOODEV-1813.
const PHASE2_TOKENS: &[&str] = &[
    "colorScheme",
    "background",
    "foreground",
    "card",
    "border",
    "muted",
    "mutedForeground",
    "sidebar",
    "sidebarForeground",
];

/// WCAG AA for normal text. The gate on `enable` is deliberately the same
/// number for all three pairs — an unreadable partner dashboard is the exact
/// failure this command exists to prevent, and `--force` is there for the
/// cases where the operator knows better.
const CONTRAST_MIN: f64 = 4.5;

/// Dashboard origin for the preview link. Overridable so a local `next dev`
/// can be previewed too.
fn web_url() -> String {
    std::env::var("SMOOAI_WEB_URL").unwrap_or_else(|_| "https://smoo.ai".to_string())
}

fn preview_url() -> String {
    format!("{}/apps?brandPreview=1", web_url().trim_end_matches('/'))
}

// ---------------------------------------------------------------------------
// Command surface
// ---------------------------------------------------------------------------

/// The chrome tokens, as flags. Deliberately excludes the Aurora meaning
/// tokens (see the module docs) and the deprecated `brandSurface`/`brandMuted`.
#[derive(Args, Default)]
pub struct ThemeFlags {
    /// Base theme: `light` drops the dark class so light tokens apply.
    #[arg(long = "color-scheme", value_parser = ["light", "dark"])]
    color_scheme: Option<String>,
    /// `--primary` — the brand accent surface.
    #[arg(long)]
    primary: Option<String>,
    /// Text drawn on top of primary surfaces.
    #[arg(long = "primary-foreground")]
    primary_foreground: Option<String>,
    /// `--accent`.
    #[arg(long)]
    accent: Option<String>,
    /// Text drawn on top of accent surfaces.
    #[arg(long = "accent-foreground")]
    accent_foreground: Option<String>,
    /// `--background` — the page surface.
    #[arg(long)]
    background: Option<String>,
    /// `--foreground` — primary text on the page.
    #[arg(long)]
    foreground: Option<String>,
    /// `--card` / `--popover` — raised surfaces.
    #[arg(long)]
    card: Option<String>,
    /// `--border` / `--input` — hairlines and inputs.
    #[arg(long)]
    border: Option<String>,
    /// `--muted` — subtle fills.
    #[arg(long)]
    muted: Option<String>,
    /// `--muted-foreground` — secondary text.
    #[arg(long = "muted-foreground")]
    muted_foreground: Option<String>,
    /// `--sidebar` — the nav rail surface.
    #[arg(long)]
    sidebar: Option<String>,
    /// `--sidebar-foreground`.
    #[arg(long = "sidebar-foreground")]
    sidebar_foreground: Option<String>,
}

impl ThemeFlags {
    /// The tokens the caller actually passed, in schema order. An empty string
    /// clears the token (writes `null`).
    fn passed(&self) -> Vec<(&'static str, &str)> {
        [
            ("colorScheme", &self.color_scheme),
            ("primary", &self.primary),
            ("primaryForeground", &self.primary_foreground),
            ("accent", &self.accent),
            ("accentForeground", &self.accent_foreground),
            ("background", &self.background),
            ("foreground", &self.foreground),
            ("card", &self.card),
            ("border", &self.border),
            ("muted", &self.muted),
            ("mutedForeground", &self.muted_foreground),
            ("sidebar", &self.sidebar),
            ("sidebarForeground", &self.sidebar_foreground),
        ]
        .into_iter()
        .filter_map(|(k, v)| v.as_deref().map(|v| (k, v)))
        .collect()
    }
}

#[derive(Subcommand)]
#[allow(clippy::large_enum_variant)]
pub enum Cmd {
    /// Print the org's white-label row, marked LIVE or staged.
    Show {
        #[arg(long = "org-id", visible_alias = "org")]
        org: Option<String>,
        /// Print the raw API response instead of the rendered summary.
        #[arg(long)]
        json: bool,
    },
    /// Derive branding from a website. Dry-run unless `--apply`.
    FromUrl {
        /// The partner's site, e.g. `https://chakrasolutions.ai/`.
        url: String,
        #[arg(long = "org-id", visible_alias = "org")]
        org: Option<String>,
        /// Write the proposal to the branding row (staged, `enabled=false`).
        #[arg(long)]
        apply: bool,
        /// Write AND flip white-label live. Implies `--apply`. Refuses on a
        /// failing contrast report unless `--force`.
        #[arg(long)]
        enable: bool,
        /// Skip re-hosting the logo candidates (colors + app name only).
        #[arg(long = "no-logo")]
        no_logo: bool,
        /// Use this logo instead of the extractor's pick (path or URL).
        #[arg(long)]
        logo: Option<String>,
        /// Use this dark logo instead of the extractor's pick (path or URL).
        #[arg(long = "logo-dark")]
        logo_dark: Option<String>,
        /// Use this favicon instead of the extractor's pick (path or URL).
        #[arg(long)]
        favicon: Option<String>,
        /// Go live even though the contrast check fails.
        #[arg(long)]
        force: bool,
        /// Print the raw propose response instead of the rendered summary.
        #[arg(long)]
        json: bool,
    },
    /// Set individual fields. Only the flags you pass are written.
    Set {
        #[command(flatten)]
        theme: ThemeFlags,
        #[arg(long = "org-id", visible_alias = "org")]
        org: Option<String>,
        /// Display name replacing "Smoo AI". Pass an empty string to clear.
        #[arg(long = "app-name")]
        app_name: Option<String>,
        /// Light-mode logo — a local path or a remote URL (re-hosted on our CDN).
        #[arg(long)]
        logo: Option<String>,
        /// Dark-mode logo — a local path or a remote URL (re-hosted on our CDN).
        #[arg(long = "logo-dark")]
        logo_dark: Option<String>,
        /// Favicon — a local path or a remote URL (re-hosted on our CDN).
        #[arg(long)]
        favicon: Option<String>,
        /// Override the in-app Help / Support URL. Empty string clears it.
        #[arg(long = "support-url")]
        support_url: Option<String>,
        /// Hide the "Powered by Smoo AI" footer on the chat widget.
        #[arg(long = "hide-powered-by")]
        hide_powered_by: Option<bool>,
    },
    /// Flip white-label LIVE. Refuses on failing contrast unless `--force`.
    Enable {
        #[arg(long = "org-id", visible_alias = "org")]
        org: Option<String>,
        /// Go live even though the contrast check fails.
        #[arg(long)]
        force: bool,
    },
    /// Turn white-label off. The row (and its colors) is kept, just not applied.
    Disable {
        #[arg(long = "org-id", visible_alias = "org")]
        org: Option<String>,
    },
    /// Print the URL that renders staged branding without going live.
    Preview {
        #[arg(long = "org-id", visible_alias = "org")]
        org: Option<String>,
    },
    /// Delete the branding row entirely (back to Smoo AI defaults).
    Clear {
        #[arg(long = "org-id", visible_alias = "org")]
        org: Option<String>,
        /// Skip the confirmation prompt.
        #[arg(long)]
        yes: bool,
    },
}

// ---------------------------------------------------------------------------
// Dispatch
// ---------------------------------------------------------------------------

#[allow(clippy::too_many_lines)]
pub async fn cmd(cmd: Cmd) -> Result<()> {
    // `require_authed` prefers the USER session and only falls back to M2M.
    // That matters here: the M2M session is org-locked, so a branding write
    // against an org you administer but whose client you don't hold 403s.
    // `th auth login` is the fix, and the error below says so.
    let client = require_authed().await?;
    match cmd {
        Cmd::Show { org, json } => {
            let o = require_active_org(&client, org)?;
            let row = fetch_branding(&client, &o).await?;
            if json {
                print_json(&row);
            } else {
                print_branding(&o, &row);
            }
        }
        Cmd::FromUrl {
            url,
            org,
            apply,
            enable,
            no_logo,
            logo,
            logo_dark,
            favicon,
            force,
            json,
        } => {
            let o = require_active_org(&client, org)?;
            let opts = FromUrlOpts {
                apply: apply || enable,
                enable,
                no_logo,
                force,
                as_json: json,
                logo,
                logo_dark,
                favicon,
            };
            from_url(&client, &o, &url, &opts).await?;
        }
        Cmd::Set {
            theme,
            org,
            app_name,
            logo,
            logo_dark,
            favicon,
            support_url,
            hide_powered_by,
        } => {
            let o = require_active_org(&client, org)?;
            let mut body = Map::new();
            if let Some(v) = app_name {
                body.insert("appName".into(), nullable(&v));
            }
            if let Some(v) = support_url {
                body.insert("supportUrl".into(), nullable(&v));
            }
            if let Some(v) = hide_powered_by {
                body.insert("hidePoweredBy".into(), json!(v));
            }

            // Dark first: its upload borrows the org's `logo` column and puts
            // it back, so a light upload in the same run must land after.
            if let Some(src) = logo_dark {
                let url = resolve_asset(&client, &o, Slot::LogoDark, &src).await?;
                body.insert("logoDarkUrl".into(), json!(url));
            }
            if let Some(src) = logo {
                let url = resolve_asset(&client, &o, Slot::Logo, &src).await?;
                body.insert("logoUrl".into(), json!(url));
            }
            if let Some(src) = favicon {
                let url = resolve_asset(&client, &o, Slot::Favicon, &src).await?;
                body.insert("faviconUrl".into(), json!(url));
            }

            let tokens = theme.passed();
            if !tokens.is_empty() {
                // PUT replaces theme_json wholesale — merge over the current row.
                let current = fetch_branding(&client, &o).await?;
                body.insert("themeJson".into(), merge_theme(current.get("themeJson"), &tokens));
            }
            if body.is_empty() {
                bail!("nothing to set — pass at least one flag (see `th branding set --help`)");
            }
            let row = put_branding(&client, &o, &Value::Object(body)).await?;
            print_branding(&o, &row);
        }
        Cmd::Enable { org, force } => {
            let o = require_active_org(&client, org)?;
            let row = fetch_branding(&client, &o).await?;
            let verdict = contrast_verdict(row.get("themeJson"));
            print_contrast(&verdict, None);
            if !verdict.passes() && !force {
                bail!(
                    "refusing to go live — the theme fails the WCAG AA {CONTRAST_MIN}:1 contrast floor. \
                     Fix the colors with `th branding set`, or pass --force if you know better."
                );
            }
            let row = put_branding(&client, &o, &json!({ "enabled": true })).await?;
            print_branding(&o, &row);
        }
        Cmd::Disable { org } => {
            let o = require_active_org(&client, org)?;
            let row = put_branding(&client, &o, &json!({ "enabled": false })).await?;
            print_branding(&o, &row);
        }
        Cmd::Preview { org } => {
            let o = require_active_org(&client, org)?;
            let row = fetch_branding(&client, &o).await?;
            println!();
            println!("  {} {}", "Preview:".dimmed(), preview_url().cyan().bold());
            if flag(&row, "enabled") {
                println!("  {} branding is already LIVE for this org", "●".dimmed());
            }
            println!();
        }
        Cmd::Clear { org, yes } => {
            let o = require_active_org(&client, org)?;
            if !yes {
                if !std::io::stdin().is_terminal() {
                    bail!("refusing to delete the branding row without confirmation — pass --yes");
                }
                let ok = dialoguer::Confirm::with_theme(&dialoguer::theme::ColorfulTheme::default())
                    .with_prompt(format!("Delete the white-label branding row for org {o}?"))
                    .default(false)
                    .interact()?;
                if !ok {
                    println!();
                    println!("  {} cancelled", "●".dimmed());
                    println!();
                    return Ok(());
                }
            }
            client.delete(&branding_path(&o)).await.context("DELETE branding")?;
            println!();
            println!("  {} Branding cleared for {}", "✓".green().bold(), o.cyan());
            println!();
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// API helpers
// ---------------------------------------------------------------------------

fn branding_path(org: &str) -> String {
    format!("/organizations/{org}/branding")
}

async fn fetch_branding(client: &SmoothApiClient, org: &str) -> Result<Value> {
    client.get(&branding_path(org)).await.context("GET branding")
}

/// PUT the branding row, turning the two failure modes that actually happen
/// into something actionable.
async fn put_branding(client: &SmoothApiClient, org: &str, body: &Value) -> Result<Value> {
    client
        .put(&branding_path(org), body)
        .await
        .map_err(|e| explain_put_failure(&e.to_string(), body))
}

/// Map a raw PUT error onto the cause. The stale-schema 400 is the one that
/// will bite until the server's theme validator catches up with the canonical
/// Zod (see the module docs); a 403 is almost always the org-locked M2M token.
///
/// Deliberately points at *what* the server doesn't understand, not at which
/// file to edit — the files move, the symptom doesn't.
fn explain_put_failure(err: &str, body: &Value) -> anyhow::Error {
    if err.contains("HTTP 400") {
        let offenders: Vec<&str> = body
            .get("themeJson")
            .and_then(Value::as_object)
            .map(|t| PHASE2_TOKENS.iter().copied().filter(|k| t.contains_key(*k)).collect())
            .unwrap_or_default();
        if !offenders.is_empty() {
            return anyhow::anyhow!(
                "{err}\n\nthe server's theme schema predates the token(s) you set: {}. \
                 It still accepts only the accent layer (primary / primaryForeground / accent / accentForeground), \
                 while the canonical schema and the dashboard's read path carry the full surface layer. \
                 Set only the accent tokens until the write validator is widened.",
                offenders.join(", ")
            );
        }
    }
    if err.contains("HTTP 403") {
        return anyhow::anyhow!("{err}\n\nthe M2M session is org-locked; run `th auth login` so branding writes ride your user JWT (which acts cross-org).");
    }
    anyhow::anyhow!("{err}")
}

/// `""` clears a nullable string field; anything else sets it.
fn nullable(v: &str) -> Value {
    if v.is_empty() {
        Value::Null
    } else {
        json!(v)
    }
}

fn flag(row: &Value, key: &str) -> bool {
    row.get(key).and_then(Value::as_bool).unwrap_or(false)
}

fn text<'a>(row: &'a Value, key: &str) -> Option<&'a str> {
    row.get(key).and_then(Value::as_str)
}

/// Layer `updates` over the row's existing `themeJson`. PUT replaces the whole
/// jsonb column, so without this a `set --primary` would silently drop every
/// other token the partner had configured.
fn merge_theme(current: Option<&Value>, updates: &[(&str, &str)]) -> Value {
    let mut theme = current.and_then(Value::as_object).cloned().unwrap_or_default();
    for (k, v) in updates {
        theme.insert((*k).to_string(), nullable(v));
    }
    // Emit only keys that carry a value. Because PUT replaces the whole column,
    // an absent key and a null one read identically — so dropping nulls clears
    // a token just as well while keeping the payload to what's actually set.
    // That matters beyond tidiness: a strict server rejects on the KEY, not the
    // value, so a row already holding null-valued keys would otherwise poison
    // every later write (SMOODEV-2822 — the dashboard emits exactly that).
    theme.retain(|_, v| !v.is_null());
    Value::Object(theme)
}

// ---------------------------------------------------------------------------
// Contrast
// ---------------------------------------------------------------------------

/// One evaluated foreground/background pair.
struct Pair {
    label: &'static str,
    ratio: Option<f64>,
}

struct Verdict {
    pairs: Vec<Pair>,
    warnings: Vec<String>,
}

impl Verdict {
    /// A pair we could not evaluate (token unset, or a non-hex CSS color) is
    /// not a failure — only a computed ratio below the floor is.
    fn passes(&self) -> bool {
        self.pairs.iter().all(|p| p.ratio.is_none_or(|r| r >= CONTRAST_MIN))
    }
}

/// sRGB channel → linear, per WCAG 2.x relative luminance.
fn linearize(c: f64) -> f64 {
    if c <= 0.039_28 {
        c / 12.92
    } else {
        ((c + 0.055) / 1.055).powf(2.4)
    }
}

/// `#rgb`, `#rrggbb`, `#rrggbbaa` → sRGB 0..1. Alpha is dropped (we can't
/// composite without knowing what's underneath). Any other CSS color form
/// (`oklch(...)`, `hsl(...)`, named) returns `None` — see [`Verdict::passes`].
fn parse_hex(s: &str) -> Option<[f64; 3]> {
    let hex = s.trim().strip_prefix('#')?;
    if !hex.chars().all(|c| c.is_ascii_hexdigit()) {
        return None;
    }
    let bytes: Vec<u8> = match hex.len() {
        3 => hex.chars().map(|c| u8::from_str_radix(&format!("{c}{c}"), 16).unwrap_or(0)).collect(),
        6 | 8 => (0..3).filter_map(|i| u8::from_str_radix(&hex[i * 2..i * 2 + 2], 16).ok()).collect(),
        _ => return None,
    };
    if bytes.len() != 3 {
        return None;
    }
    Some([f64::from(bytes[0]) / 255.0, f64::from(bytes[1]) / 255.0, f64::from(bytes[2]) / 255.0])
}

fn luminance(rgb: [f64; 3]) -> f64 {
    0.0722f64.mul_add(linearize(rgb[2]), 0.7152f64.mul_add(linearize(rgb[1]), 0.2126 * linearize(rgb[0])))
}

fn contrast_ratio(a: [f64; 3], b: [f64; 3]) -> f64 {
    let (la, lb) = (luminance(a), luminance(b));
    let (hi, lo) = if la >= lb { (la, lb) } else { (lb, la) };
    (hi + 0.05) / (lo + 0.05)
}

/// Evaluate the same three pairs the propose endpoint reports, locally, from a
/// `themeJson` object. Used by `enable` (which has no server report to lean on)
/// and as the fallback when `from-url` gets a response without one.
fn contrast_verdict(theme: Option<&Value>) -> Verdict {
    let obj = theme.and_then(Value::as_object);
    let get = |k: &str| obj.and_then(|t| t.get(k)).and_then(Value::as_str).filter(|s| !s.is_empty());

    let mut pairs = Vec::new();
    let mut warnings = Vec::new();
    for (label, fg_key, bg_key) in [
        ("foreground on background", "foreground", "background"),
        ("primaryForeground on primary", "primaryForeground", "primary"),
        ("mutedForeground on background", "mutedForeground", "background"),
    ] {
        let (Some(fg), Some(bg)) = (get(fg_key), get(bg_key)) else {
            // Token unset → the Smoo default applies, which we know is legible.
            continue;
        };
        if let (Some(f), Some(b)) = (parse_hex(fg), parse_hex(bg)) {
            pairs.push(Pair {
                label,
                ratio: Some(contrast_ratio(f, b)),
            });
        } else {
            warnings.push(format!("{label}: not evaluated (non-hex color)"));
            pairs.push(Pair { label, ratio: None });
        }
    }
    Verdict { pairs, warnings }
}

fn print_contrast(verdict: &Verdict, server: Option<&Contrast>) {
    println!();
    println!("  {}", "Contrast".bold());
    // The server omits a ratio it couldn't derive, so `passes: true` with zero
    // ratios is vacuous — an empty proposal trivially "passes". Never paint a
    // green check over nothing; say what wasn't measured and let the local
    // verdict below speak instead.
    if let Some(c) = server.filter(|c| !c.measured().is_empty()) {
        for (label, r) in c.measured() {
            print_ratio(label, Some(r));
        }
        for w in &c.warnings {
            println!("    {} {}", "!".yellow(), w.yellow());
        }
        match c.passes {
            Some(true) => println!("    {} passes", "✓".green().bold()),
            Some(false) => println!("    {} FAILS", "✗".red().bold()),
            None => println!("    {} no verdict reported", "●".dimmed()),
        }
        println!();
        return;
    }
    if server.is_some() {
        println!(
            "    {} {}",
            "●".dimmed(),
            "the extractor measured no color pairs — its verdict is vacuous".dimmed()
        );
    }
    if verdict.pairs.is_empty() {
        println!("    {} nothing to check — the theme keeps the Smoo defaults", "●".dimmed());
        println!();
        return;
    }
    for p in &verdict.pairs {
        print_ratio(p.label, p.ratio);
    }
    for w in &verdict.warnings {
        println!("    {} {}", "!".yellow(), w.yellow());
    }
    if verdict.passes() {
        println!("    {} passes", "✓".green().bold());
    } else {
        println!("    {} FAILS the {CONTRAST_MIN}:1 floor", "✗".red().bold());
    }
    println!();
}

fn print_ratio(label: &str, ratio: Option<f64>) {
    match ratio {
        Some(r) if r >= CONTRAST_MIN => println!("    {} {label} {}", "✓".green(), format!("{r:.1}:1").bold()),
        Some(r) => println!("    {} {label} {}", "✗".red(), format!("{r:.1}:1").red().bold()),
        None => println!("    {} {label} {}", "?".dimmed(), "not evaluated".dimmed()),
    }
}

// ---------------------------------------------------------------------------
// Rendering
// ---------------------------------------------------------------------------

/// A colored block for a hex token, so `show` reads as a swatch table rather
/// than a wall of hex.
#[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss, reason = "channels are clamped to 0..1 by parse_hex")]
fn swatch(value: &str) -> String {
    parse_hex(value).map_or_else(
        || "   ".to_string(),
        |rgb| {
            "███"
                .truecolor((rgb[0] * 255.0) as u8, (rgb[1] * 255.0) as u8, (rgb[2] * 255.0) as u8)
                .to_string()
        },
    )
}

fn print_theme(theme: Option<&Value>) {
    let Some(obj) = theme.and_then(Value::as_object).filter(|o| !o.is_empty()) else {
        println!("  {} {}", "Theme:".dimmed(), "default".dimmed());
        return;
    };
    println!("  {}", "Theme".bold());
    for (k, v) in obj {
        let Some(s) = v.as_str().filter(|s| !s.is_empty()) else { continue };
        println!("    {} {:<20} {}", swatch(s), k.dimmed(), s);
    }
}

fn print_branding(org: &str, row: &Value) {
    let enabled = flag(row, "enabled");
    let configured = enabled
        || text(row, "appName").is_some()
        || text(row, "logoUrl").is_some()
        || text(row, "logoDarkUrl").is_some()
        || text(row, "faviconUrl").is_some()
        || text(row, "supportUrl").is_some()
        || flag(row, "hidePoweredBy")
        || row.get("themeJson").is_some_and(|t| !t.is_null());

    println!();
    print!("  {} {}  ", "Org".dimmed(), org.cyan());
    if enabled {
        println!("{}", " LIVE ".on_green().black().bold());
    } else if configured {
        println!("{}", " staged (not live) ".on_yellow().black().bold());
    } else {
        println!("{}", "not configured".dimmed());
    }
    println!();

    for (label, key) in [
        ("App name", "appName"),
        ("Logo", "logoUrl"),
        ("Logo (dark)", "logoDarkUrl"),
        ("Favicon", "faviconUrl"),
        ("Support URL", "supportUrl"),
    ] {
        if let Some(v) = text(row, key) {
            println!("  {:<12} {}", format!("{label}:").dimmed(), v);
        }
    }
    if flag(row, "hidePoweredBy") {
        println!("  {:<12} {}", "Powered by:".dimmed(), "hidden");
    }
    print_theme(row.get("themeJson"));

    if configured && !enabled {
        println!();
        println!("  {} {}", "Preview:".dimmed(), preview_url().cyan().bold());
        println!("  {} {}", "Go live:".dimmed(), "th branding enable".dimmed());
    }
    println!();
}

// ---------------------------------------------------------------------------
// from-url — the propose endpoint (SMOODEV-2820 Lane A)
// ---------------------------------------------------------------------------

/// `POST /organizations/{org}/branding/propose` response. Every field is
/// optional: the extractor omits what it can't derive, and unknown keys are
/// tolerated so a server-side addition doesn't break an older `th`.
#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Proposal {
    #[serde(default)]
    theme: Option<Value>,
    #[serde(default)]
    app_name: Option<String>,
    #[serde(default)]
    logo_candidates: Vec<LogoCandidate>,
    #[serde(default)]
    contrast: Option<Contrast>,
    #[serde(default)]
    notes: Vec<String>,
}

#[derive(Debug, Deserialize)]
struct LogoCandidate {
    url: String,
    /// `logo` | `logoDark` | `favicon`.
    kind: String,
    #[serde(default)]
    source: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Contrast {
    #[serde(default)]
    foreground_on_background: Option<f64>,
    #[serde(default)]
    primary_foreground_on_primary: Option<f64>,
    #[serde(default)]
    muted_foreground_on_background: Option<f64>,
    #[serde(default)]
    passes: Option<bool>,
    #[serde(default)]
    warnings: Vec<String>,
}

impl Proposal {
    /// First candidate of each kind. The extractor emits them best-first and
    /// can return several per kind (a real run returned the wordmark PNG *and*
    /// the `og:image` page graphic, both as `logo`), so the pick is shown in
    /// the output and overridable with `--logo` / `--logo-dark` / `--favicon`.
    fn candidate(&self, kind: &str) -> Option<&LogoCandidate> {
        self.logo_candidates.iter().find(|c| c.kind == kind)
    }
}

impl Contrast {
    /// The ratios the extractor actually derived. Absent (not null) is how it
    /// reports "couldn't measure this pair", so an empty result means the
    /// `passes` flag has nothing behind it.
    fn measured(&self) -> Vec<(&'static str, f64)> {
        [
            ("foreground on background", self.foreground_on_background),
            ("primaryForeground on primary", self.primary_foreground_on_primary),
            ("mutedForeground on background", self.muted_foreground_on_background),
        ]
        .into_iter()
        .filter_map(|(label, r)| r.map(|r| (label, r)))
        .collect()
    }
}

/// Does the report say "do not ship this"? Absent verdict = not a refusal;
/// the local check in [`contrast_verdict`] is the backstop.
fn server_refuses(contrast: Option<&Contrast>) -> bool {
    contrast.is_some_and(|c| c.passes == Some(false))
}

/// The `--enable` gate, as a decision on its own so it can be tested without a
/// network. Either verdict failing is enough to stop: the server's report is
/// authoritative when present, and the local check covers a response that
/// carries none. `--force` is the only way past.
fn refuse_to_go_live(contrast: Option<&Contrast>, theme: Option<&Value>, force: bool) -> bool {
    !force && (server_refuses(contrast) || !contrast_verdict(theme).passes())
}

/// Everything `from-url` does beyond "fetch the proposal".
#[derive(Default)]
#[allow(clippy::struct_excessive_bools, reason = "these are the command's flags, one field per flag")]
struct FromUrlOpts {
    apply: bool,
    enable: bool,
    no_logo: bool,
    force: bool,
    as_json: bool,
    logo: Option<String>,
    logo_dark: Option<String>,
    favicon: Option<String>,
}

impl FromUrlOpts {
    /// The operator's override for a slot, if they passed one.
    fn override_for(&self, slot: Slot) -> Option<&str> {
        match slot {
            Slot::Logo => self.logo.as_deref(),
            Slot::LogoDark => self.logo_dark.as_deref(),
            Slot::Favicon => self.favicon.as_deref(),
        }
    }
}

async fn from_url(client: &SmoothApiClient, org: &str, url: &str, opts: &FromUrlOpts) -> Result<()> {
    let FromUrlOpts {
        apply,
        enable,
        no_logo,
        force,
        as_json,
        ..
    } = *opts;
    let raw = client
        .post(&format!("/organizations/{org}/branding/propose"), Some(&json!({ "url": url })))
        .await
        .map_err(|e| {
            if e.to_string().contains("HTTP 404") {
                anyhow::anyhow!("{e}\n\nthe branding propose endpoint isn't deployed yet (SMOODEV-2820 Lane A). Everything else in `th branding` works today.")
            } else {
                e
            }
        })?;

    if as_json {
        print_json(&raw);
        if !apply {
            return Ok(());
        }
    }

    let proposal: Proposal = serde_json::from_value(raw.clone()).context("parse propose response")?;

    if !as_json {
        println!();
        println!("  {} {}", "Proposed branding from".dimmed(), url.cyan().bold());
        if let Some(name) = &proposal.app_name {
            println!("  {:<12} {}", "App name:".dimmed(), name.bold());
        }
        print_theme(proposal.theme.as_ref());
        print_candidates(&proposal, opts);
        print_contrast(&contrast_verdict(proposal.theme.as_ref()), proposal.contrast.as_ref());
        for n in &proposal.notes {
            println!("  {} {}", "●".dimmed(), n.dimmed());
        }
    }

    if !apply {
        println!();
        println!("  {} {}", "Dry run.".dimmed(), "Re-run with --apply to stage it, --enable to go live.".dimmed());
        println!();
        return Ok(());
    }

    // The gate. An unreadable partner dashboard is the failure mode this whole
    // command exists to prevent, so the refusal is on the way IN, before a
    // single byte is written — not after.
    if enable && refuse_to_go_live(proposal.contrast.as_ref(), proposal.theme.as_ref(), force) {
        bail!(
            "refusing to go live — the proposed theme fails the WCAG AA {CONTRAST_MIN}:1 contrast floor. \
             Re-run without --enable to stage it, fix the colors with `th branding set`, or pass --force."
        );
    }

    let mut body = Map::new();
    if let Some(name) = &proposal.app_name {
        body.insert("appName".into(), json!(name));
    }
    if let Some(theme) = proposal.theme.as_ref().and_then(Value::as_object) {
        let current = fetch_branding(client, org).await?;
        let tokens: Vec<(&str, &str)> = theme.iter().filter_map(|(k, v)| v.as_str().map(|s| (k.as_str(), s))).collect();
        body.insert("themeJson".into(), merge_theme(current.get("themeJson"), &tokens));
    }

    if !no_logo {
        // Dark first — see the note in `Cmd::Set`.
        for (kind, field, slot) in [
            ("logoDark", "logoDarkUrl", Slot::LogoDark),
            ("logo", "logoUrl", Slot::Logo),
            ("favicon", "faviconUrl", Slot::Favicon),
        ] {
            // An explicit --logo/--logo-dark/--favicon beats the extractor's
            // pick. Worth having: the first `logo` candidate is sometimes the
            // page's og:image (a screenshot) rather than the mark.
            let Some(src) = opts.override_for(slot).or_else(|| proposal.candidate(kind).map(|c| c.url.as_str())) else {
                continue;
            };
            // A single unusable candidate (an .ico favicon, a 404, a host that
            // resolves private) must not sink the whole apply — the colors are
            // still worth writing.
            match resolve_asset(client, org, slot, src).await {
                Ok(hosted) => {
                    body.insert(field.into(), json!(hosted));
                }
                Err(e) => eprintln!("  {} skipped {kind} ({}): {e}", "!".yellow(), src.dimmed()),
            }
        }
    }

    if enable {
        body.insert("enabled".into(), json!(true));
    }
    if body.is_empty() {
        bail!("the proposal had nothing to write");
    }
    let row = put_branding(client, org, &Value::Object(body)).await?;
    print_branding(org, &row);
    Ok(())
}

/// List the candidates, marking the one that would actually be used for each
/// slot. The extractor can return several per kind and the first isn't always
/// the mark — showing the pick is what makes the `--logo` override discoverable.
fn print_candidates(proposal: &Proposal, opts: &FromUrlOpts) {
    for (kind, slot) in [("logo", Slot::Logo), ("logoDark", Slot::LogoDark), ("favicon", Slot::Favicon)] {
        if let Some(src) = opts.override_for(slot) {
            println!("  {} {:<10} {} {}", "→".green().bold(), kind.bold(), src, "(--override)".dimmed());
        }
    }
    if proposal.logo_candidates.is_empty() {
        println!("  {} {}", "Logos:".dimmed(), "none found".dimmed());
        return;
    }
    println!("  {}", "Logo candidates".bold());
    let mut seen: Vec<&str> = Vec::new();
    for c in &proposal.logo_candidates {
        let overridden = Slot::for_kind(&c.kind).is_some_and(|s| opts.override_for(s).is_some());
        let picked = !overridden && !seen.contains(&c.kind.as_str());
        seen.push(&c.kind);
        let src = c.source.as_deref().unwrap_or("");
        let marker = if picked {
            "→".green().bold().to_string()
        } else {
            "○".dimmed().to_string()
        };
        println!("    {marker} {:<10} {} {}", c.kind.bold(), c.url, format!("({src})").dimmed());
    }
}

// ---------------------------------------------------------------------------
// Logo pipeline
// ---------------------------------------------------------------------------

/// What the asset is for. Distinct from the upload endpoint's `variant`,
/// which has no dark slot — see [`Slot::variant`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Slot {
    Logo,
    LogoDark,
    Favicon,
}

impl Slot {
    /// The propose endpoint's `kind` → the slot it fills.
    fn for_kind(kind: &str) -> Option<Self> {
        match kind {
            "logo" => Some(Self::Logo),
            "logoDark" => Some(Self::LogoDark),
            "favicon" => Some(Self::Favicon),
            _ => None,
        }
    }

    fn label(self) -> &'static str {
        match self {
            Self::Logo => "logo",
            Self::LogoDark => "logo-dark",
            Self::Favicon => "favicon",
        }
    }

    /// The `variant` the platform upload endpoint understands. It has three:
    /// `logo` → `organizations.logo`, `icon` → `organizations.icon`,
    /// `logoWordmark` → `organizations.logoWordmark`. There is no dark slot,
    /// and `logoWordmark` is load-bearing (it renders in proposals), so the
    /// dark variant borrows `logo` and [`upload_asset`] puts the column back.
    fn variant(self) -> &'static str {
        match self {
            Self::Logo | Self::LogoDark => "logo",
            Self::Favicon => "icon",
        }
    }
}

/// The upload endpoint's cap (`maxSize: 5 * 1024 * 1024`). Enforced here too so
/// a 20MB logo fails on the local read instead of after a slow upload.
const MAX_ASSET_BYTES: usize = 5 * 1024 * 1024;

/// Take a local path or a remote URL and return a URL on OUR CDN. A partner's
/// own server is never left as the source of truth for their logo.
async fn resolve_asset(client: &SmoothApiClient, org: &str, slot: Slot, src: &str) -> Result<String> {
    let bytes = if src.starts_with("http://") || src.starts_with("https://") {
        fetch_remote(src).await?
    } else {
        let bytes = std::fs::read(src).with_context(|| format!("read {src}"))?;
        if bytes.len() > MAX_ASSET_BYTES {
            bail!("{src} is {} bytes; the platform caps uploads at {MAX_ASSET_BYTES}", bytes.len());
        }
        bytes
    };
    let mime = sniff_image(&bytes).with_context(|| format!("--{}: {src} is not an image the platform accepts", slot.label()))?;
    upload_asset(client, org, slot, mime, bytes).await
}

/// Fetch a user-supplied URL with the same care `vetUrl` takes server-side:
/// http(s) only, no private / loopback / link-local hosts (169.254.169.254 is
/// the cloud metadata endpoint), no redirect following, and a hard byte cap.
async fn fetch_remote(raw: &str) -> Result<Vec<u8>> {
    let url = vet_url(raw)?;
    let http = reqwest::Client::builder()
        .user_agent(format!("th/{}", env!("CARGO_PKG_VERSION")))
        .redirect(reqwest::redirect::Policy::none())
        .timeout(std::time::Duration::from_secs(30))
        .build()?;
    let resp = http.get(url).send().await.with_context(|| format!("GET {raw}"))?;
    let status = resp.status();
    if status.is_redirection() {
        // Following blindly is how a vetted public host becomes a request to
        // 169.254.169.254. Make the operator re-point at the final URL.
        let to = resp
            .headers()
            .get(reqwest::header::LOCATION)
            .and_then(|v| v.to_str().ok())
            .unwrap_or("(no Location)");
        bail!("{raw} redirected to {to} — pass the final URL directly (redirects are not followed)");
    }
    if !status.is_success() {
        bail!("GET {raw} returned HTTP {status}");
    }
    if let Some(len) = resp.content_length() {
        if len > MAX_ASSET_BYTES as u64 {
            bail!("{raw} is {len} bytes; the platform caps uploads at {MAX_ASSET_BYTES}");
        }
    }
    let bytes = resp.bytes().await.with_context(|| format!("read body of {raw}"))?;
    if bytes.len() > MAX_ASSET_BYTES {
        // Servers lie about (or omit) Content-Length.
        bail!("{raw} is {} bytes; the platform caps uploads at {MAX_ASSET_BYTES}", bytes.len());
    }
    Ok(bytes.to_vec())
}

/// Reject non-public URLs to limit SSRF blast radius — mirrors the monorepo's
/// `vetUrl` in `packages/backend/src/services/brand-palette/extract-palette.ts`.
#[allow(
    clippy::case_sensitive_file_extension_comparisons,
    reason = "these are hostname suffixes, not filenames; host is already lowercased"
)]
fn vet_url(raw: &str) -> Result<reqwest::Url> {
    let url = reqwest::Url::parse(raw).with_context(|| format!("{raw} is not a valid URL"))?;
    if !matches!(url.scheme(), "http" | "https") {
        bail!("only http(s) URLs are supported, got `{}`", url.scheme());
    }
    let host = url.host_str().unwrap_or_default().to_ascii_lowercase();
    let private = host == "localhost"
        || host.ends_with(".local")
        || host.ends_with(".internal")
        || host == "::1"
        || host == "[::1]"
        || host == "0.0.0.0"
        || host.starts_with("127.")
        || host.starts_with("10.")
        || host.starts_with("192.168.")
        || host.starts_with("169.254.")
        || is_private_172(&host);
    if private {
        bail!("refusing to fetch a private / loopback host (`{host}`)");
    }
    Ok(url)
}

/// `172.16.0.0/12` — i.e. `172.16.` through `172.31.`.
fn is_private_172(host: &str) -> bool {
    host.strip_prefix("172.")
        .and_then(|rest| rest.split('.').next())
        .and_then(|octet| octet.parse::<u8>().ok())
        .is_some_and(|o| (16..=31).contains(&o))
}

/// Magic-byte sniff, restricted to what the platform's `allowedMimes` accepts.
/// The sniffed type is what we send as the part's Content-Type, so the server's
/// `expectedMimeType` cross-check can never disagree with the bytes.
fn sniff_image(bytes: &[u8]) -> Result<&'static str> {
    if bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
        return Ok("image/png");
    }
    if bytes.starts_with(&[0xFF, 0xD8, 0xFF]) {
        return Ok("image/jpeg");
    }
    if bytes.starts_with(b"GIF87a") || bytes.starts_with(b"GIF89a") {
        return Ok("image/gif");
    }
    if bytes.len() >= 12 && bytes.starts_with(b"RIFF") && &bytes[8..12] == b"WEBP" {
        return Ok("image/webp");
    }
    if looks_like_svg(bytes) {
        return Ok("image/svg+xml");
    }
    if bytes.starts_with(&[0x00, 0x00, 0x01, 0x00]) {
        bail!("that's an .ico — the platform's logo upload accepts png / jpeg / gif / webp / svg only. Point at a PNG or SVG favicon instead.");
    }
    bail!("unrecognized image format (png / jpeg / gif / webp / svg supported)")
}

fn looks_like_svg(bytes: &[u8]) -> bool {
    let head = String::from_utf8_lossy(&bytes[..bytes.len().min(1024)]);
    let head = head.trim_start();
    (head.starts_with("<?xml") || head.starts_with("<!DOCTYPE") || head.starts_with('<')) && head.contains("<svg")
}

fn extension_for(mime: &str) -> &'static str {
    match mime {
        "image/jpeg" => "jpg",
        "image/gif" => "gif",
        "image/webp" => "webp",
        "image/svg+xml" => "svg",
        _ => "png",
    }
}

/// Upload bytes to the org's brand assets and return the public HTTPS URL.
///
/// `POST /organizations/{org}/logo/upload` (multipart: `file` + `variant`) is
/// the only endpoint that puts brand images in the PUBLIC-read published-assets
/// bucket — the presigned `files/upload-url` flow writes to the private bucket,
/// whose URLs 403 everywhere they render (pearl th-8f3591 / SMOODEV-2566). The
/// tradeoff is a side effect: it also stamps `organizations.<variant>`. For
/// `logo` and `icon` that's the same asset by another name; for the dark
/// variant it isn't, so we put the column back.
async fn upload_asset(client: &SmoothApiClient, org: &str, slot: Slot, mime: &'static str, bytes: Vec<u8>) -> Result<String> {
    // ponytail: capture-and-restore because the endpoint has no `logoDark`
    // variant and the spare column (`logoWordmark`) renders in proposals.
    // Upgrade path: add a `logoDark` variant to
    // `packages/backend/src/routes/organization-logo-upload.ts` and delete this.
    let restore = if slot == Slot::LogoDark {
        match client.get(&format!("/organizations/{org}")).await {
            Ok(o) => Some(o.get("logo").cloned().unwrap_or(Value::Null)),
            Err(e) => {
                eprintln!(
                    "  {} could not read the org's current logo ({e}); it may be overwritten by the dark upload",
                    "!".yellow()
                );
                None
            }
        }
    } else {
        None
    };

    let url = post_multipart(client, org, slot.variant(), mime, bytes).await?;

    if let Some(previous) = restore {
        if let Err(e) = client.patch(&format!("/organizations/{org}"), &json!({ "logo": previous })).await {
            eprintln!("  {} uploaded the dark logo but failed to restore organizations.logo: {e}", "!".yellow());
        }
    }
    Ok(url)
}

/// The raw multipart POST. `SmoothApiClient` only speaks JSON, so this borrows
/// its bearer and talks to the same base URL directly — the same shape
/// `files.rs` uses for its presigned byte transfers.
async fn post_multipart(client: &SmoothApiClient, org: &str, variant: &str, mime: &'static str, bytes: Vec<u8>) -> Result<String> {
    let token = client
        .credentials()
        .map(|c| c.access_token)
        .ok_or_else(|| anyhow::anyhow!("not logged in — run `th auth login`"))?;
    let url = format!("{}/organizations/{org}/logo/upload", smooth_api_client::base_url().trim_end_matches('/'));

    // The server derives the stored extension from the filename, so synthesize
    // one that matches the sniffed bytes rather than trusting the source name.
    let part = reqwest::multipart::Part::bytes(bytes)
        .file_name(format!("{variant}.{}", extension_for(mime)))
        .mime_str(mime)?;
    let form = reqwest::multipart::Form::new().part("file", part).text("variant", variant.to_string());

    let resp = reqwest::Client::new()
        .post(&url)
        .bearer_auth(token)
        .multipart(form)
        .send()
        .await
        .with_context(|| format!("POST {url}"))?;
    let status = resp.status();
    let text = resp.text().await.unwrap_or_default();
    if !status.is_success() {
        bail!("logo upload returned HTTP {status}: {text}");
    }
    serde_json::from_str::<Value>(&text)
        .ok()
        .and_then(|v| v.get("url").and_then(Value::as_str).map(ToString::to_string))
        .ok_or_else(|| anyhow::anyhow!("logo upload response had no `url`: {text}"))
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, reason = "unwrap/expect are the idiom for test assertions")]
mod tests {
    use super::*;

    // -- theme merge ---------------------------------------------------------

    #[test]
    fn merge_layers_over_existing_tokens() {
        // PUT replaces theme_json wholesale, so a partial `set` MUST keep the
        // tokens it didn't touch — this is the regression that would silently
        // wipe a partner's theme.
        let current = json!({ "primary": "#111111", "accent": "#222222" });
        let merged = merge_theme(Some(&current), &[("primary", "#7c3aed")]);
        assert_eq!(merged["primary"], "#7c3aed");
        assert_eq!(merged["accent"], "#222222", "untouched token must survive");
    }

    #[test]
    fn sequential_sets_accumulate_rather_than_replace() {
        // The behavior most likely to regress silently: PUT replaces the whole
        // theme_json column, so `set A` then `set B` must leave A in place —
        // and an absent token must stay absent, not become an explicit null.
        let after_a = merge_theme(None, &[("primary", "#111111")]);
        let after_b = merge_theme(Some(&after_a), &[("accent", "#222222")]);
        let after_c = merge_theme(Some(&after_b), &[("sidebar", "#333333")]);
        assert_eq!(after_c["primary"], "#111111");
        assert_eq!(after_c["accent"], "#222222");
        assert_eq!(after_c["sidebar"], "#333333");
        // Absent ≠ null: untouched tokens are not written at all.
        assert!(after_c.get("border").is_none(), "an untouched token must not appear as null");
        // Clearing one leaves the rest alone.
        let cleared = merge_theme(Some(&after_c), &[("accent", "")]);
        assert!(cleared.get("accent").is_none());
        assert_eq!(cleared["primary"], "#111111");
    }

    #[test]
    fn merge_from_nothing_and_clears_with_empty_string() {
        let fresh = merge_theme(None, &[("primary", "#fff")]);
        assert_eq!(fresh["primary"], "#fff");
        let cleared = merge_theme(Some(&json!({ "primary": "#fff" })), &[("primary", "")]);
        assert!(cleared.get("primary").is_none(), "a cleared token is dropped, not sent as null");
    }

    #[test]
    fn merge_never_emits_a_null_and_never_pads_absent_tokens() {
        // SMOODEV-2822: the dashboard emits every surface key unconditionally,
        // null when blank, and a strict server rejects on the KEY regardless of
        // value — so one dashboard save would poison every later CLI write.
        // We send ONLY keys that carry a value: no padding, and inherited nulls
        // are dropped rather than echoed back.
        let poisoned = json!({
            "primary": "#7c3aed", "accent": null, "colorScheme": null, "background": null,
            "foreground": null, "card": null, "border": null, "mutedForeground": null,
        });
        let merged = merge_theme(Some(&poisoned), &[("accent", "#47c4d7")]);
        let obj = merged.as_object().unwrap();
        assert!(obj.values().all(|v| !v.is_null()), "must never emit a null theme key: {merged}");
        assert_eq!(obj.len(), 2, "only the two tokens that carry a value: {merged}");
        assert_eq!(obj["primary"], "#7c3aed");
        assert_eq!(obj["accent"], "#47c4d7");

        // And setting one token never invents the other twelve.
        let one = merge_theme(None, &[("primary", "#111111")]);
        assert_eq!(one.as_object().unwrap().len(), 1);
    }

    #[test]
    fn merge_ignores_a_null_theme_column() {
        assert_eq!(merge_theme(Some(&Value::Null), &[("accent", "#0f0")])["accent"], "#0f0");
    }

    // -- contrast ------------------------------------------------------------

    #[test]
    fn hex_parses_in_every_accepted_form() {
        assert_eq!(parse_hex("#fff"), Some([1.0, 1.0, 1.0]));
        assert_eq!(parse_hex("#ffffff"), Some([1.0, 1.0, 1.0]));
        assert_eq!(parse_hex("#000000"), Some([0.0, 0.0, 0.0]));
        assert_eq!(parse_hex("#ffffffff"), Some([1.0, 1.0, 1.0])); // alpha dropped
        assert_eq!(parse_hex("  #FFF  "), Some([1.0, 1.0, 1.0])); // case + padding
        assert!(parse_hex("oklch(0.7 0.1 200)").is_none());
        assert!(parse_hex("rebeccapurple").is_none());
        assert!(parse_hex("#gggggg").is_none());
        assert!(parse_hex("#ff").is_none());
    }

    #[test]
    fn contrast_ratio_matches_wcag_reference_values() {
        // Black on white is the canonical 21:1; a color against itself is 1:1.
        let white = parse_hex("#ffffff").unwrap();
        let black = parse_hex("#000000").unwrap();
        assert!((contrast_ratio(black, white) - 21.0).abs() < 0.01);
        assert!((contrast_ratio(white, black) - 21.0).abs() < 0.01, "ratio is order-independent");
        assert!((contrast_ratio(white, white) - 1.0).abs() < 0.001);
    }

    #[test]
    fn verdict_flags_an_unreadable_theme() {
        // Light grey text on white — the partner-dashboard disaster.
        let theme = json!({ "foreground": "#cccccc", "background": "#ffffff" });
        let v = contrast_verdict(Some(&theme));
        assert!(!v.passes(), "1.6:1 must fail");
    }

    #[test]
    fn verdict_passes_a_readable_theme() {
        let theme = json!({
            "foreground": "#212121", "background": "#ffffff",
            "primary": "#7c3aed", "primaryForeground": "#ffffff",
            "mutedForeground": "#686e77",
        });
        let v = contrast_verdict(Some(&theme));
        assert!(v.passes(), "the Lane A sample theme reports passes:true");
        assert_eq!(v.pairs.len(), 3);
    }

    #[test]
    fn verdict_is_permissive_where_it_cannot_judge() {
        // No theme at all → Smoo defaults, nothing to check.
        assert!(contrast_verdict(None).passes());
        assert!(contrast_verdict(Some(&json!({}))).passes());
        // One side unset → the default token applies; not our call.
        assert!(contrast_verdict(Some(&json!({ "foreground": "#cccccc" }))).passes());
        // Non-hex → warn, don't block.
        let v = contrast_verdict(Some(&json!({ "foreground": "oklch(0.9 0 0)", "background": "#ffffff" })));
        assert!(v.passes());
        assert_eq!(v.warnings.len(), 1);
    }

    #[test]
    fn server_verdict_only_refuses_on_an_explicit_false() {
        assert!(server_refuses(Some(&Contrast {
            passes: Some(false),
            ..Default::default()
        })));
        assert!(!server_refuses(Some(&Contrast {
            passes: Some(true),
            ..Default::default()
        })));
        assert!(!server_refuses(Some(&Contrast::default())), "no verdict is not a refusal");
        assert!(!server_refuses(None));
    }

    // -- propose response ----------------------------------------------------

    /// The frozen Lane A contract, verbatim.
    const PROPOSE_FIXTURE: &str = r##"{
      "url": "https://chakrasolutions.ai/",
      "theme": { "colorScheme":"light","primary":"#7c3aed","primaryForeground":"#ffffff","accent":"#47c4d7",
                 "accentForeground":"#0b1220","background":"#ffffff","foreground":"#212121","card":"#f5f5f5",
                 "border":"#e5e5e5","muted":"#f4f4f4","mutedForeground":"#686e77",
                 "sidebar":"#ffffff","sidebarForeground":"#212121" },
      "appName": "Chakra AI Solutions",
      "logoCandidates": [ { "url":"https://x/a.png", "kind":"logo",     "source":"img" },
                          { "url":"https://x/b.png", "kind":"logoDark", "source":"img" },
                          { "url":"https://x/c.ico", "kind":"favicon",  "source":"link[rel=icon]" } ],
      "dominantColors": [ { "hex":"#ffffff","weight":141,"luminance":1,"chroma":0,"hue":null } ],
      "contrast": { "foregroundOnBackground":15.9, "primaryForegroundOnPrimary":5.2,
                    "mutedForegroundOnBackground":4.9, "passes":true, "warnings":[] },
      "notes": ["Static extraction …"]
    }"##;

    #[test]
    fn propose_fixture_deserializes() {
        let p: Proposal = serde_json::from_str(PROPOSE_FIXTURE).expect("frozen contract must parse");
        assert_eq!(p.app_name.as_deref(), Some("Chakra AI Solutions"));
        assert_eq!(p.logo_candidates.len(), 3);
        assert_eq!(p.candidate("logoDark").map(|c| c.url.as_str()), Some("https://x/b.png"));
        assert_eq!(p.candidate("favicon").and_then(|c| c.source.as_deref()), Some("link[rel=icon]"));
        assert!(p.candidate("wordmark").is_none());
        let c = p.contrast.expect("contrast present");
        assert_eq!(c.passes, Some(true));
        assert!((c.foreground_on_background.unwrap() - 15.9).abs() < f64::EPSILON);
        assert!(!server_refuses(Some(&c)));
        assert_eq!(p.notes.len(), 1);
    }

    #[test]
    fn propose_tolerates_omitted_and_unknown_fields() {
        // Lane A omits what it can't derive, and may add fields later.
        let p: Proposal = serde_json::from_str(r#"{"url":"https://x","somethingNew":42}"#).expect("must not fail on unknown/missing");
        assert!(p.theme.is_none());
        assert!(p.app_name.is_none());
        assert!(p.logo_candidates.is_empty());
        assert!(p.contrast.is_none());
    }

    #[test]
    fn fixture_theme_survives_a_render_and_a_merge() {
        let p: Proposal = serde_json::from_str(PROPOSE_FIXTURE).unwrap();
        let theme = p.theme.as_ref().unwrap().as_object().unwrap();
        let tokens: Vec<(&str, &str)> = theme.iter().filter_map(|(k, v)| v.as_str().map(|s| (k.as_str(), s))).collect();
        assert_eq!(tokens.len(), 13, "all 13 chrome tokens carried through");
        let merged = merge_theme(Some(&json!({ "primary": "#old" })), &tokens);
        assert_eq!(merged["primary"], "#7c3aed");
        assert_eq!(merged["sidebarForeground"], "#212121");
        // The locked Aurora tokens are structurally absent and stay that way.
        assert!(merged.get("colorHeat0").is_none());
        assert!(merged.get("gradientAurora").is_none());
    }

    #[test]
    fn enable_gate_honours_the_server_verdict_then_the_local_one() {
        let p: Proposal = serde_json::from_str(PROPOSE_FIXTURE).unwrap();
        let good = p.theme.clone();

        // The frozen fixture reports passes:true and reads clean locally.
        assert!(!refuse_to_go_live(p.contrast.as_ref(), good.as_ref(), false));

        // Server says no → refuse, even though the theme parses fine.
        let nope = Contrast {
            passes: Some(false),
            ..Default::default()
        };
        assert!(refuse_to_go_live(Some(&nope), good.as_ref(), false));
        // …and --force is the only way past it.
        assert!(!refuse_to_go_live(Some(&nope), good.as_ref(), true));

        // No report at all → the local check is the backstop, and it catches
        // grey-on-white that the server never got to judge.
        let unreadable = json!({ "foreground": "#cccccc", "background": "#ffffff" });
        assert!(refuse_to_go_live(None, Some(&unreadable), false));
        assert!(!refuse_to_go_live(None, Some(&unreadable), true));

        // Server says yes but the theme is locally unreadable → still refuse.
        // Disagreement resolves toward not shipping an unreadable dashboard.
        let yes = Contrast {
            passes: Some(true),
            ..Default::default()
        };
        assert!(refuse_to_go_live(Some(&yes), Some(&unreadable), false));

        // Nothing to judge → nothing to refuse.
        assert!(!refuse_to_go_live(None, None, false));
    }

    /// The real chakrasolutions.ai run (Lane A, PR #3676) — multiple candidates
    /// per kind, and the second `logo` is the page's og:image rather than the mark.
    const CHAKRA_LIVE_FIXTURE: &str = r##"{
      "url": "https://chakrasolutions.ai/",
      "theme": { "colorScheme":"light","primary":"#8b5cf6","primaryForeground":"#000000","accent":"#47c4d7",
                 "background":"#ffffff","foreground":"#000000","card":"#f5f5f5","border":"#eeeeee",
                 "muted":"#f4f4f4","mutedForeground":"#686e77" },
      "appName": "Chakra AI Solutions",
      "logoCandidates": [ { "url":"https://x/wordmark.png", "kind":"logo",    "source":"img" },
                          { "url":"https://x/og-image.png", "kind":"logo",    "source":"meta[og:image]" },
                          { "url":"https://x/icon-32.png",  "kind":"favicon", "source":"link[rel=icon]" },
                          { "url":"https://x/icon-192.png", "kind":"favicon", "source":"link[rel=icon]" } ],
      "dominantColors": [ { "hex":"#ffffff","weight":141,"luminance":1,"chroma":0,"hue":null } ],
      "contrast": { "foregroundOnBackground":21, "primaryForegroundOnPrimary":4.96,
                    "mutedForegroundOnBackground":4.54, "passes":true, "warnings":[] },
      "notes": []
    }"##;

    #[test]
    fn live_chakra_fixture_picks_first_per_kind_and_agrees_on_contrast() {
        let p: Proposal = serde_json::from_str(CHAKRA_LIVE_FIXTURE).unwrap();
        // Two `logo` and two `favicon` — best-first, so first-match per kind.
        assert_eq!(p.logo_candidates.len(), 4);
        assert_eq!(p.candidate("logo").map(|c| c.url.as_str()), Some("https://x/wordmark.png"));
        assert_eq!(p.candidate("favicon").map(|c| c.url.as_str()), Some("https://x/icon-32.png"));
        // No logoDark on this site — the slot is simply left alone.
        assert!(p.candidate("logoDark").is_none());
        // `hue: null` on a true neutral must not break the parse.
        assert!(p.contrast.is_some());
        // Our local check and the server's report must agree on the verdict,
        // even though the two compute the ratios slightly differently.
        assert!(!refuse_to_go_live(p.contrast.as_ref(), p.theme.as_ref(), false));
    }

    #[test]
    fn overrides_beat_the_extractors_pick() {
        let opts = FromUrlOpts {
            logo: Some("./our-logo.svg".into()),
            ..Default::default()
        };
        assert_eq!(opts.override_for(Slot::Logo), Some("./our-logo.svg"));
        assert_eq!(opts.override_for(Slot::Favicon), None);
        assert_eq!(Slot::for_kind("logoDark"), Some(Slot::LogoDark));
        assert_eq!(Slot::for_kind("wordmark"), None);
    }

    #[test]
    fn a_verdict_with_no_measurements_is_vacuous() {
        // The extractor OMITS a ratio it couldn't derive, so `passes: true`
        // with nothing measured must not read as a pass — an empty proposal
        // would otherwise sail through `--enable`.
        let empty = Contrast {
            passes: Some(true),
            ..Default::default()
        };
        assert!(empty.measured().is_empty());

        let real: Proposal = serde_json::from_str(CHAKRA_LIVE_FIXTURE).unwrap();
        let measured = real.contrast.as_ref().unwrap().measured();
        assert_eq!(measured.len(), 3);
        assert!((measured[0].1 - 21.0).abs() < f64::EPSILON);

        // A partial report is still real evidence for the pairs it covers.
        let partial = Contrast {
            foreground_on_background: Some(12.0),
            passes: Some(true),
            ..Default::default()
        };
        assert_eq!(partial.measured().len(), 1);
    }

    #[test]
    fn swatch_renders_a_block_for_hex_and_blank_for_anything_else() {
        let block = swatch("#7c3aed");
        assert!(block.contains('█'), "hex must render a swatch, got {block:?}");
        // oklch / named colors are valid in the schema but not renderable here;
        // pad instead so the column stays aligned.
        assert_eq!(swatch("oklch(0.7 0.1 200)"), "   ");
        assert_eq!(swatch(""), "   ");
    }

    #[test]
    fn fixture_theme_passes_the_local_contrast_gate() {
        // The local check is the backstop when the server sends no report; it
        // must agree with the frozen contract's `passes:true`.
        let p: Proposal = serde_json::from_str(PROPOSE_FIXTURE).unwrap();
        assert!(contrast_verdict(p.theme.as_ref()).passes());
    }

    // -- URL vetting ---------------------------------------------------------

    #[test]
    fn vet_url_blocks_private_and_metadata_hosts() {
        for bad in [
            "http://localhost/logo.png",
            "http://127.0.0.1/logo.png",
            "http://10.0.0.5/logo.png",
            "http://192.168.1.1/logo.png",
            "http://169.254.169.254/latest/meta-data/", // cloud metadata
            "http://172.16.0.1/x.png",
            "http://172.31.255.255/x.png",
            "http://foo.internal/x.png",
            "http://printer.local/x.png",
            "http://0.0.0.0/x.png",
        ] {
            assert!(vet_url(bad).is_err(), "{bad} must be refused");
        }
    }

    #[test]
    fn vet_url_allows_public_hosts_including_near_misses() {
        for ok in [
            "https://chakrasolutions.ai/logo.png",
            "http://example.com/logo.svg",
            "https://172.15.0.1/x.png",  // just below the private /12
            "https://172.32.0.1/x.png",  // just above it
            "https://10a.example.com/x", // not the 10/8 block
        ] {
            assert!(vet_url(ok).is_ok(), "{ok} must be allowed");
        }
    }

    #[test]
    fn vet_url_rejects_non_http_schemes_and_junk() {
        assert!(vet_url("file:///etc/passwd").is_err());
        assert!(vet_url("ftp://example.com/logo.png").is_err());
        assert!(vet_url("gopher://example.com").is_err());
        assert!(vet_url("not a url").is_err());
    }

    // -- image sniffing ------------------------------------------------------

    #[test]
    fn sniff_recognizes_the_accepted_formats() {
        assert_eq!(sniff_image(b"\x89PNG\r\n\x1a\nrest").unwrap(), "image/png");
        assert_eq!(sniff_image(&[0xFF, 0xD8, 0xFF, 0xE0]).unwrap(), "image/jpeg");
        assert_eq!(sniff_image(b"GIF89a....").unwrap(), "image/gif");
        assert_eq!(sniff_image(b"RIFF\0\0\0\0WEBPVP8 ").unwrap(), "image/webp");
        assert_eq!(
            sniff_image(br#"<?xml version="1.0"?><svg xmlns="http://www.w3.org/2000/svg"/>"#).unwrap(),
            "image/svg+xml"
        );
        assert_eq!(sniff_image(b"<svg viewBox=\"0 0 1 1\"></svg>").unwrap(), "image/svg+xml");
    }

    #[test]
    fn sniff_rejects_non_images_and_names_the_ico_case() {
        // .ico is what `link[rel=icon]` usually yields, and the platform's
        // allowlist has no image/x-icon — say so instead of a bare 400.
        let ico = sniff_image(&[0x00, 0x00, 0x01, 0x00, 0x01, 0x00]).unwrap_err().to_string();
        assert!(ico.contains(".ico"), "got {ico}");
        assert!(sniff_image(b"<html><body>404</body></html>").is_err());
        assert!(sniff_image(b"").is_err());
        assert!(sniff_image(b"%PDF-1.7").is_err());
    }

    #[test]
    fn extension_follows_the_sniffed_mime() {
        assert_eq!(extension_for("image/png"), "png");
        assert_eq!(extension_for("image/jpeg"), "jpg");
        assert_eq!(extension_for("image/svg+xml"), "svg");
        assert_eq!(extension_for("image/webp"), "webp");
    }

    // -- upload slots --------------------------------------------------------

    #[test]
    fn dark_logo_never_borrows_the_wordmark_column() {
        // `logoWordmark` renders in customer proposals — clobbering it to park
        // a dark logo would corrupt live documents.
        assert_eq!(Slot::Logo.variant(), "logo");
        assert_eq!(Slot::Favicon.variant(), "icon");
        assert_eq!(Slot::LogoDark.variant(), "logo");
        assert_ne!(Slot::LogoDark.variant(), "logoWordmark");
        assert_eq!(Slot::LogoDark.label(), "logo-dark");
    }

    // -- flags ---------------------------------------------------------------

    #[test]
    fn theme_flags_report_only_what_was_passed() {
        let flags = ThemeFlags {
            primary: Some("#7c3aed".into()),
            sidebar_foreground: Some("#212121".into()),
            ..Default::default()
        };
        assert_eq!(flags.passed(), vec![("primary", "#7c3aed"), ("sidebarForeground", "#212121")]);
        assert!(ThemeFlags::default().passed().is_empty());
    }

    // -- error explanation ---------------------------------------------------

    #[test]
    fn a_400_naming_phase2_tokens_gets_the_stale_schema_diagnosis() {
        let body = json!({ "themeJson": { "primary": "#fff", "background": "#000" } });
        let msg = explain_put_failure("PUT /x returned HTTP 400 Bad Request: Invalid request body", &body).to_string();
        assert!(msg.contains("background"), "must name the offending token: {msg}");
        assert!(!msg.contains("primary = "), "must not blame an accepted token");
        assert!(msg.contains("predates"), "must explain the cause: {msg}");
        // Worded to age: it describes the symptom, not a file that will move.
        assert!(!msg.contains(".rs") && !msg.contains(".ts"), "must not name source files: {msg}");
    }

    #[test]
    fn other_failures_are_passed_through_or_explained_on_their_own_terms() {
        let accent_only = json!({ "themeJson": { "primary": "#fff" } });
        let msg = explain_put_failure("PUT /x returned HTTP 400 Bad Request: nope", &accent_only).to_string();
        assert!(!msg.contains("predates"), "accent-only 400 has a different cause: {msg}");

        let forbidden = explain_put_failure("PUT /x returned HTTP 403 Forbidden", &json!({})).to_string();
        assert!(forbidden.contains("th auth login"));

        let plain = explain_put_failure("PUT /x returned HTTP 500", &json!({})).to_string();
        assert_eq!(plain, "PUT /x returned HTTP 500");
    }

    // -- misc ----------------------------------------------------------------

    #[test]
    fn nullable_distinguishes_clear_from_set() {
        assert!(nullable("").is_null());
        assert_eq!(nullable("Acme CRM"), json!("Acme CRM"));
    }

    #[test]
    fn preview_url_is_the_dashboards_brand_preview_param() {
        // Matches the link on apps/web .../settings/branding/page.tsx.
        assert_eq!(preview_url(), "https://smoo.ai/apps?brandPreview=1");
    }

    #[test]
    fn branding_path_is_org_scoped() {
        assert_eq!(branding_path("abc"), "/organizations/abc/branding");
    }
}
