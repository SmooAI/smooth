//! `th widgets` — scaffold + verify SmooAI dashboard widgets.
//!
//! Widgets live in the smooai monorepo (not here). This command reads and
//! scaffolds INTO that repo (default `../smooai-SMOODEV-2560-aurora-widgets`,
//! `--repo` to override). The five touchpoints to add a widget are:
//!   1. `packages/schemas/src/dashboard/widget-registry.ts` — WIDGET_REGISTRY entry
//!   2. same file — IMPLEMENTED_WIDGET_IDS add
//!   3. `rust/api-prime/src/handlers/dashboard/widget_registry.rs` — Rust mirror
//!   4. `apps/web/components/dashboard/widgets/<id>-widget.tsx` — the component
//!   5. `apps/web/components/dashboard/widgets/widget-renderer.tsx` — a `case`
//!
//! `new` does 1–5 (registry/renderer edits surgical, component+test always
//! written; anything it can't insert surgically is PRINTED for you to paste).
//! `list`/`check` parse those files with a light string scan (no `regex`/`node`
//! dep — robust against a missing toolchain). `check` is the parity gate that
//! catches the registry/renderer/Rust drift class. `preview` is best-effort
//! (scaffolds a temp route + prints the screenshot command — see cmd_preview).

use std::path::{Path, PathBuf};

use anstream::println;
use anyhow::{bail, Context, Result};
use clap::Subcommand;
use owo_colors::OwoColorize;

use super::print_json;

const DEFAULT_REPO: &str = "/Users/brentrager/dev/smooai/smooai-SMOODEV-2560-aurora-widgets";

const REGISTRY_TS: &str = "packages/schemas/src/dashboard/widget-registry.ts";
const REGISTRY_RS: &str = "rust/api-prime/src/handlers/dashboard/widget_registry.rs";
const RENDERER_TSX: &str = "apps/web/components/dashboard/widgets/widget-renderer.tsx";
const WIDGETS_DIR: &str = "apps/web/components/dashboard/widgets";

const CATEGORIES: &[&str] = &[
    "overview",
    "agents",
    "crm",
    "support",
    "campaigns",
    "analytics",
    "content",
    "commerce",
    "operations",
];

const COMPONENT_TMPL: &str = include_str!("../widget_templates/component.tsx.tmpl");
const TEST_TMPL: &str = include_str!("../widget_templates/test.tsx.tmpl");

#[derive(Subcommand)]
pub enum Cmd {
    /// Scaffold a new widget across all 5 touchpoints. Registry/renderer/Rust
    /// edits are inserted surgically; the component + test files are always
    /// written. Anything that can't be inserted is printed for you to paste.
    New {
        /// Widget id — snake_case, e.g. `deal_velocity`.
        id: String,
        /// Widget category (one of overview|agents|crm|support|campaigns|analytics|content|commerce|operations).
        #[arg(long)]
        product: String,
        /// Display name. Defaults to a Title Case of the id.
        #[arg(long)]
        name: Option<String>,
        /// smooai repo path. Defaults to the aurora-widgets worktree.
        #[arg(long)]
        repo: Option<String>,
        /// Overwrite the component/test files if they already exist.
        #[arg(long)]
        force: bool,
    },
    /// List every widget in WIDGET_REGISTRY with its category, features,
    /// supported sizes, and whether it has a real renderer.
    List {
        #[arg(long)]
        repo: Option<String>,
        /// Only widgets with a real renderer (IMPLEMENTED_WIDGET_IDS).
        #[arg(long)]
        implemented: bool,
        /// Filter to one category.
        #[arg(long)]
        category: Option<String>,
        #[arg(long)]
        json: bool,
    },
    /// Parity gate — verify the TS registry, Rust mirror, and renderer agree.
    /// Exits non-zero on drift. Catches the known registry/features drift class.
    Check {
        #[arg(long)]
        repo: Option<String>,
        #[arg(long)]
        json: bool,
    },
    /// Best-effort visual preview — scaffold a temp preview route + print the
    /// exact playwright screenshot command. See cmd_preview for the caveats.
    Preview {
        /// Widget id to preview.
        id: String,
        #[arg(long)]
        repo: Option<String>,
        /// small|medium|large|all. Default: all.
        #[arg(long, default_value = "all")]
        size: String,
        /// Output PNG path (informational — used in the printed command).
        #[arg(long)]
        out: Option<String>,
    },
}

pub async fn cmd(cmd: Cmd) -> Result<()> {
    match cmd {
        Cmd::New {
            id,
            product,
            name,
            repo,
            force,
        } => cmd_new(&id, &product, name.as_deref(), repo, force),
        Cmd::List {
            repo,
            implemented,
            category,
            json,
        } => cmd_list(repo, implemented, category.as_deref(), json),
        Cmd::Check { repo, json } => cmd_check(repo, json),
        Cmd::Preview { id, repo, size, out } => cmd_preview(&id, repo, &size, out.as_deref()),
    }
}

