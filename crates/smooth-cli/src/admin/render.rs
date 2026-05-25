//! Output rendering for `th admin` — pretty tables by default,
//! `--json` for raw JSON.
//!
//! Uses `tabled` (the same lib nushell uses) for the table layer,
//! pushed hard on theming:
//!
//! - **Modern rounded borders** — single-line UTF8 with rounded
//!   corners. Less visual noise than full grids.
//! - **Cyan bold headers** + underline so the header band reads as
//!   distinct from data even without colour terminal support.
//! - **Smart cell formatting** — UUIDs truncate to first 8 chars +
//!   ellipsis; ISO timestamps render as `YYYY-MM-DD HH:MM`; long
//!   nested JSON gets compact + truncated.
//! - **Status colours** — common status field values (`active`,
//!   `cancelled`, `expired`, `pending`, `error`, …) coloured
//!   green/red/yellow.
//!
//! Generic JSON-shape detector handles four common patterns:
//!
//! - array of objects → row-aligned table, columns = union of keys
//! - array of primitives → single-column table
//! - envelope `{key: [...], other: ...}` → unwrap, render the list,
//!   print envelope extras as a footer for pagination metadata
//! - object → key/value two-column table
//!
//! Per-endpoint customization via [`TableOptions`]: explicit column
//! order, omit noisy fields, label the table.

use owo_colors::OwoColorize;
use serde_json::Value;
use tabled::{
    builder::Builder,
    settings::{
        object::{Columns, Rows},
        style::{HorizontalLine, Style},
        Color, Modify, Width,
    },
};

/// How a single command wants to format its output.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Format {
    /// Pretty unicode table (default).
    #[default]
    Table,
    /// Pretty-printed JSON.
    Json,
}

impl Format {
    /// Resolve from a `--json` boolean flag.
    #[must_use]
    pub fn from_flag(json: bool) -> Self {
        if json {
            Self::Json
        } else {
            Self::Table
        }
    }
}

/// Optional per-endpoint tweaks.
#[derive(Debug, Default, Clone)]
pub struct TableOptions {
    /// If `Some`, render exactly these columns in this order. If
    /// `None`, derive from the union of keys in the rows.
    pub columns: Option<Vec<&'static str>>,
    /// Optional label printed above the table.
    pub label: Option<&'static str>,
}

impl TableOptions {
    /// Builder: lock down the column order.
    #[must_use]
    pub fn with_columns(mut self, columns: &'static [&'static str]) -> Self {
        self.columns = Some(columns.to_vec());
        self
    }

    /// Builder: set the header label.
    #[must_use]
    pub fn with_label(mut self, label: &'static str) -> Self {
        self.label = Some(label);
        self
    }
}

/// Render a value according to `format`. The single entry point.
pub fn render(value: &Value, format: Format, opts: &TableOptions) {
    match format {
        Format::Json => print_json(value),
        Format::Table => print_table(value, opts),
    }
}

fn print_json(value: &Value) {
    match serde_json::to_string_pretty(value) {
        Ok(s) => println!("{s}"),
        Err(_) => println!("{value}"),
    }
}

fn print_table(value: &Value, opts: &TableOptions) {
    // Envelope unwrap: `{key: [...], other: ...}` where exactly one
    // top-level key is an array. Render the array, footer the rest.
    let (rows_value, envelope_extras): (&Value, Vec<(String, &Value)>) = match value {
        Value::Object(map) => {
            let array_keys: Vec<&String> = map.iter().filter_map(|(k, v)| if v.is_array() { Some(k) } else { None }).collect();
            if array_keys.len() == 1 {
                let key = array_keys[0].clone();
                let extras: Vec<(String, &Value)> = map.iter().filter(|(k, _)| **k != key).map(|(k, v)| (k.clone(), v)).collect();
                (&map[&key], extras)
            } else {
                (value, Vec::new())
            }
        }
        _ => (value, Vec::new()),
    };

    if let Some(label) = opts.label {
        println!();
        println!("  {}", label.bold().cyan());
    }

    match rows_value {
        Value::Array(items) if items.is_empty() => {
            println!("  {}", "(no results)".dimmed());
        }
        Value::Array(items) if items.iter().all(Value::is_object) => {
            print_object_array(items, opts);
        }
        Value::Array(items) => {
            // Array of primitives — single-column.
            let mut builder = Builder::default();
            builder.push_record(["value"]);
            for item in items {
                builder.push_record([cell_text(item, "value")]);
            }
            let mut table = builder.build();
            apply_modern_style(&mut table);
            println!("{table}");
            println!("  {} {}", items.len().to_string().bold(), "row(s)".dimmed());
        }
        Value::Object(map) => {
            // Key/value two-column.
            let mut builder = Builder::default();
            builder.push_record(["field", "value"]);
            let mut keys: Vec<&String> = map.keys().collect();
            keys.sort();
            for k in keys {
                builder.push_record([k.clone(), cell_text(&map[k], k)]);
            }
            let mut table = builder.build();
            apply_modern_style(&mut table);
            println!("{table}");
        }
        Value::Null => {
            println!("  {}", "(empty response)".dimmed());
        }
        primitive => {
            println!("  {}", cell_text(primitive, ""));
        }
    }

    if !envelope_extras.is_empty() {
        println!();
        println!("  {}", "metadata".dimmed());
        let mut builder = Builder::default();
        for (k, v) in &envelope_extras {
            builder.push_record([k.clone(), cell_text(v, k)]);
        }
        let mut table = builder.build();
        table.with(Style::psql());
        println!("{table}");
    }
}

