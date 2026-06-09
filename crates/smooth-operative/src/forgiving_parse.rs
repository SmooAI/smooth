//! Forgiving tool-call parser for messy LLM output.
//!
//! Pearl th-c65ca3: small / cheap / quantized models routinely emit
//! tool-call intent as VISIBLE CONTENT instead of using the native
//! OpenAI `tool_calls` channel. The most common offender is the
//! Anthropic-style pseudo-XML `<function=NAME>{json args}</function>`
//! block, but the wild also includes Hermes-style `<tool_call>{json}`,
//! triple-backtick fenced JSON, and ad-hoc `NAME({json})` callouts.
//!
//! A rigid parser throws these away and the model's intended action
//! never runs. We do the opposite: try every reasonable format we've
//! seen, pull out a `(name, arguments)` pair, and let the agent loop
//! (or its caller) decide whether to execute it.
//!
//! This module is INSPIRED by the "Forgiving tool-call parser"
//! pattern documented in the SmallCode coding-agent project
//! (https://github.com/Doorman11991/smallcode). Same goal, Rust
//! implementation, fewer formats — only the ones we've actually
//! observed in our own gateway logs.
//!
//! ## Formats accepted
//!
//! 1. `<function=NAME>{json_args}</function>` — th-c65ca3 case
//! 2. `<function_calls>NAME({json_args})</function_calls>` — variant
//! 3. `<tool_call>{json}</tool_call>` — Hermes
//! 4. ` ```json\n{"name": "...", "arguments": {...}}\n``` ` — fenced
//! 5. ` ```\n{"function": "...", "args": {...}}\n``` ` — bare-JSON content
//! 6. `NAME({json_args})` — only when args parse as a JSON object and
//!    the whole message is short enough to look like a callout
//!
//! ## What we DON'T do
//!
//! - We never auto-execute. The parser returns `Option<ParsedToolCall>`
//!   and the caller decides. This keeps the security review surface tiny.
//! - We don't try to repair arbitrarily-broken JSON. If we can't parse
//!   the arguments to `serde_json::Value`, we return `None` (the
//!   sanitizer will fall back to its "[NOTE: malformed]" stub).

use serde_json::Value;

/// A tool call recovered from the assistant's visible content.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParsedToolCall {
    /// Tool name (e.g. `"bash"`, `"create_pearl"`).
    pub name: String,
    /// JSON arguments. Always `Value::Object` when the call came from a
    /// well-formed payload; could be `Value::Null` for tools that take
    /// no arguments.
    pub arguments: Value,
    /// The exact slice of input that produced this call, so the caller
    /// can strip it from any text it surfaces back to the user.
    pub raw: String,
}

/// Try every supported format. Returns the first match, or `None` if
/// the text doesn't contain a recoverable tool call.
///
/// Order of attempts is most-common-first. Each format helper is a
/// pure function over `&str` so they're cheap to call in sequence.
pub fn forgiving_parse_tool_call(text: &str) -> Option<ParsedToolCall> {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return None;
    }
    parse_function_xml(trimmed)
        .or_else(|| parse_function_calls_xml(trimmed))
        .or_else(|| parse_tool_call_hermes(trimmed))
        .or_else(|| parse_fenced_json(trimmed))
        .or_else(|| parse_bare_json_object(trimmed))
        .or_else(|| parse_name_paren_call(trimmed))
}

/// Pull the FIRST tool-call slice (`raw` of `forgiving_parse_tool_call`)
/// out of `text`, so callers can surface "everything except the tool
/// call" to the user. Returns `None` if no tool call was found.
///
/// Currently exercised only by unit tests; kept on the public surface
/// for future streaming code that wants to keep prose but drop the
/// inline call.
#[allow(dead_code)]
pub fn strip_tool_call_from_content(text: &str) -> Option<String> {
    let parsed = forgiving_parse_tool_call(text)?;
    let mut stripped = text.replacen(&parsed.raw, "", 1);
    // Collapse leftover blank-line runs so the output reads naturally.
    while stripped.contains("\n\n\n") {
        stripped = stripped.replace("\n\n\n", "\n\n");
    }
    Some(stripped.trim().to_string())
}