// ---------------------------------------------------------------------------
// Repo resolution
// ---------------------------------------------------------------------------

fn resolve_repo(repo: Option<String>) -> Result<PathBuf> {
    let base = PathBuf::from(repo.unwrap_or_else(|| DEFAULT_REPO.to_string()));
    let registry = base.join(REGISTRY_TS);
    if !registry.is_file() {
        bail!("no widget registry at {} — is `--repo` the smooai monorepo root?", registry.display());
    }
    Ok(base)
}

// ---------------------------------------------------------------------------
// Parsing helpers (plain string scan — no regex/node dependency)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, serde::Serialize)]
struct WidgetInfo {
    id: String,
    name: String,
    category: String,
    #[serde(rename = "requiredFeatures")]
    required_features: Vec<String>,
    #[serde(rename = "supportedSizes")]
    supported_sizes: Option<Vec<String>>,
    #[serde(rename = "defaultVariant")]
    default_variant: Option<String>,
    implemented: bool,
}

/// Read `value` following `key` in `chunk`, where the field looks like
/// `key: 'value'`. Returns None if absent.
fn quoted_field(chunk: &str, key: &str) -> Option<String> {
    let start = chunk.find(&format!("{key}: '"))? + key.len() + 3;
    let rest = &chunk[start..];
    let end = rest.find('\'')?;
    Some(rest[..end].to_string())
}

/// Read a `key: [ ...quoted... ]` array's string members from `chunk`.
/// Returns None if the key is absent (distinct from present-but-empty).
fn array_field(chunk: &str, key: &str) -> Option<Vec<String>> {
    let start = chunk.find(&format!("{key}: ["))? + key.len() + 3;
    let rest = &chunk[start..];
    let end = rest.find(']')?;
    Some(quoted_strings(&rest[..end]))
}

/// Extract every single-quoted string in `s` (order preserved).
fn quoted_strings(s: &str) -> Vec<String> {
    let mut out = Vec::new();
    let bytes = s.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'\'' {
            if let Some(rel) = s[i + 1..].find('\'') {
                out.push(s[i + 1..i + 1 + rel].to_string());
                i = i + 1 + rel + 1;
                continue;
            }
        }
        i += 1;
    }
    out
}

/// Slice `s` between `from` (exclusive) and the first `to` after it (exclusive
/// of `to`). If `to` isn't found, runs to the end.
fn slice_between<'a>(s: &'a str, from: &str, to: &str) -> Option<&'a str> {
    let start = s.find(from)? + from.len();
    let rest = &s[start..];
    let end = rest.find(to).unwrap_or(rest.len());
    Some(&rest[..end])
}

/// Parse WIDGET_REGISTRY into ordered WidgetInfo. Keys off the per-entry
/// `id: '...'` field (unique per entry, always first), splitting the registry
/// block on it so each window holds exactly one entry's fields.
fn parse_registry(ts: &str, implemented: &[String]) -> Vec<WidgetInfo> {
    let block = slice_between(ts, "WIDGET_REGISTRY: Record<string, WidgetDefinition> = {", "\nexport ").unwrap_or(ts);
    let marker = "id: '";
    let mut out = Vec::new();
    let mut search = 0;
    while let Some(rel) = block[search..].find(marker) {
        let abs = search + rel + marker.len();
        // id = up to closing quote
        let Some(qend) = block[abs..].find('\'') else { break };
        let id = block[abs..abs + qend].to_string();
        // window = from here up to the next `id: '` (or end)
        let next = block[abs..].find(marker).map(|r| abs + r).unwrap_or(block.len());
        let window = &block[abs..next];
        out.push(WidgetInfo {
            name: quoted_field(window, "name").unwrap_or_default(),
            category: quoted_field(window, "category").unwrap_or_default(),
            required_features: array_field(window, "requiredFeatures").unwrap_or_default(),
            supported_sizes: array_field(window, "supportedSizes"),
            default_variant: quoted_field(window, "defaultVariant"),
            implemented: implemented.iter().any(|i| i == &id),
            id,
        });
        search = abs;
    }
    out
}

/// Parse the IMPLEMENTED_WIDGET_IDS Set membership.
fn parse_implemented(ts: &str) -> Vec<String> {
    let block = slice_between(ts, "IMPLEMENTED_WIDGET_IDS", "]);").unwrap_or("");
    // Only the strings after the `new Set<string>([` opener.
    let inner = block.find("([").map(|i| &block[i + 2..]).unwrap_or(block);
    quoted_strings(inner)
}