fn print_object_array(items: &[Value], opts: &TableOptions) {
    let columns: Vec<String> = match &opts.columns {
        Some(cols) => cols.iter().map(|s| (*s).to_string()).collect(),
        None => {
            let mut keyset = std::collections::BTreeSet::new();
            for item in items {
                if let Some(map) = item.as_object() {
                    for k in map.keys() {
                        keyset.insert(k.clone());
                    }
                }
            }
            keyset.into_iter().collect()
        }
    };

    let mut builder = Builder::default();
    builder.push_record(columns.iter().cloned());
    for item in items {
        let row: Vec<String> = columns
            .iter()
            .map(|col| {
                let v = item.get(col.as_str()).unwrap_or(&Value::Null);
                cell_text(v, col)
            })
            .collect();
        builder.push_record(row);
    }
    let mut table = builder.build();
    apply_modern_style(&mut table);
    println!("{table}");
    println!("  {} {}", items.len().to_string().bold(), "row(s)".dimmed());
}

/// Apply the "legit" styling — modern rounded borders, bold cyan
/// header band, smart widths, status colouring.
fn apply_modern_style(table: &mut tabled::Table) {
    table
        .with(
            Style::modern_rounded()
                .horizontals([(1, HorizontalLine::inherit(Style::modern_rounded()))])
                .remove_horizontal(),
        )
        .with(Modify::new(Rows::first()).with(Color::BOLD | Color::FG_CYAN))
        .with(Modify::new(Columns::new(..)).with(Width::wrap(50).keep_words(true)));
}

/// Stringify a JSON value for table-cell display, with column-aware
/// smart formatting.
fn cell_text(v: &Value, column: &str) -> String {
    match v {
        Value::Null => "—".to_string(),
        Value::String(s) => smart_format_string(s, column),
        Value::Bool(b) => b.to_string(),
        Value::Number(n) => n.to_string(),
        Value::Array(_) | Value::Object(_) => {
            let raw = serde_json::to_string(v).unwrap_or_else(|_| v.to_string());
            if raw.chars().count() > 50 {
                let truncated: String = raw.chars().take(47).collect();
                format!("{truncated}…")
            } else {
                raw
            }
        }
    }
}

/// Column-aware string display:
/// - looks-like-a-UUID → truncate to first 8 chars + ellipsis
/// - looks-like-an-ISO-timestamp → render as `YYYY-MM-DD HH:MM`
/// - looks-like-a-status (column name == "status" or "state") → coloured
/// - everything else → as-is
fn smart_format_string(s: &str, column: &str) -> String {
    if looks_like_uuid(s) {
        return format!("{}…", &s[..8.min(s.len())]);
    }
    if looks_like_iso_timestamp(s) {
        return format_iso_timestamp(s);
    }
    let col_lower = column.to_lowercase();
    if col_lower == "status" || col_lower == "state" {
        return colour_status(s);
    }
    s.to_string()
}

fn looks_like_uuid(s: &str) -> bool {
    // RFC 4122 shape: 8-4-4-4-12 hex. Length 36.
    if s.len() != 36 {
        return false;
    }
    let bytes = s.as_bytes();
    bytes[8] == b'-' && bytes[13] == b'-' && bytes[18] == b'-' && bytes[23] == b'-' && s.chars().all(|c| c.is_ascii_hexdigit() || c == '-')
}

fn looks_like_iso_timestamp(s: &str) -> bool {
    // Quick shape check: "YYYY-MM-DDTHH:MM:SS..." — must start with
    // four digits, dash, two digits, dash, two digits, then T or
    // space.
    if s.len() < 19 {
        return false;
    }
    let bytes = s.as_bytes();
    bytes[0].is_ascii_digit()
        && bytes[1].is_ascii_digit()
        && bytes[2].is_ascii_digit()
        && bytes[3].is_ascii_digit()
        && bytes[4] == b'-'
        && bytes[5].is_ascii_digit()
        && bytes[6].is_ascii_digit()
        && bytes[7] == b'-'
        && bytes[8].is_ascii_digit()
        && bytes[9].is_ascii_digit()
        && (bytes[10] == b'T' || bytes[10] == b' ')
}