// ---------------------------------------------------------------------------
// Format 1: `<function=NAME>{json_args}</function>` (or `</tool_call>`)
//
// Real-world example (th-c65ca3):
//     <function=host_tool>{"tool": "curl", "args": ["-I", "https://x.y"]}</function>
//
// Some emitters close with `</tool_call>` instead of `</function>`.
// We accept either to maximize recovery rate.
// ---------------------------------------------------------------------------
fn parse_function_xml(text: &str) -> Option<ParsedToolCall> {
    let open_idx = text.find("<function=")?;
    let after_open = &text[open_idx + "<function=".len()..];
    let name_end = after_open.find('>')?;
    let name = after_open[..name_end].trim().trim_matches('"').to_string();
    if name.is_empty() {
        return None;
    }
    let body_start_abs = open_idx + "<function=".len() + name_end + 1;
    let body = &text[body_start_abs..];
    let (args_str, close_end_rel) = find_close_tag(body, &["</function>", "</tool_call>"])?;
    let arguments = parse_args_json_loose(args_str).or_else(|| parse_args_bare_text(args_str, &name))?;
    let raw_end = body_start_abs + close_end_rel;
    Some(ParsedToolCall {
        name,
        arguments,
        raw: text[open_idx..raw_end].to_string(),
    })
}

// ---------------------------------------------------------------------------
// Format 2: `<function_calls>NAME({json})</function_calls>`
//
// Some Claude-style emitters wrap the call in `<function_calls>` and
// put the call itself as `NAME(args)` inside. The args may be raw JSON
// OR a quoted string containing JSON — we accept both.
// ---------------------------------------------------------------------------
fn parse_function_calls_xml(text: &str) -> Option<ParsedToolCall> {
    let open_tag = "<function_calls>";
    let close_tag = "</function_calls>";
    let open_idx = text.find(open_tag)?;
    let body_start = open_idx + open_tag.len();
    let close_rel = text[body_start..].find(close_tag)?;
    let body = text[body_start..body_start + close_rel].trim();
    let parsed = parse_name_paren_call_inner(body)?;
    let raw_end = body_start + close_rel + close_tag.len();
    Some(ParsedToolCall {
        raw: text[open_idx..raw_end].to_string(),
        ..parsed
    })
}

// ---------------------------------------------------------------------------
// Format 3: `<tool_call>{json}</tool_call>` (Hermes-style)
//
// Example:
//     <tool_call>{"name":"bash","arguments":{"cmd":"ls"}}</tool_call>
// ---------------------------------------------------------------------------
fn parse_tool_call_hermes(text: &str) -> Option<ParsedToolCall> {
    let open_tag = "<tool_call>";
    let close_tag = "</tool_call>";
    let open_idx = text.find(open_tag)?;
    let body_start = open_idx + open_tag.len();
    let close_rel = text[body_start..].find(close_tag)?;
    let body = text[body_start..body_start + close_rel].trim();
    let value: Value = serde_json::from_str(body).ok()?;
    let parsed = parsed_from_named_object(&value)?;
    let raw_end = body_start + close_rel + close_tag.len();
    Some(ParsedToolCall {
        raw: text[open_idx..raw_end].to_string(),
        ..parsed
    })
}

// ---------------------------------------------------------------------------
// Format 4: ```json\n{...}\n``` (fenced)
//
// Models love to wrap JSON in markdown fences. We accept ```json or a
// bare ```. The inner JSON must have a `name`/`function`/`tool` key.
// ---------------------------------------------------------------------------
fn parse_fenced_json(text: &str) -> Option<ParsedToolCall> {
    let fence_idx = text.find("```")?;
    let after_fence = &text[fence_idx + 3..];
    // Optional language tag on the same line as the opening fence.
    let body_start_rel = after_fence.find('\n')?;
    let body_start_abs = fence_idx + 3 + body_start_rel + 1;
    let close_rel = text[body_start_abs..].find("```")?;
    let body = text[body_start_abs..body_start_abs + close_rel].trim();
    let value: Value = serde_json::from_str(body).ok()?;
    let parsed = parsed_from_named_object(&value)?;
    let raw_end = body_start_abs + close_rel + 3;
    Some(ParsedToolCall {
        raw: text[fence_idx..raw_end].to_string(),
        ..parsed
    })
}