/// Parse `case '<id>':` labels from the renderer switch.
fn parse_renderer_cases(tsx: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut search = 0;
    let marker = "case '";
    while let Some(rel) = tsx[search..].find(marker) {
        let abs = search + rel + marker.len();
        if let Some(qend) = tsx[abs..].find('\'') {
            out.push(tsx[abs..abs + qend].to_string());
        }
        search = abs;
    }
    out
}

/// Parse widget ids from the Rust registry's `w(` constructor calls (first
/// string argument). Skips the `fn w(` definition (no leading quote) and any
/// `…w(` substring of a longer identifier (leading char is alphanumeric).
fn parse_rust_ids(rs: &str) -> Vec<String> {
    let mut out = Vec::new();
    let bytes = rs.as_bytes();
    let mut search = 0;
    while let Some(rel) = rs[search..].find("w(") {
        let abs = search + rel;
        search = abs + 2;
        // require a non-identifier char before `w`
        if abs > 0 {
            let prev = bytes[abs - 1];
            if prev.is_ascii_alphanumeric() || prev == b'_' {
                continue;
            }
        }
        // skip whitespace after `w(`, expect a quote
        let after = &rs[abs + 2..];
        let trimmed = after.trim_start();
        if let Some(stripped) = trimmed.strip_prefix('"') {
            if let Some(qend) = stripped.find('"') {
                out.push(stripped[..qend].to_string());
            }
        }
    }
    out
}

/// Parse the Rust post-build `match def.id.as_str() { … }` loop that assigns
/// supportedSizes/defaultVariant. Returns `id -> (sizes, variant)` — one entry
/// per id, expanding `|`-separated arm patterns (e.g. `"welcome" | "quick_stats"`)
/// and reading each arm's own `vec![…]` contents + `default_variant` string.
fn parse_rust_supported(rs: &str) -> std::collections::BTreeMap<String, (Vec<String>, Option<String>)> {
    let mut out = std::collections::BTreeMap::new();
    // Scope to the match block so unrelated `Some(vec![…])` elsewhere can't leak in.
    // Tolerate both `match def.id {` and `match def.id.as_str() {` (def.id is a
    // &'static str, so the plain form is what compiles on the pinned rustc).
    let block = slice_between(rs, "match def.id", "\n    }\n    registry").unwrap_or(rs);
    let assign = "supported_sizes = Some(vec![";
    let mut idx = 0;
    while let Some(rel) = block[idx..].find(assign) {
        let apos = idx + rel;
        // The arm's `=>` is the last one before this assignment; the pattern is
        // the text from the previous arm boundary (`{`/`}`) up to that `=>`.
        let Some(arrow) = block[..apos].rfind("=>") else { break };
        let pstart = block[..arrow].rfind(['{', '}']).map(|i| i + 1).unwrap_or(0);
        let ids = dq_strings(&block[pstart..arrow]);
        // This arm's size list.
        let vstart = apos + assign.len();
        let Some(vend) = block[vstart..].find("])") else { break };
        let sizes = dq_strings(&block[vstart..vstart + vend]);
        // This arm's default_variant — search only up to the next arm's `=>`.
        let arm_limit = block[vstart..].find("=>").map(|i| vstart + i).unwrap_or(block.len());
        let variant = slice_between(&block[vstart..arm_limit], "default_variant = Some(\"", "\"").map(str::to_string);
        for id in ids {
            out.insert(id, (sizes.clone(), variant.clone()));
        }
        idx = vstart + vend;
    }
    out
}

/// Extract double-quoted strings (Rust literals) from `s`.
fn dq_strings(s: &str) -> Vec<String> {
    let mut out = Vec::new();
    let bytes = s.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'"' {
            if let Some(rel) = s[i + 1..].find('"') {
                out.push(s[i + 1..i + 1 + rel].to_string());
                i = i + 1 + rel + 1;
                continue;
            }
        }
        i += 1;
    }
    out
}

// ---------------------------------------------------------------------------
// list
// ---------------------------------------------------------------------------

fn cmd_list(repo: Option<String>, implemented_only: bool, category: Option<&str>, json: bool) -> Result<()> {
    let repo = resolve_repo(repo)?;
    let ts = std::fs::read_to_string(repo.join(REGISTRY_TS)).context("read widget-registry.ts")?;
    let implemented = parse_implemented(&ts);
    let mut widgets = parse_registry(&ts, &implemented);

    if implemented_only {
        widgets.retain(|w| w.implemented);
    }
    if let Some(cat) = category {
        widgets.retain(|w| w.category == cat);
    }

    if json {
        print_json(&serde_json::to_value(&widgets)?);
        return Ok(());
    }

    println!();
    if widgets.is_empty() {
        println!("  {} {}", "●".dimmed(), "no widgets match".dimmed());
        println!();
        return Ok(());
    }
    for w in &widgets {
        let check = if w.implemented {
            "✓".green().to_string()
        } else {
            "○".dimmed().to_string()
        };
        let feats = if w.required_features.is_empty() {
            "—".dimmed().to_string()
        } else {
            w.required_features.join(",").dimmed().to_string()
        };
        let sizes = w.supported_sizes.as_ref().map(|s| s.join("/")).unwrap_or_else(|| "s/m/l".to_string());
        println!(
            "  {} {:<24} {:<11} {:<20} {}",
            check,
            w.id.cyan(),
            format!("[{}]", w.category).yellow(),
            feats,
            sizes.dimmed()
        );
    }
    println!();
    println!(
        "  {} {} widgets · {} implemented",
        "●".dimmed(),
        widgets.len().to_string().bold(),
        widgets.iter().filter(|w| w.implemented).count().to_string().green()
    );
    println!();
    Ok(())
}

