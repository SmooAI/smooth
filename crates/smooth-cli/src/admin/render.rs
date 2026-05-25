//! Output rendering for `th admin` — pretty tables by default,
//! `--json` for raw JSON.
//!
//! Generic JSON-shape detector handles the four common patterns
//! from `/admin/*` endpoints:
//!
//! - **array of objects** → table with columns = union of all keys
//! - **array of primitives** → single-column table
//! - **object with one list-valued key** (envelope, e.g.
//!   `{organizations: [...], pagination: {...}}`) → unwrap that list,
//!   render its rows, print envelope metadata as a footer
//! - **object** → key/value two-column table
//! - **null / empty** → "(no results)" line
//!
//! Per-endpoint custom rendering (column reordering, omitting noisy
//! fields like `created_at` timestamps, coloring statuses) layers
//! on top via [`TableOptions::columns`] — see usage in `user.rs` /
//! `org.rs` if a specific endpoint needs hand-tuning.

use comfy_table::{presets::UTF8_FULL_CONDENSED, Cell, ContentArrangement, Table};
use owo_colors::OwoColorize;
use serde_json::Value;

/// How a single command wants to format its output. The leaf
/// dispatch branches on `Format::Json` (raw) vs `Format::Table`
/// (pretty).
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

/// Optional per-endpoint tweaks. The default (`TableOptions::default()`)
/// is "show every column from every row, ordered alphabetically" —
/// fine for ad-hoc inspection. Use `with_columns` to force a specific
/// column order / subset for endpoints whose response shape we know.
#[derive(Debug, Default, Clone)]
pub struct TableOptions {
    /// If `Some`, render exactly these columns in this order. If
    /// `None`, derive from the union of keys in the rows.
    pub columns: Option<Vec<&'static str>>,
    /// Optional human label for the rendered set ("organizations",
    /// "members", etc.) shown above the table.
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

/// Render a value according to `format`. The single entry point —
/// commands call this with their HTTP response.
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
    // Try to unwrap a common envelope: `{key: [...], ...}` where
    // exactly one top-level key is an array. Render the array, then
    // show the remaining keys as a footer for pagination metadata.
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
        println!("{}", label.bold().cyan());
    }

    match rows_value {
        Value::Array(items) if items.is_empty() => {
            println!("  {}", "(no results)".dimmed());
        }
        Value::Array(items) if items.iter().all(Value::is_object) => {
            print_object_array(items, opts);
        }
        Value::Array(items) => {
            // Array of primitives — single-column table.
            let mut table = Table::new();
            table
                .load_preset(UTF8_FULL_CONDENSED)
                .set_content_arrangement(ContentArrangement::Dynamic)
                .set_header(vec![Cell::new("value")]);
            for item in items {
                table.add_row(vec![Cell::new(cell_text(item))]);
            }
            println!("{table}");
            println!("  {} {}", items.len().to_string().bold(), "row(s)".dimmed());
        }
        Value::Object(map) => {
            // Single object — key/value two-column table.
            let mut table = Table::new();
            table
                .load_preset(UTF8_FULL_CONDENSED)
                .set_content_arrangement(ContentArrangement::Dynamic)
                .set_header(vec![Cell::new("field"), Cell::new("value")]);
            let mut keys: Vec<&String> = map.keys().collect();
            keys.sort();
            for k in keys {
                table.add_row(vec![Cell::new(k), Cell::new(cell_text(&map[k]))]);
            }
            println!("{table}");
        }
        Value::Null => {
            println!("  {}", "(empty response)".dimmed());
        }
        primitive => {
            println!("  {}", cell_text(primitive));
        }
    }

    if !envelope_extras.is_empty() {
        println!();
        let mut footer = Table::new();
        footer.load_preset(UTF8_FULL_CONDENSED).set_content_arrangement(ContentArrangement::Dynamic);
        for (k, v) in envelope_extras {
            footer.add_row(vec![Cell::new(k), Cell::new(cell_text(v))]);
        }
        println!("{}", "metadata".dimmed());
        println!("{footer}");
    }
}

fn print_object_array(items: &[Value], opts: &TableOptions) {
    // Determine columns: explicit list if given, else union of all
    // keys (in alphabetical order so output is stable across runs).
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

    let mut table = Table::new();
    table.load_preset(UTF8_FULL_CONDENSED).set_content_arrangement(ContentArrangement::Dynamic);
    table.set_header(columns.iter().map(|c| Cell::new(c.bold().to_string())).collect::<Vec<_>>());

    for item in items {
        let row: Vec<Cell> = columns
            .iter()
            .map(|col| {
                let v = item.get(col.as_str()).unwrap_or(&Value::Null);
                Cell::new(cell_text(v))
            })
            .collect();
        table.add_row(row);
    }
    println!("{table}");
    println!("  {} {}", items.len().to_string().bold(), "row(s)".dimmed());
}

/// Stringify a JSON value for table-cell display. Strings stay
/// unquoted; nested objects/arrays serialize compactly so they fit.
fn cell_text(v: &Value) -> String {
    match v {
        Value::Null => "—".to_string(),
        Value::String(s) => s.clone(),
        Value::Bool(b) => b.to_string(),
        Value::Number(n) => n.to_string(),
        Value::Array(_) | Value::Object(_) => {
            let raw = serde_json::to_string(v).unwrap_or_else(|_| v.to_string());
            // Truncate long compact-JSON so tables don't blow up. The
            // user can re-run with --json to get the full value.
            if raw.chars().count() > 60 {
                let truncated: String = raw.chars().take(57).collect();
                format!("{truncated}…")
            } else {
                raw
            }
        }
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
    fn cell_text_handles_primitives_and_compacts_nested() {
        assert_eq!(cell_text(&json!(null)), "—");
        assert_eq!(cell_text(&json!("hello")), "hello");
        assert_eq!(cell_text(&json!(42)), "42");
        assert_eq!(cell_text(&json!(true)), "true");
        // Nested compact-serialized, under 60 chars stays intact.
        let nested = json!({"a": 1, "b": "x"});
        assert!(cell_text(&nested).contains("a") && cell_text(&nested).contains("b"));
        // Long nested → truncated with ellipsis.
        let long = json!({"long_key_name_one": "padding_padding_padding_padding_padding"});
        let s = cell_text(&long);
        assert!(s.ends_with('…'), "long values truncate with an ellipsis, got: {s}");
    }

    #[test]
    fn table_options_builders_compose() {
        let opts = TableOptions::default().with_columns(&["id", "email"]).with_label("users");
        assert_eq!(opts.columns.as_deref(), Some(&["id", "email"][..]));
        assert_eq!(opts.label, Some("users"));
    }
}