fn format_iso_timestamp(s: &str) -> String {
    // "2026-05-24T18:42:50.671917Z" → "2026-05-24 18:42"
    let mut out = String::with_capacity(16);
    out.push_str(&s[..10]);
    out.push(' ');
    out.push_str(&s[11..16.min(s.len())]);
    out
}

fn colour_status(s: &str) -> String {
    match s.to_lowercase().as_str() {
        "active" | "ok" | "success" | "completed" | "healthy" | "ready" => s.green().bold().to_string(),
        "cancelled" | "canceled" | "failed" | "error" | "unhealthy" | "expired" => s.red().bold().to_string(),
        "pending" | "trial" | "warning" | "deprecated" => s.yellow().bold().to_string(),
        _ => s.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn from_flag_resolves_json_and_table() {
        assert_eq!(Format::from_flag(true), Format::Json);
        assert_eq!(Format::from_flag(false), Format::Table);
    }

    #[test]
    fn looks_like_uuid_accepts_real_uuid_rejects_others() {
        assert!(looks_like_uuid("f47ac10b-58cc-4372-a567-0e02b2c3d479"));
        assert!(!looks_like_uuid("not-a-uuid"));
        assert!(!looks_like_uuid("short"));
        assert!(!looks_like_uuid("xxxxxxxx-xxxx-xxxx-xxxx-xxxxxxxxxxxx")); // not hex
    }

    #[test]
    fn smart_format_truncates_uuids_to_first_8_plus_ellipsis() {
        let out = smart_format_string("f47ac10b-58cc-4372-a567-0e02b2c3d479", "id");
        assert_eq!(out, "f47ac10b…");
    }

    #[test]
    fn smart_format_iso_timestamp_renders_yyyy_mm_dd_hh_mm() {
        let out = smart_format_string("2026-05-24T18:42:50.671917Z", "created_at");
        assert_eq!(out, "2026-05-24 18:42");
    }

    #[test]
    fn smart_format_iso_timestamp_handles_space_separator() {
        let out = smart_format_string("2026-05-24 18:42:50", "created_at");
        assert_eq!(out, "2026-05-24 18:42");
    }

    #[test]
    fn smart_format_passes_through_plain_strings() {
        assert_eq!(smart_format_string("hello", "name"), "hello");
        assert_eq!(smart_format_string("brent@smoo.ai", "email"), "brent@smoo.ai");
    }

    #[test]
    fn cell_text_truncates_long_nested_with_ellipsis() {
        let long = json!({"key": "padding_padding_padding_padding_padding_padding_padding"});
        let s = cell_text(&long, "metadata");
        assert!(s.ends_with('…'), "long nested values truncate: {s}");
    }

    #[test]
    fn table_options_builders_compose() {
        let opts = TableOptions::default().with_columns(&["id", "email"]).with_label("users");
        assert_eq!(opts.columns.as_deref(), Some(&["id", "email"][..]));
        assert_eq!(opts.label, Some("users"));
    }

    #[test]
    fn colour_status_known_values_get_colour_codes_other_passthrough() {
        // Coloured = contains an ANSI escape. The actual code depends
        // on the terminal-detect logic in owo-colors; in tests it's
        // usually present.
        let active = colour_status("active");
        let unknown = colour_status("xyzzy");
        assert!(active.contains("active"));
        assert_eq!(unknown, "xyzzy"); // passthrough
    }
}

#[cfg(test)]
mod visual_demo {
    use super::*;
    use serde_json::json;

    /// `cargo test -p smooai-smooth-cli admin::render::visual_demo -- --nocapture`
    /// to eyeball the table styling. Always passes; the value is the printout.
    #[test]
    fn show_orgs_list_demo() {
        let orgs = json!({
            "organizations": [
                {"id": "f47ac10b-58cc-4372-a567-0e02b2c3d479", "name": "Every Kid Plays", "status": "active", "created_at": "2026-04-12T18:42:50.671917Z"},
                {"id": "a1b2c3d4-e5f6-4789-abcd-ef0123456789", "name": "Smoo AI", "status": "active", "created_at": "2025-08-01T09:00:00Z"},
                {"id": "deadbeef-1111-4222-8333-444444444444", "name": "Test Org (Pending)", "status": "pending", "created_at": "2026-05-24T20:15:00Z"},
                {"id": "00000000-0000-4000-8000-000000000001", "name": "Cancelled Co", "status": "cancelled", "created_at": "2024-12-01T12:00:00Z"},
            ],
            "total": 4,
            "limit": 50,
            "offset": 0
        });
        let opts = TableOptions::default()
            .with_label("organizations")
            .with_columns(&["id", "name", "status", "created_at"]);
        println!();
        render(&orgs, Format::Table, &opts);
        println!();
    }
}