// ---------------------------------------------------------------------------
// check
// ---------------------------------------------------------------------------

fn cmd_check(repo: Option<String>, json: bool) -> Result<()> {
    let repo = resolve_repo(repo)?;
    let ts = std::fs::read_to_string(repo.join(REGISTRY_TS)).context("read widget-registry.ts")?;
    let rs = std::fs::read_to_string(repo.join(REGISTRY_RS)).context("read widget_registry.rs")?;
    let renderer = std::fs::read_to_string(repo.join(RENDERER_TSX)).context("read widget-renderer.tsx")?;

    let implemented = parse_implemented(&ts);
    let widgets = parse_registry(&ts, &implemented);
    let ts_ids: Vec<String> = widgets.iter().map(|w| w.id.clone()).collect();
    let rust_ids = parse_rust_ids(&rs);
    let cases = parse_renderer_cases(&renderer);

    let mut failures: Vec<String> = Vec::new();

    // Rule 1: every TS registry id exists in the Rust mirror.
    for id in &ts_ids {
        if !rust_ids.contains(id) {
            failures.push(format!("registry id `{id}` is missing from the Rust widget_registry.rs mirror"));
        }
    }
    // Also flag Rust ids not in TS (reverse drift).
    for id in &rust_ids {
        if !ts_ids.contains(id) {
            failures.push(format!("Rust registry has `{id}` but it's not in the TS WIDGET_REGISTRY"));
        }
    }

    // Rule 2: every implemented id has a component file (or an inline case) AND a renderer case.
    for id in &implemented {
        let file = repo.join(WIDGETS_DIR).join(format!("{}-widget.tsx", id.replace('_', "-")));
        let has_case = cases.contains(id);
        if !has_case {
            failures.push(format!("implemented `{id}` has no `case '{id}':` in widget-renderer.tsx"));
        }
        // A component file OR an inline case satisfies the "has a renderer" contract
        // (e.g. `welcome` renders inline with no file). Only fail if neither.
        if !file.is_file() && !has_case {
            failures.push(format!(
                "implemented `{id}` has neither {}-widget.tsx nor a renderer case",
                id.replace('_', "-")
            ));
        }
    }

    // Rule 3: every renderer case id is in the registry.
    for id in &cases {
        if !ts_ids.contains(id) {
            failures.push(format!("renderer `case '{id}':` references an id not in WIDGET_REGISTRY"));
        }
    }

    // Rule 4: supportedSizes/defaultVariant parity between TS and the Rust mirror.
    let ts_sized: Vec<&WidgetInfo> = widgets.iter().filter(|w| w.supported_sizes.is_some()).collect();
    let rust_supported = parse_rust_supported(&rs);
    for w in &ts_sized {
        let Some((rust_s, rust_v)) = rust_supported.get(&w.id) else {
            failures.push(format!("`{}` sets supportedSizes in TS but not in the Rust mirror", w.id));
            continue;
        };
        if let Some(ts_s) = &w.supported_sizes {
            if ts_s != rust_s {
                failures.push(format!("`{}` supportedSizes differ: TS {ts_s:?} vs Rust {rust_s:?}", w.id));
            }
        }
        if let (Some(tv), Some(rv)) = (&w.default_variant, rust_v) {
            if tv != rv {
                failures.push(format!("`{}` defaultVariant differ: TS `{tv}` vs Rust `{rv}`", w.id));
            }
        }
    }
    for id in rust_supported.keys() {
        if !ts_sized.iter().any(|w| &w.id == id) {
            failures.push(format!("`{id}` sets supportedSizes in the Rust mirror but not in TS"));
        }
    }

    if json {
        print_json(&serde_json::json!({
            "ok": failures.is_empty(),
            "registryCount": ts_ids.len(),
            "rustCount": rust_ids.len(),
            "implementedCount": implemented.len(),
            "rendererCases": cases.len(),
            "failures": failures,
        }));
        if failures.is_empty() {
            return Ok(());
        }
        bail!("widget parity check failed with {} issue(s)", failures.len());
    }

    println!();
    println!("  {}", "widget parity check".bold());
    let row = |ok: bool, label: &str| {
        let mark = if ok { "✓".green().to_string() } else { "✗".red().to_string() };
        println!("    {mark} {label}");
    };
    row(
        ts_ids.iter().all(|i| rust_ids.contains(i)) && rust_ids.iter().all(|i| ts_ids.contains(i)),
        &format!("registry parity — {} TS / {} Rust", ts_ids.len(), rust_ids.len()),
    );
    row(
        implemented.iter().all(|i| cases.contains(i)),
        &format!("implemented widgets have a renderer case — {}", implemented.len()),
    );
    row(
        cases.iter().all(|i| ts_ids.contains(i)),
        &format!("renderer cases are all registered — {}", cases.len()),
    );
    row(
        failures.iter().all(|f| !f.contains("supportedSizes") && !f.contains("defaultVariant")),
        "size-variant parity (TS ↔ Rust)",
    );
    println!();

    if failures.is_empty() {
        println!("  {} {}", "✓".green().bold(), "all widget touchpoints are in parity".green());
        println!();
        return Ok(());
    }
    println!("  {} {} drift issue(s):", "✗".red().bold(), failures.len().to_string().red());
    for f in &failures {
        println!("    {} {}", "-".red(), f);
    }
    println!();
    bail!("widget parity check failed with {} issue(s)", failures.len());
}