// ---------------------------------------------------------------------------
// Format 5: bare JSON object (whole message is JSON-only)
//
// Some models, when told "respond with JSON", just dump the object as
// the entire content. We accept it ONLY when the trimmed input parses
// as a JSON object AND contains a known name key — otherwise we'd
// flag every JSON-shaped reply as a tool call.
// ---------------------------------------------------------------------------
fn parse_bare_json_object(text: &str) -> Option<ParsedToolCall> {
    if !text.starts_with('{') || !text.ends_with('}') {
        return None;
    }
    let value: Value = serde_json::from_str(text).ok()?;
    let parsed = parsed_from_named_object(&value)?;
    Some(ParsedToolCall {
        raw: text.to_string(),
        ..parsed
    })
}

// ---------------------------------------------------------------------------
// Format 6: `NAME({json_args})`
//
// Last-resort heuristic: the whole content looks like a function call.
// We require:
//   - first non-whitespace token to be an identifier
//   - immediately followed by `(`
//   - matching balanced parens covering the rest
//   - inside parens parses as JSON
//
// Without these guards we'd misclassify normal prose like
// "Call doctor(now)" as a tool call.
// ---------------------------------------------------------------------------
fn parse_name_paren_call(text: &str) -> Option<ParsedToolCall> {
    // Only fire when the message is "just a call" — short and tight.
    if text.lines().count() > 3 || text.len() > 2000 {
        return None;
    }
    parse_name_paren_call_inner(text)
}

fn parse_name_paren_call_inner(text: &str) -> Option<ParsedToolCall> {
    let trimmed = text.trim();
    let paren_idx = trimmed.find('(')?;
    let name = trimmed[..paren_idx].trim().to_string();
    if name.is_empty() || !name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
        return None;
    }
    if !trimmed.ends_with(')') {
        return None;
    }
    let args_str = trimmed[paren_idx + 1..trimmed.len() - 1].trim();
    let arguments = if args_str.is_empty() {
        Value::Object(serde_json::Map::new())
    } else {
        parse_args_json_loose(args_str)?
    };
    Some(ParsedToolCall {
        name,
        arguments,
        raw: trimmed.to_string(),
    })
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Given the body of `<function=NAME>…`, find the closing tag and
/// return `(body_before_close, end_offset_past_close)`.
fn find_close_tag<'a>(body: &'a str, candidates: &[&str]) -> Option<(&'a str, usize)> {
    candidates
        .iter()
        .filter_map(|tag| body.find(tag).map(|i| (i, i + tag.len())))
        .min_by_key(|(i, _)| *i)
        .map(|(close_start, close_end)| (&body[..close_start], close_end))
}

/// Parse a JSON value with a couple of common cleanups: stripping
/// surrounding quotes (when the args came from `NAME("{...}")`),
/// trimming trailing commas inside objects (a frequent SLM mistake).
fn parse_args_json_loose(s: &str) -> Option<Value> {
    let s = s.trim();
    // Direct parse — works for the common case `<function=foo>{"a":1}`.
    if let Ok(v) = serde_json::from_str::<Value>(s) {
        return Some(v);
    }
    // Strip a single layer of surrounding quotes and try again.
    if s.len() >= 2 && s.starts_with('"') && s.ends_with('"') {
        let inner = &s[1..s.len() - 1];
        // Un-escape the obvious backslash-quote escapes.
        let unescaped = inner.replace("\\\"", "\"").replace("\\\\", "\\");
        if let Ok(v) = serde_json::from_str::<Value>(&unescaped) {
            return Some(v);
        }
    }
    // Remove trailing commas before `}` or `]` — these break strict
    // JSON but are visually unambiguous.
    let cleaned = strip_trailing_commas(s);
    if cleaned != s {
        if let Ok(v) = serde_json::from_str::<Value>(&cleaned) {
            return Some(v);
        }
    }
    None
}