// ---------------------------------------------------------------------------
// new
// ---------------------------------------------------------------------------

fn cmd_new(id: &str, product: &str, name: Option<&str>, repo: Option<String>, force: bool) -> Result<()> {
    if !CATEGORIES.contains(&product) {
        bail!("`{product}` is not a valid category. One of: {}", CATEGORIES.join(", "));
    }
    if !id.chars().next().is_some_and(|c| c.is_ascii_lowercase()) || !id.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_') {
        bail!("`{id}` must be snake_case (lowercase letters, digits, underscores; leading letter)");
    }
    let repo = resolve_repo(repo)?;
    let display = name.map(str::to_string).unwrap_or_else(|| title_case(id));
    let component = format!("{}Widget", pascal_case(id));
    let file_stem = format!("{}-widget", id.replace('_', "-"));
    let icon = "Sparkles";
    let feature = category_feature(product);
    let category_pascal = pascal_case(product);

    println!();
    println!("  {} scaffolding widget {} ({})", "●".cyan(), id.cyan().bold(), product.yellow());
    println!();

    // --- Files (always written) ---
    let comp_path = repo.join(WIDGETS_DIR).join(format!("{file_stem}.tsx"));
    let test_path = repo.join(WIDGETS_DIR).join("__tests__").join(format!("{file_stem}.test.tsx"));

    let comp_body = COMPONENT_TMPL
        .replace("{{COMPONENT}}", &component)
        .replace("{{NAME}}", &display)
        .replace("{{ICON}}", icon)
        .replace("{{ID}}", id);
    write_file(&comp_path, &comp_body, force)?;
    println!("    {} {}  {}", "+".green(), rel(&repo, &comp_path).dimmed(), "component".dimmed());

    let test_body = TEST_TMPL
        .replace("{{COMPONENT}}", &component)
        .replace("{{FILE_STEM}}", &file_stem)
        .replace("{{NAME}}", &display);
    write_file(&test_path, &test_body, force)?;
    println!("    {} {}  {}", "+".green(), rel(&repo, &test_path).dimmed(), "test".dimmed());

    // --- Surgical edits ---
    let mut printed: Vec<(String, String)> = Vec::new();

    // Touchpoint 1: WIDGET_REGISTRY entry — insert at top of the object literal.
    let feat_ts = feature.map(|f| format!("'{f}'")).unwrap_or_default();
    let registry_entry = format!(
        "    {id}: {{\n        id: '{id}',\n        name: '{display}',\n        description: '{display} widget',\n        icon: '{icon}',\n        category: '{product}',\n        requiredFeatures: [{feat_ts}],\n        defaultSize: {{ w: 6, h: 3 }},\n        minSize: {{ w: 4, h: 2 }},\n        maxSize: {{ w: 12, h: 5 }},\n        configurable: false,\n    }},\n"
    );
    let ts_path = repo.join(REGISTRY_TS);
    let ts = std::fs::read_to_string(&ts_path)?;
    if ts.contains(&format!("\n    {id}: {{")) {
        println!("    {} WIDGET_REGISTRY already has `{id}` — skipped", "=".dimmed());
    } else if let Some(updated) = insert_after(&ts, "WIDGET_REGISTRY: Record<string, WidgetDefinition> = {\n", &registry_entry) {
        // Touchpoint 2: IMPLEMENTED_WIDGET_IDS — insert into the Set.
        let updated = insert_after(&updated, "new Set<string>([\n", &format!("    '{id}',\n")).unwrap_or(updated);
        std::fs::write(&ts_path, updated)?;
        println!("    {} WIDGET_REGISTRY entry + IMPLEMENTED_WIDGET_IDS", "~".green());
    } else {
        printed.push(("WIDGET_REGISTRY (paste into the object literal)".into(), registry_entry.clone()));
        printed.push(("IMPLEMENTED_WIDGET_IDS (paste into the Set)".into(), format!("    '{id}',")));
    }

    // Touchpoint 3: Rust mirror — insert a w(...) call at the top of the vec!.
    let feat_rs = feature.map(|f| format!("\"{f}\"")).unwrap_or_default();
    let rust_entry = format!(
        "        w(\n            \"{id}\",\n            \"{display}\",\n            \"{display} widget\",\n            \"{icon}\",\n            WidgetCategory::{category_pascal},\n            &[{feat_rs}],\n            size(6, 3),\n            size(4, 2),\n            size(12, 5),\n            false,\n            None,\n        ),\n"
    );
    let rs_path = repo.join(REGISTRY_RS);
    let rs = std::fs::read_to_string(&rs_path)?;
    if rs.contains(&format!("\"{id}\",")) {
        println!("    {} Rust widget_registry.rs already has `{id}` — skipped", "=".dimmed());
    } else if let Some(updated) = insert_after(&rs, "let mut registry = vec![\n", &rust_entry) {
        std::fs::write(&rs_path, updated)?;
        println!("    {} Rust widget_registry.rs w(...) entry", "~".green());
    } else {
        printed.push(("rust/api-prime widget_registry.rs (paste into the vec!)".into(), rust_entry.clone()));
    }

    // Touchpoint 5: renderer — import + switch case.
    let rn_path = repo.join(RENDERER_TSX);
    let renderer = std::fs::read_to_string(&rn_path)?;
    let import_line = format!("import {{ {component} }} from './{file_stem}';\n");
    let case_block = format!("        case '{id}':\n            return <{component} config={{config}} />;\n");
    if renderer.contains(&format!("case '{id}':")) {
        println!("    {} widget-renderer.tsx already has `{id}` — skipped", "=".dimmed());
    } else {
        // Insert import before the interface, case after `switch (widgetId) {`.
        let with_import = insert_before(&renderer, "\ninterface WidgetRendererProps", &import_line);
        let with_case = with_import.as_deref().and_then(|r| insert_after(r, "switch (widgetId) {\n", &case_block));
        if let Some(updated) = with_case {
            std::fs::write(&rn_path, updated)?;
            println!("    {} widget-renderer.tsx import + case", "~".green());
        } else {
            printed.push((
                "widget-renderer.tsx import (top with the other widget imports)".into(),
                import_line.trim_end().to_string(),
            ));
            printed.push(("widget-renderer.tsx case (inside the switch)".into(), case_block.trim_end().to_string()));
        }
    }

    if !printed.is_empty() {
        println!();
        println!("  {} could not insert automatically — paste these:", "!".yellow().bold());
        for (label, snippet) in &printed {
            println!();
            println!("  {} {}", "→".yellow(), label.bold());
            for line in snippet.lines() {
                println!("      {line}");
            }
        }
    }

    println!();
    println!(
        "  {} {}",
        "next:".dimmed(),
        "wire the component's data fetch, review requiredFeatures, then:".dimmed()
    );
    println!("    {} {}", "•".dimmed(), format!("th widgets check --repo {}", rel_repo(&repo)).dimmed());
    println!(
        "    {} {}",
        "•".dimmed(),
        format!("pnpm --filter @smooai/web vitest run components/dashboard/widgets/__tests__/{file_stem}.test.tsx").dimmed()
    );
    println!();
    Ok(())
}

// ---------------------------------------------------------------------------
// preview (best-effort)
// ---------------------------------------------------------------------------

fn cmd_preview(id: &str, repo: Option<String>, size: &str, out: Option<&str>) -> Result<()> {
    let repo = resolve_repo(repo)?;
    let ts = std::fs::read_to_string(repo.join(REGISTRY_TS)).context("read widget-registry.ts")?;
    let implemented = parse_implemented(&ts);
    if !implemented.iter().any(|i| i == id) {
        bail!("`{id}` isn't in IMPLEMENTED_WIDGET_IDS — nothing to render. Run `th widgets list --implemented`.");
    }
    let sizes: Vec<&str> = if size == "all" {
        vec!["small", "medium", "large"]
    } else if ["small", "medium", "large"].contains(&size) {
        vec![size]
    } else {
        bail!("--size must be small|medium|large|all");
    };

    // TODO(th widgets): full headless render isn't reliably drivable from the CLI
    // — the dashboard widgets need the authed Next app + org context + a running
    // dev server + QueryClient. Rather than ship a flaky screenshot path, scaffold
    // a self-contained temp preview route and hand back the exact commands. This
    // is intentionally best-effort (see the command's doc comment).
    let route_dir = repo.join("apps/web/app/(dev)/widget-preview").join(id);
    std::fs::create_dir_all(&route_dir).with_context(|| format!("create {}", route_dir.display()))?;
    let route_path = route_dir.join("page.tsx");

    let providers: String = sizes
        .iter()
        .map(|s| {
            format!(
                "                <div style={{{{ width: 380, padding: 16 }}}}>\n                    <p style={{{{ fontSize: 11, opacity: 0.6 }}}}>{s}</p>\n                    <WidgetSizeProvider size=\"{s}\">\n                        <WidgetRenderer widgetId=\"{id}\" definition={{def}} />\n                    </WidgetSizeProvider>\n                </div>"
            )
        })
        .collect::<Vec<_>>()
        .join("\n");

    let page = format!(
        "'use client';\n\n// TEMP preview route scaffolded by `th widgets preview {id}`. Delete after use:\n//   rm -rf apps/web/app/(dev)/widget-preview\nimport {{ QueryClient, QueryClientProvider }} from '@tanstack/react-query';\nimport {{ WIDGET_REGISTRY }} from '@smooai/schemas/dashboard/widget-registry';\nimport {{ WidgetRenderer }} from '#/components/dashboard/widgets/widget-renderer';\nimport {{ WidgetSizeProvider }} from '#/components/dashboard/widgets/widget-kit';\n\nconst client = new QueryClient();\nconst def = WIDGET_REGISTRY['{id}'];\n\nexport default function WidgetPreviewPage() {{\n    return (\n        <QueryClientProvider client={{client}}>\n            <div style={{{{ display: 'flex', flexWrap: 'wrap', gap: 24, padding: 24, background: '#0b0b0f' }}}}>\n{providers}\n            </div>\n        </QueryClientProvider>\n    );\n}}\n"
    );
    std::fs::write(&route_path, page).with_context(|| format!("write {}", route_path.display()))?;

    let out = out.unwrap_or("widget-preview.png");
    println!();
    println!(
        "  {} {}",
        "●".yellow(),
        "widget preview is BEST-EFFORT (see `th widgets preview --help`)".yellow()
    );
    println!("    {} {}", "+".green(), rel(&repo, &route_path).dimmed());
    println!();
    println!("  {} start the app, then screenshot:", "→".dimmed());
    println!("    {} {}", "1.".dimmed(), "pnpm --filter @smooai/web dev   # or your usual dev:sst".dimmed());
    println!("    {} {}", "2.".dimmed(), format!("open http://localhost:3000/widget-preview/{id}").dimmed());
    println!(
        "    {} {}",
        "3.".dimmed(),
        format!("pnpm --filter @smooai/web exec playwright screenshot --full-page http://localhost:3000/widget-preview/{id} {out}").dimmed()
    );
    println!();
    println!("  {} {}", "cleanup:".dimmed(), "rm -rf apps/web/app/(dev)/widget-preview".dimmed());
    println!();
    Ok(())
}