/// Last-ditch arg recovery for `<function=NAME>BARE TEXT</function>` —
/// the pattern small models love when they "know they should call a
/// tool" but haven't internalized JSON. Pearl th-67e338.
///
/// We strip orphaned closing tags (`</parameter>`, `</param>`, `</args>`),
/// trim whitespace, and if a single non-empty line remains, wrap it as
/// `{"<first-arg>": "<line>"}` where the arg name is inferred from the
/// tool name's first parameter (`read_file` → `path`, `bash` → `command`,
/// `search` → `query`, …). When we can't infer, we use `arg` as a
/// generic fallback so the structured note in
/// `sanitize_pseudo_tool_xml` still NAMES the tool — much more useful
/// to the next-turn LLM than a vague "malformed XML" stub.
fn parse_args_bare_text(s: &str, tool_name: &str) -> Option<Value> {
    let stripped = s
        .replace("</parameter>", "")
        .replace("</param>", "")
        .replace("</args>", "")
        .replace("</arg>", "");
    let trimmed = stripped.trim();
    if trimmed.is_empty() {
        return None;
    }
    // Reject things that look like JSON (already handled by the loose
    // parser above; if we're here it failed to parse, but we still want
    // to avoid treating broken JSON as a bare path).
    if trimmed.starts_with('{') || trimmed.starts_with('[') {
        return None;
    }
    // Reject anything still containing structured tags (e.g. Claude's
    // `<parameter=...>` opening tags) — the body isn't a single bare
    // value, it's a multi-arg pseudo-XML payload we can't trivially
    // collapse to one positional argument. Fall through to the legacy
    // stub for those.
    if trimmed.contains('<') {
        return None;
    }
    // Map the tool name to its most-likely first positional arg.
    let key = match tool_name {
        "read_file" | "read" | "open_file" | "cat" => "path",
        "write_file" | "write" | "create_file" | "edit_file" | "patch_file" => "path",
        "bash" | "shell" | "sh" | "exec" | "run" | "run_bash" | "run_shell" => "command",
        "search" | "grep" | "rg" | "ripgrep" | "code_search" => "query",
        "list_files" | "ls" | "list" => "path",
        _ => "arg",
    };
    Some(serde_json::json!({ key: trimmed.to_string() }))
}

fn strip_trailing_commas(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for ch in s.chars() {
        if ch == '}' || ch == ']' {
            // Walk back past spaces to see if the prior non-space was `,`.
            let trimmed_tail = out.trim_end();
            if trimmed_tail.ends_with(',') {
                let new_len = trimmed_tail.len() - 1;
                out.truncate(new_len);
            }
        }
        out.push(ch);
    }
    out
}

/// Extract a `ParsedToolCall` from a generic `{"name":..., "arguments":...}`
/// style object. Accepts a few key-name variants we've seen in the wild:
///   - `name` / `function` / `tool`
///   - `arguments` / `args` / `parameters` / `input`
fn parsed_from_named_object(value: &Value) -> Option<ParsedToolCall> {
    let obj = value.as_object()?;
    let name = ["name", "function", "tool"]
        .iter()
        .find_map(|k| obj.get(*k).and_then(Value::as_str))
        .map(str::to_string)?;
    if name.is_empty() {
        return None;
    }
    let arguments = ["arguments", "args", "parameters", "input"]
        .iter()
        .find_map(|k| obj.get(*k).cloned())
        .unwrap_or_else(|| Value::Object(serde_json::Map::new()));
    let arguments = normalize_arguments(arguments);
    Some(ParsedToolCall {
        name,
        arguments,
        raw: String::new(), // caller fills this in with the slice it found
    })
}