// ---------------------------------------------------------------------------
// small utils
// ---------------------------------------------------------------------------

fn insert_after(haystack: &str, anchor: &str, insertion: &str) -> Option<String> {
    let idx = haystack.find(anchor)? + anchor.len();
    let mut out = String::with_capacity(haystack.len() + insertion.len());
    out.push_str(&haystack[..idx]);
    out.push_str(insertion);
    out.push_str(&haystack[idx..]);
    Some(out)
}

fn insert_before(haystack: &str, anchor: &str, insertion: &str) -> Option<String> {
    let idx = haystack.find(anchor)?;
    let mut out = String::with_capacity(haystack.len() + insertion.len());
    out.push_str(&haystack[..idx]);
    out.push_str(insertion);
    out.push_str(&haystack[idx..]);
    Some(out)
}

fn write_file(path: &Path, body: &str, force: bool) -> Result<()> {
    if path.exists() && !force {
        bail!("{} already exists — pass --force to overwrite", path.display());
    }
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).with_context(|| format!("create {}", parent.display()))?;
    }
    std::fs::write(path, body).with_context(|| format!("write {}", path.display()))?;
    Ok(())
}

fn pascal_case(s: &str) -> String {
    s.split('_')
        .filter(|p| !p.is_empty())
        .map(|p| {
            let mut c = p.chars();
            match c.next() {
                Some(f) => f.to_ascii_uppercase().to_string() + c.as_str(),
                None => String::new(),
            }
        })
        .collect()
}

fn title_case(s: &str) -> String {
    s.split('_')
        .filter(|p| !p.is_empty())
        .map(|p| {
            let mut c = p.chars();
            match c.next() {
                Some(f) => f.to_ascii_uppercase().to_string() + c.as_str(),
                None => String::new(),
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// Best-guess feature key for a category (the widget's requiredFeatures seed).
/// User is told to review it — this is a convenience, not a contract.
fn category_feature(category: &str) -> Option<&'static str> {
    match category {
        "agents" => Some("agents"),
        "crm" => Some("crm"),
        "support" => Some("support"),
        "campaigns" => Some("campaigns"),
        "analytics" => Some("analytics"),
        "content" => Some("contentbuilder"),
        "commerce" => Some("commerce"),
        // overview + operations have no single obvious feature — leave empty.
        _ => None,
    }
}

fn rel(repo: &Path, path: &Path) -> String {
    path.strip_prefix(repo).unwrap_or(path).display().to_string()
}

fn rel_repo(repo: &Path) -> String {
    repo.display().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE_TS: &str = r#"
export const WIDGET_REGISTRY: Record<string, WidgetDefinition> = {
    welcome: {
        id: 'welcome',
        name: 'Welcome',
        description: 'x',
        icon: 'Sparkles',
        category: 'overview',
        requiredFeatures: [],
        supportedSizes: ['large'],
        defaultVariant: 'large',
        configurable: false,
    },
    crm_pipeline: {
        id: 'crm_pipeline',
        name: 'Sales Pipeline',
        description: 'x',
        icon: 'TrendingUp',
        category: 'crm',
        requiredFeatures: ['crm'],
        configurable: false,
    },
};

export const IMPLEMENTED_WIDGET_IDS: ReadonlySet<string> = new Set<string>([
    'welcome',
    'crm_pipeline',
]);
"#;

    const SAMPLE_RS: &str = r#"
    let mut registry = vec![
        w(
            "welcome",
            "Welcome",
            "x",
            "Sparkles",
            WidgetCategory::Overview,
            &[],
            size(6, 2),
            size(4, 2),
            size(12, 3),
            false,
            None,
        ),
        w(
            "crm_pipeline",
            "Sales Pipeline",
            "x",
            "TrendingUp",
            WidgetCategory::Crm,
            &["crm"],
            size(6, 3),
            size(4, 3),
            size(12, 5),
            false,
            None,
        ),
    ];
    for def in registry.iter_mut() {
        match def.id.as_str() {
            "welcome" | "quick_stats" => {
                def.supported_sizes = Some(vec!["large".into()]);
                def.default_variant = Some("large".into());
            }
            "ga_conversions" => {
                def.supported_sizes = Some(vec!["medium".into(), "large".into()]);
                def.default_variant = Some("medium".into());
            }
            _ => {}
        }
    }
    registry
});
"#;

    #[test]
    fn parses_registry_ids_and_fields() {
        let impl_ids = parse_implemented(SAMPLE_TS);
        assert_eq!(impl_ids, vec!["welcome", "crm_pipeline"]);
        let widgets = parse_registry(SAMPLE_TS, &impl_ids);
        assert_eq!(widgets.len(), 2);
        assert_eq!(widgets[0].id, "welcome");
        assert_eq!(widgets[0].name, "Welcome");
        assert_eq!(widgets[0].category, "overview");
        assert_eq!(widgets[0].supported_sizes.as_deref(), Some(&["large".to_string()][..]));
        assert_eq!(widgets[1].required_features, vec!["crm"]);
        assert!(widgets[1].supported_sizes.is_none());
        assert!(widgets[0].implemented);
    }

    #[test]
    fn parses_rust_ids_skipping_fn_def() {
        let ids = parse_rust_ids(SAMPLE_RS);
        assert_eq!(ids, vec!["welcome", "crm_pipeline"]);
        let sized = parse_rust_supported(SAMPLE_RS);
        // welcome + quick_stats share the ['large'] arm; ga_conversions its own.
        assert_eq!(sized["welcome"], (vec!["large".to_string()], Some("large".to_string())));
        assert_eq!(sized["quick_stats"], (vec!["large".to_string()], Some("large".to_string())));
        assert_eq!(
            sized["ga_conversions"],
            (vec!["medium".to_string(), "large".to_string()], Some("medium".to_string()))
        );
    }

    #[test]
    fn renderer_cases_extracted() {
        let cases = parse_renderer_cases("switch (x) {\n case 'welcome':\n case 'crm_pipeline':\n }");
        assert_eq!(cases, vec!["welcome", "crm_pipeline"]);
    }

    #[test]
    fn casing_helpers() {
        assert_eq!(pascal_case("crm_pipeline"), "CrmPipeline");
        assert_eq!(title_case("deal_velocity"), "Deal Velocity");
        assert_eq!(pascal_case("ga_top_pages"), "GaTopPages");
    }

    #[test]
    fn insert_after_places_text() {
        let got = insert_after("[A][B]", "[A]", "X").unwrap();
        assert_eq!(got, "[A]X[B]");
        assert!(insert_after("abc", "zzz", "X").is_none());
    }
}