/// Arguments that arrived as a JSON-encoded STRING (`"{\"x\":1}"`)
/// instead of an actual object are common with cheap models. Try one
/// level of un-stringification before giving up.
fn normalize_arguments(value: Value) -> Value {
    match value {
        Value::String(s) => serde_json::from_str::<Value>(&s).unwrap_or(Value::String(s)),
        other => other,
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn obj(pairs: &[(&str, Value)]) -> Value {
        let mut m = serde_json::Map::new();
        for (k, v) in pairs {
            m.insert((*k).to_string(), v.clone());
        }
        Value::Object(m)
    }

    // -- Pearl th-67e338: bare-text args ---------------------------------

    #[test]
    fn parses_function_xml_with_bare_text_arg_th_67e338() {
        // The exact pattern from coding-sessions/fa62ad8c-…json — agent
        // emits a path as the body, with a stray </parameter> closing tag.
        let input = "<function=read_file>\nINSTRUCTIONS.md\n</parameter>\n</function>";
        let parsed = forgiving_parse_tool_call(input).expect("bare-text args must parse");
        assert_eq!(parsed.name, "read_file");
        assert_eq!(parsed.arguments, obj(&[("path", Value::String("INSTRUCTIONS.md".into()))]));
    }

    #[test]
    fn parses_function_xml_with_bare_text_bash_arg_th_67e338() {
        let input = "<function=bash>\npython3 -m pytest -q\n</function>";
        let parsed = forgiving_parse_tool_call(input).expect("bare-text bash must parse");
        assert_eq!(parsed.name, "bash");
        assert_eq!(parsed.arguments, obj(&[("command", Value::String("python3 -m pytest -q".into()))]));
    }

    #[test]
    fn parses_function_xml_with_bare_text_unknown_tool_uses_generic_arg_th_67e338() {
        let input = "<function=mystery_tool>\nsome value\n</function>";
        let parsed = forgiving_parse_tool_call(input).expect("unknown-tool bare-text must parse");
        assert_eq!(parsed.name, "mystery_tool");
        assert_eq!(parsed.arguments, obj(&[("arg", Value::String("some value".into()))]));
    }

    // -- Format 1: <function=NAME>{json}</function> ------------------------

    #[test]
    fn parses_function_xml_th_c65ca3_case() {
        // The exact failure mode in pearl th-c65ca3.
        let input = r#"Sure, let me try.
<function=host_tool>{"tool":"curl","args":["-I","https://smoo-hub.com"]}</function>"#;
        let parsed = forgiving_parse_tool_call(input).expect("th-c65ca3 case must parse");
        assert_eq!(parsed.name, "host_tool");
        assert_eq!(
            parsed.arguments,
            obj(&[
                ("tool", Value::String("curl".into())),
                ("args", serde_json::json!(["-I", "https://smoo-hub.com"]))
            ])
        );
        assert!(parsed.raw.starts_with("<function=host_tool>"));
        assert!(parsed.raw.ends_with("</function>"));
    }

    #[test]
    fn parses_function_xml_with_tool_call_closer() {
        // Same pattern but closer is `</tool_call>` (a real-world variant).
        let input = r#"<function=bash>{"cmd":"ls -la"}</tool_call>"#;
        let parsed = forgiving_parse_tool_call(input).expect("function+tool_call closer must parse");
        assert_eq!(parsed.name, "bash");
        assert_eq!(parsed.arguments, obj(&[("cmd", Value::String("ls -la".into()))]));
    }

    // -- Format 2: <function_calls>NAME({json})</function_calls> -----------

    #[test]
    fn parses_function_calls_xml() {
        let input = r#"<function_calls>bash({"cmd":"echo hi"})</function_calls>"#;
        let parsed = forgiving_parse_tool_call(input).expect("function_calls form must parse");
        assert_eq!(parsed.name, "bash");
        assert_eq!(parsed.arguments, obj(&[("cmd", Value::String("echo hi".into()))]));
    }

    // -- Format 3: <tool_call>{json}</tool_call> (Hermes) ------------------

    #[test]
    fn parses_hermes_tool_call() {
        let input = r#"<tool_call>{"name":"bash","arguments":{"cmd":"ls"}}</tool_call>"#;
        let parsed = forgiving_parse_tool_call(input).expect("hermes form must parse");
        assert_eq!(parsed.name, "bash");
        assert_eq!(parsed.arguments, obj(&[("cmd", Value::String("ls".into()))]));
    }

    #[test]
    fn hermes_accepts_function_key() {
        let input = r#"<tool_call>{"function":"read_file","args":{"path":"/tmp/x"}}</tool_call>"#;
        let parsed = forgiving_parse_tool_call(input).expect("hermes+function key must parse");
        assert_eq!(parsed.name, "read_file");
        assert_eq!(parsed.arguments, obj(&[("path", Value::String("/tmp/x".into()))]));
    }

    // -- Format 4: ```json fenced ------------------------------------------

    #[test]
    fn parses_fenced_json_with_lang_tag() {
        let input = "Here's the call:\n```json\n{\"name\":\"bash\",\"arguments\":{\"cmd\":\"pwd\"}}\n```";
        let parsed = forgiving_parse_tool_call(input).expect("fenced json must parse");
        assert_eq!(parsed.name, "bash");
        assert_eq!(parsed.arguments, obj(&[("cmd", Value::String("pwd".into()))]));
    }

    #[test]
    fn parses_fenced_json_no_lang_tag() {
        let input = "```\n{\"function\":\"foo\",\"input\":{\"x\":1}}\n```";
        let parsed = forgiving_parse_tool_call(input).expect("bare fenced json must parse");
        assert_eq!(parsed.name, "foo");
        assert_eq!(parsed.arguments, obj(&[("x", Value::from(1))]));
    }

    // -- Format 5: bare JSON object ---------------------------------------

    #[test]
    fn parses_bare_json_object() {
        let input = r#"{"name":"bash","arguments":{"cmd":"true"}}"#;
        let parsed = forgiving_parse_tool_call(input).expect("bare json must parse");
        assert_eq!(parsed.name, "bash");
        assert_eq!(parsed.arguments, obj(&[("cmd", Value::String("true".into()))]));
    }

    #[test]
    fn bare_json_unwraps_string_arguments() {
        // Cheap models emit arguments as a JSON-encoded STRING; we
        // re-parse to a real object so downstream callers see the
        // structure they expect.
        let input = r#"{"name":"bash","arguments":"{\"cmd\":\"ls\"}"}"#;
        let parsed = forgiving_parse_tool_call(input).expect("string-wrapped args must parse");
        assert_eq!(parsed.name, "bash");
        assert_eq!(parsed.arguments, obj(&[("cmd", Value::String("ls".into()))]));
    }

    // -- Format 6: NAME({json}) -------------------------------------------

    #[test]
    fn parses_name_paren_call() {
        let input = r#"bash({"cmd":"ls"})"#;
        let parsed = forgiving_parse_tool_call(input).expect("name(args) must parse");
        assert_eq!(parsed.name, "bash");
        assert_eq!(parsed.arguments, obj(&[("cmd", Value::String("ls".into()))]));
    }

    #[test]
    fn name_paren_rejects_multiline_prose() {
        // Don't be too eager — long prose mentioning a function call by
        // accident must not be classified as a tool call.
        let input = "Long thing.\nLine two.\nLine three.\nLine four ends with foo(bar)";
        assert!(forgiving_parse_tool_call(input).is_none());
    }

    // -- Negative cases ---------------------------------------------------

    #[test]
    fn plain_prose_returns_none() {
        let input = "I think the answer is 42. Here's why: it's the answer.";
        assert!(forgiving_parse_tool_call(input).is_none());
    }

    #[test]
    fn empty_string_returns_none() {
        assert!(forgiving_parse_tool_call("").is_none());
        assert!(forgiving_parse_tool_call("   \n  \t").is_none());
    }

    #[test]
    fn malformed_json_in_function_xml_returns_none() {
        // We don't try to repair arbitrarily-broken JSON. Sanitizer
        // takes over and stubs it out.
        let input = r#"<function=bash>{"cmd": ls}</function>"#;
        assert!(forgiving_parse_tool_call(input).is_none());
    }

    #[test]
    fn json_object_without_name_field_returns_none() {
        // Bare-JSON content that ISN'T a tool call must not be misread.
        let input = r#"{"result": 42, "ok": true}"#;
        assert!(forgiving_parse_tool_call(input).is_none());
    }

    // -- Round-trip property test -----------------------------------------

    #[test]
    fn args_round_trip_through_function_xml() {
        // Build a JSON value, wrap it in <function=NAME>...</function>,
        // parse, and assert the args come back identical. Covers
        // nested arrays + objects + numbers + bools + strings + null.
        let cases: Vec<Value> = vec![
            serde_json::json!({}),
            serde_json::json!({"x": 1}),
            serde_json::json!({"x": "y"}),
            serde_json::json!({"a": [1, 2, 3], "b": {"c": null}}),
            serde_json::json!({"flag": true, "n": 2.5, "s": "hello world"}),
        ];
        for args in cases {
            let payload = format!("<function=foo>{args}</function>");
            let parsed = forgiving_parse_tool_call(&payload).expect("round-trip must parse");
            assert_eq!(parsed.name, "foo");
            assert_eq!(parsed.arguments, args, "round-trip args mismatch for {payload}");
        }
    }

    // -- strip_tool_call_from_content -------------------------------------

    #[test]
    fn strip_removes_xml_block_keeps_surrounding_prose() {
        let input = "Sure, here we go:\n<function=bash>{\"cmd\":\"ls\"}</function>\nThat should do it.";
        let stripped = strip_tool_call_from_content(input).expect("must strip");
        assert!(stripped.contains("Sure, here we go:"));
        assert!(stripped.contains("That should do it."));
        assert!(!stripped.contains("<function="));
    }

    #[test]
    fn strip_returns_none_for_plain_prose() {
        assert!(strip_tool_call_from_content("just prose, nothing else").is_none());
    }
}
