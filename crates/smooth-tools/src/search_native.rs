//! Native meta-search — the self-contained backend behind
//! [`web_search`](crate::WebSearchTool) (pearl th-7031ba, option B).
//!
//! `web_search` used to shell `th search`, which hits Smoo's in-house search
//! service on api.smoo.ai — so the tool silently degraded to "not logged in →
//! capped free tier" or failed outright when that service was unreachable. This
//! module aggregates public search APIs **in-process**, so `web_search` works on
//! any machine with or without Smoo auth.
//!
//! Sources, in descending rank weight:
//!
//! | source | key | notes |
//! |---|---|---|
//! | Brave Search API | `SMOOTH_BRAVE_API_KEY` / `BRAVE_API_KEY` | real web index, free tier ~2k/mo |
//! | SearXNG (JSON) | `SMOOTH_SEARXNG_URL` | real web index, keyless if you host one |
//! | DuckDuckGo Instant Answer | none | keyless, entity/definition coverage only |
//! | Hacker News (Algolia) | none | keyless, the recency source for software/news |
//! | Wikipedia search | none | keyless, encyclopedic coverage only |
//!
//! Every source is optional and failures are swallowed per-source: with no key
//! and no SearXNG instance you still get the three keyless sources. Those cover
//! entities and recent software/news well but are *not* a general web index —
//! set a Brave key (or point at a SearXNG instance) for that. No HTML scraping
//! anywhere — JSON APIs only, so there's no parser to keep re-fixing when
//! someone's markup changes.

use std::collections::HashMap;
use std::fmt::Write as _;

use serde_json::Value;
use smooai_fetch::{Method, RequestInit};

/// One ranked hit, whatever source it came from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SearchResult {
    pub title: String,
    pub url: String,
    pub snippet: String,
    /// Which backend produced it — surfaced in the rendered output so the model
    /// (and we, when debugging) can see the coverage it actually got.
    pub source: &'static str,
}

/// Rank weight per source: a real web index outranks an encyclopedia.
fn weight(source: &str) -> f64 {
    match source {
        "brave" => 1.0,
        "searxng" => 0.95,
        "duckduckgo" => 0.7,
        "hackernews" => 0.65,
        _ => 0.6, // wikipedia
    }
}

/// Search every configured source concurrently and return merged, ranked hits.
///
/// Never errors on a source failure — a dead/unconfigured backend contributes
/// nothing and the rest still answer. An empty `Vec` means every source came
/// back empty.
pub async fn search(query: &str, max: usize) -> Vec<SearchResult> {
    let (brave, searxng, ddg, wiki, hn) = tokio::join!(
        brave(query, max),
        searxng(query),
        duckduckgo(query),
        wikipedia(query, max),
        hacker_news(query, max)
    );
    merge_rank(vec![brave, searxng, ddg, wiki, hn], max)
}

/// True when a keyed real-index source is configured — the "good path".
#[must_use]
pub fn has_web_index() -> bool {
    brave_key().is_some() || searxng_base().is_some()
}

fn brave_key() -> Option<String> {
    ["SMOOTH_BRAVE_API_KEY", "BRAVE_API_KEY", "BRAVE_SEARCH_API_KEY"]
        .iter()
        .find_map(|k| std::env::var(k).ok())
        .filter(|v| !v.trim().is_empty())
}

fn searxng_base() -> Option<String> {
    std::env::var("SMOOTH_SEARXNG_URL")
        .ok()
        .map(|u| u.trim_end_matches('/').to_owned())
        .filter(|v| !v.is_empty())
}

// ---------------------------------------------------------------- transport

/// GET a JSON document, or `None` on any failure (network, non-2xx, non-JSON).
/// Uses the house resilient client (retries/timeout/backoff) rather than raw
/// reqwest.
async fn get_json(url: &str, headers: HashMap<String, String>) -> Option<Value> {
    let init = RequestInit {
        method: Method::GET,
        headers,
        body: None,
    };
    match smooai_fetch::fetch::<Value>(url, init).await {
        Ok(resp) if resp.ok => resp.data,
        Ok(resp) => {
            tracing::debug!(url, status = resp.status, "search source returned non-2xx");
            None
        }
        Err(err) => {
            tracing::debug!(url, %err, "search source failed");
            None
        }
    }
}

/// Wikipedia (and good manners generally) wants a real User-Agent.
fn base_headers() -> HashMap<String, String> {
    let mut h = HashMap::new();
    h.insert("accept".to_owned(), "application/json".to_owned());
    h.insert(
        "user-agent".to_owned(),
        concat!("smooth-th/", env!("CARGO_PKG_VERSION"), " (https://smoo.ai)").to_owned(),
    );
    h
}

// ------------------------------------------------------------------ sources

async fn brave(query: &str, max: usize) -> Vec<SearchResult> {
    let Some(key) = brave_key() else { return Vec::new() };
    let url = format!(
        "https://api.search.brave.com/res/v1/web/search?q={}&count={}",
        urlencoding::encode(query),
        max.clamp(1, 20)
    );
    let mut headers = base_headers();
    headers.insert("x-subscription-token".to_owned(), key);
    get_json(&url, headers).await.map(|v| parse_brave(&v)).unwrap_or_default()
}

async fn searxng(query: &str) -> Vec<SearchResult> {
    let Some(base) = searxng_base() else { return Vec::new() };
    let url = format!("{base}/search?q={}&format=json", urlencoding::encode(query));
    get_json(&url, base_headers()).await.map(|v| parse_searxng(&v)).unwrap_or_default()
}

async fn duckduckgo(query: &str) -> Vec<SearchResult> {
    let url = format!(
        "https://api.duckduckgo.com/?q={}&format=json&no_html=1&skip_disambig=1",
        urlencoding::encode(query)
    );
    get_json(&url, base_headers()).await.map(|v| parse_ddg(&v)).unwrap_or_default()
}

async fn wikipedia(query: &str, max: usize) -> Vec<SearchResult> {
    let url = format!(
        "https://en.wikipedia.org/w/api.php?action=query&list=search&srsearch={}&srlimit={}&format=json&origin=*",
        urlencoding::encode(query),
        max.clamp(1, 10)
    );
    get_json(&url, base_headers()).await.map(|v| parse_wikipedia(&v)).unwrap_or_default()
}

/// Hacker News (Algolia) — keyless, and the one keyless source with *recency*
/// for software/news queries, which is most of what Big Smooth gets asked.
async fn hacker_news(query: &str, max: usize) -> Vec<SearchResult> {
    let url = format!(
        "https://hn.algolia.com/api/v1/search?query={}&tags=story&hitsPerPage={}",
        urlencoding::encode(query),
        max.clamp(1, 10)
    );
    get_json(&url, base_headers()).await.map(|v| parse_hn(&v)).unwrap_or_default()
}

// ------------------------------------------------------------------ parsers

/// Brave: `web.results[] { title, url, description }`.
fn parse_brave(v: &Value) -> Vec<SearchResult> {
    v["web"]["results"]
        .as_array()
        .map(|rs| rs.iter().filter_map(|r| hit(r, "title", "url", "description", "brave")).collect())
        .unwrap_or_default()
}

/// SearXNG JSON format: `results[] { title, url, content }`.
fn parse_searxng(v: &Value) -> Vec<SearchResult> {
    v["results"]
        .as_array()
        .map(|rs| rs.iter().filter_map(|r| hit(r, "title", "url", "content", "searxng")).collect())
        .unwrap_or_default()
}

/// One `RelatedTopics` entry → a result, if it carries a URL.
fn push_topic(t: &Value, out: &mut Vec<SearchResult>) {
    if let (Some(url), Some(text)) = (t["FirstURL"].as_str(), t["Text"].as_str()) {
        if !url.is_empty() {
            // "Title - the rest of the blurb" is DDG's convention.
            let (title, snippet) = text.split_once(" - ").unwrap_or((text, text));
            out.push(SearchResult {
                title: title.trim().to_owned(),
                url: url.to_owned(),
                snippet: strip_html(snippet.trim()),
                source: "duckduckgo",
            });
        }
    }
}

/// DuckDuckGo Instant Answer: the abstract (when there is one) plus the flat and
/// nested `RelatedTopics`, which carry `Text` + `FirstURL`.
fn parse_ddg(v: &Value) -> Vec<SearchResult> {
    let mut out = Vec::new();
    let abstract_url = v["AbstractURL"].as_str().unwrap_or_default();
    if !abstract_url.is_empty() {
        let text = first_nonempty(&[v["AbstractText"].as_str(), v["Abstract"].as_str(), v["Answer"].as_str()]);
        out.push(SearchResult {
            title: first_nonempty(&[v["Heading"].as_str()]),
            url: abstract_url.to_owned(),
            snippet: strip_html(&text),
            source: "duckduckgo",
        });
    }
    for topic in v["RelatedTopics"].as_array().map(Vec::as_slice).unwrap_or_default() {
        if let Some(nested) = topic["Topics"].as_array() {
            for t in nested {
                push_topic(t, &mut out);
            }
        } else {
            push_topic(topic, &mut out);
        }
    }
    out
}

/// HN Algolia: `hits[] { title, url, story_text, points, created_at }`. Stories
/// with no outbound `url` (Ask HN &c.) fall back to their HN discussion page.
fn parse_hn(v: &Value) -> Vec<SearchResult> {
    v["hits"]
        .as_array()
        .map(|hits| {
            hits.iter()
                .filter_map(|h| {
                    let title = h["title"].as_str().unwrap_or_default().trim();
                    if title.is_empty() {
                        return None;
                    }
                    let url = match h["url"].as_str().map(str::trim).filter(|u| !u.is_empty()) {
                        Some(u) => u.to_owned(),
                        None => format!("https://news.ycombinator.com/item?id={}", h["objectID"].as_str()?),
                    };
                    let date = h["created_at"].as_str().unwrap_or_default().get(..10).unwrap_or_default();
                    Some(SearchResult {
                        title: strip_html(title),
                        url,
                        snippet: format!("Hacker News, {date} — {} points", h["points"].as_u64().unwrap_or_default()),
                        source: "hackernews",
                    })
                })
                .collect()
        })
        .unwrap_or_default()
}

/// MediaWiki search: `query.search[] { title, snippet }` (snippet is HTML).
fn parse_wikipedia(v: &Value) -> Vec<SearchResult> {
    v["query"]["search"]
        .as_array()
        .map(|rs| {
            rs.iter()
                .filter_map(|r| {
                    let title = r["title"].as_str()?.trim();
                    if title.is_empty() {
                        return None;
                    }
                    Some(SearchResult {
                        title: title.to_owned(),
                        url: format!("https://en.wikipedia.org/wiki/{}", urlencoding::encode(&title.replace(' ', "_"))),
                        snippet: strip_html(r["snippet"].as_str().unwrap_or_default()),
                        source: "wikipedia",
                    })
                })
                .collect()
        })
        .unwrap_or_default()
}

/// Shared `{title, url, snippet}` extraction for the JSON-shaped web indexes.
fn hit(r: &Value, title_key: &str, url_key: &str, snippet_key: &str, source: &'static str) -> Option<SearchResult> {
    let url = r[url_key].as_str()?.trim();
    if url.is_empty() {
        return None;
    }
    Some(SearchResult {
        title: strip_html(r[title_key].as_str().unwrap_or_default().trim()),
        url: url.to_owned(),
        snippet: strip_html(r[snippet_key].as_str().unwrap_or_default().trim()),
        source,
    })
}

fn first_nonempty(candidates: &[Option<&str>]) -> String {
    candidates.iter().flatten().find(|s| !s.trim().is_empty()).unwrap_or(&"").trim().to_owned()
}

// ------------------------------------------------------------- merge + rank

/// Merge per-source hit lists into one ranked list.
///
/// Score = source weight, decayed by the hit's position within its own source,
/// plus a consensus bonus each time another source returns the same URL. Dedup
/// is on a normalized URL (scheme/`www.`/trailing-slash-insensitive).
fn merge_rank(groups: Vec<Vec<SearchResult>>, max: usize) -> Vec<SearchResult> {
    let mut scored: Vec<(f64, SearchResult)> = Vec::new();
    let mut seen: HashMap<String, usize> = HashMap::new();
    for group in groups {
        for (idx, r) in group.into_iter().enumerate() {
            #[allow(clippy::cast_precision_loss, reason = "result indexes are tiny; f64 is exact here")]
            let score = weight(r.source) / (idx as f64).mul_add(0.1, 1.0);
            let key = normalize_url(&r.url);
            // Same page from a second source is corroboration, not a duplicate row:
            // bump the existing entry and keep the better-scoring copy's text.
            if let Some(&i) = seen.get(&key) {
                scored[i].0 += 0.25;
                if score > weight(scored[i].1.source) {
                    scored[i].1 = r;
                } else if scored[i].1.snippet.is_empty() {
                    scored[i].1.snippet = r.snippet;
                }
            } else {
                seen.insert(key, scored.len());
                scored.push((score, r));
            }
        }
    }
    scored.sort_by(|a, b| b.0.total_cmp(&a.0));

    // No single source may fill more than half the page while another has hits:
    // a narrow source (Wikipedia on "latest tokio release notes") otherwise
    // floods out the one result that actually answered. Overflow is appended
    // afterwards, so we never return fewer than we could.
    let cap = max.div_ceil(2).max(1);
    let mut per_source: HashMap<&str, usize> = HashMap::new();
    let (mut kept, mut overflow) = (Vec::new(), Vec::new());
    for (_, r) in scored {
        let n = per_source.entry(r.source).or_default();
        if *n < cap {
            *n += 1;
            kept.push(r);
        } else {
            overflow.push(r);
        }
    }
    kept.extend(overflow);
    kept.truncate(max);
    kept
}

/// Scheme-, `www.`- and trailing-slash-insensitive URL key for dedup.
fn normalize_url(url: &str) -> String {
    let u = url.trim().to_lowercase();
    let u = u.strip_prefix("https://").or_else(|| u.strip_prefix("http://")).unwrap_or(&u);
    u.strip_prefix("www.").unwrap_or(u).trim_end_matches('/').to_owned()
}

/// Drop tags and decode the handful of entities the sources actually emit
/// (MediaWiki snippets are HTML; Brave descriptions carry `<strong>`).
fn strip_html(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut in_tag = false;
    for c in s.chars() {
        match c {
            '<' => in_tag = true,
            '>' => in_tag = false,
            _ if !in_tag => out.push(c),
            _ => {}
        }
    }
    let out = decode_numeric_entities(&out)
        .replace("&quot;", "\"")
        .replace("&apos;", "'")
        .replace("&nbsp;", " ")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&amp;", "&");
    out.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Decode `&#39;` / `&#039;` / `&#x2019;` — MediaWiki zero-pads its numeric
/// entities, so a fixed list of `&#39;`-style replacements misses half of them.
fn decode_numeric_entities(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(start) = rest.find("&#") {
        out.push_str(&rest[..start]);
        let after = &rest[start + 2..];
        let decoded = after.find(';').filter(|&end| end > 0 && end <= 8).and_then(|end| {
            let digits = &after[..end];
            let code = match digits.strip_prefix(['x', 'X']) {
                Some(hex) => u32::from_str_radix(hex, 16).ok()?,
                None => digits.parse().ok()?,
            };
            char::from_u32(code).map(|c| (c, end))
        });
        if let Some((c, end)) = decoded {
            out.push(c);
            rest = &after[end + 1..];
        } else {
            // Not an entity after all — keep the literal `&#` and move past it.
            out.push_str("&#");
            rest = after;
        }
    }
    out.push_str(rest);
    out
}

/// Render ranked hits the way `th search` prints them, so the model sees a
/// familiar shape regardless of which backend answered.
#[must_use]
pub fn render(query: &str, results: &[SearchResult]) -> String {
    if results.is_empty() {
        return format!(
            "No results for {query:?}.\n\nOnly keyless sources (DuckDuckGo Instant Answer, Hacker News, Wikipedia) are configured and none had \
             coverage for this query. Set SMOOTH_BRAVE_API_KEY (Brave Search API, free tier) or SMOOTH_SEARXNG_URL for full web-index coverage."
        );
    }
    let mut out = String::new();
    for (i, r) in results.iter().enumerate() {
        let title = if r.title.is_empty() { "(untitled)" } else { &r.title };
        let _ = writeln!(out, "{}. {title} [{}]\n   {}", i + 1, r.source, r.url);
        if !r.snippet.is_empty() {
            let snippet: String = r.snippet.chars().take(300).collect();
            let _ = writeln!(out, "   {snippet}");
        }
    }
    out
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, reason = "unwrap/expect are the idiom for test assertions")]
mod tests {
    use serde_json::json;

    use super::*;

    fn r(url: &str, source: &'static str) -> SearchResult {
        SearchResult {
            title: format!("t {url}"),
            url: url.to_owned(),
            snippet: "s".to_owned(),
            source,
        }
    }

    #[test]
    fn parses_brave_web_results() {
        let v = json!({"web": {"results": [
            {"title": "Tokio <strong>1.0</strong>", "url": "https://tokio.rs", "description": "async runtime &amp; more"},
            {"title": "no url", "description": "dropped"},
            {"title": "blank url", "url": "  ", "description": "dropped"}
        ]}});
        let got = parse_brave(&v);
        assert_eq!(got.len(), 1, "entries without a usable url are dropped");
        assert_eq!(got[0].title, "Tokio 1.0", "tags stripped");
        assert_eq!(got[0].snippet, "async runtime & more", "entities decoded");
        assert_eq!(got[0].source, "brave");
    }

    #[test]
    fn parses_searxng_results() {
        let v = json!({"results": [{"title": "T", "url": "https://e.com/a", "content": "c"}]});
        assert_eq!(
            parse_searxng(&v),
            vec![SearchResult {
                title: "T".into(),
                url: "https://e.com/a".into(),
                snippet: "c".into(),
                source: "searxng"
            }]
        );
    }

    #[test]
    fn parses_ddg_abstract_and_flat_and_nested_topics() {
        let v = json!({
            "Heading": "Rust",
            "AbstractText": "Rust is a systems language.",
            "AbstractURL": "https://en.wikipedia.org/wiki/Rust_(programming_language)",
            "RelatedTopics": [
                {"FirstURL": "https://crates.io", "Text": "crates.io - the Rust registry"},
                {"Name": "Group", "Topics": [{"FirstURL": "https://docs.rs", "Text": "docs.rs"}]},
                {"Name": "empty group"}
            ]
        });
        let got = parse_ddg(&v);
        assert_eq!(got.len(), 3, "abstract + flat topic + nested topic");
        assert_eq!(got[0].title, "Rust");
        assert_eq!(got[0].snippet, "Rust is a systems language.");
        assert_eq!(got[1].title, "crates.io");
        assert_eq!(got[1].snippet, "the Rust registry", "DDG's `Title - blurb` convention split");
        assert_eq!(got[2].url, "https://docs.rs", "nested Topics are walked");
    }

    #[test]
    fn ddg_without_abstract_yields_only_topics() {
        let v = json!({"AbstractURL": "", "RelatedTopics": [{"FirstURL": "https://a.com", "Text": "A"}]});
        let got = parse_ddg(&v);
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].url, "https://a.com");
    }

    #[test]
    fn parses_wikipedia_and_builds_article_urls() {
        let v = json!({"query": {"search": [
            {"title": "Rust (programming language)", "snippet": "<span class=\"searchmatch\">Rust</span> is fast"},
            {"title": "  ", "snippet": "dropped"}
        ]}});
        let got = parse_wikipedia(&v);
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].url, "https://en.wikipedia.org/wiki/Rust_%28programming_language%29");
        assert_eq!(got[0].snippet, "Rust is fast");
    }

    #[test]
    fn parses_hn_stories_and_falls_back_to_the_discussion_page() {
        let v = json!({"hits": [
            {"title": "Tokio 1.0", "url": "https://tokio.rs/blog", "points": 320, "created_at": "2026-01-02T03:04:05.000Z", "objectID": "1"},
            {"title": "Ask HN: rust?", "url": "", "points": 12, "created_at": "2026-02-03T00:00:00.000Z", "objectID": "42"},
            {"title": "", "url": "https://x.com", "objectID": "3"}
        ]});
        let got = parse_hn(&v);
        assert_eq!(got.len(), 2, "untitled hits are dropped");
        assert_eq!(got[0].url, "https://tokio.rs/blog");
        assert_eq!(got[0].snippet, "Hacker News, 2026-01-02 — 320 points");
        assert_eq!(got[1].url, "https://news.ycombinator.com/item?id=42", "urlless story → its HN thread");
    }

    #[test]
    fn empty_or_malformed_json_parses_to_nothing() {
        for v in [
            json!({}),
            json!({"web": 3}),
            json!({"query": {"search": "nope"}}),
            json!({"hits": 1}),
            json!(null),
        ] {
            assert!(
                parse_brave(&v).is_empty()
                    && parse_searxng(&v).is_empty()
                    && parse_ddg(&v).is_empty()
                    && parse_wikipedia(&v).is_empty()
                    && parse_hn(&v).is_empty()
            );
        }
    }

    #[test]
    fn merge_ranks_web_index_above_keyless_sources() {
        let merged = merge_rank(vec![vec![r("https://a.com", "wikipedia")], vec![r("https://b.com", "brave")]], 10);
        assert_eq!(merged[0].url, "https://b.com", "brave outranks wikipedia despite ordering");
        assert_eq!(merged.len(), 2);
    }

    #[test]
    fn merge_dedups_across_sources_and_prefers_the_stronger_source() {
        let merged = merge_rank(
            vec![
                vec![r("https://en.wikipedia.org/wiki/Rust", "wikipedia")],
                vec![r("http://WWW.en.wikipedia.org/wiki/Rust/", "brave")],
            ],
            10,
        );
        assert_eq!(merged.len(), 1, "same page modulo scheme/www/trailing slash");
        assert_eq!(merged[0].source, "brave", "stronger source's copy wins");
    }

    #[test]
    fn merge_gives_corroborated_results_a_bonus() {
        // Two keyless sources agreeing (0.7 + 0.25) beat one lone searxng hit (0.95/1.1).
        let merged = merge_rank(
            vec![
                vec![r("https://x.com", "duckduckgo")],
                vec![r("https://ignored.com", "searxng"), r("https://x.com", "searxng")],
            ],
            10,
        );
        assert_eq!(merged.len(), 2);
        assert_eq!(merged[0].url, "https://x.com", "corroboration outranks a single mid-list hit");
    }

    #[test]
    fn merge_keeps_a_snippet_when_the_winning_source_lacks_one() {
        let mut blank = r("https://x.com", "brave");
        blank.snippet = String::new();
        let merged = merge_rank(vec![vec![blank], vec![r("https://x.com", "wikipedia")]], 10);
        assert_eq!(merged[0].snippet, "s", "backfilled from the corroborating source");
    }

    #[test]
    fn merge_respects_position_decay_within_a_source() {
        let merged = merge_rank(vec![vec![r("https://1.com", "brave"), r("https://2.com", "brave")]], 10);
        assert_eq!(merged[0].url, "https://1.com");
    }

    #[test]
    fn merge_truncates_to_max() {
        let group: Vec<_> = (0..10).map(|i| r(&format!("https://{i}.com"), "brave")).collect();
        assert_eq!(merge_rank(vec![group], 3).len(), 3);
    }

    #[test]
    fn render_lists_results_with_source_labels() {
        let out = render("q", &[r("https://a.com", "brave")]);
        assert!(out.contains("1. t https://a.com [brave]"), "{out}");
        assert!(out.contains("https://a.com"));
    }

    #[test]
    fn render_explains_how_to_get_better_coverage_when_empty() {
        let out = render("obscure thing", &[]);
        assert!(out.contains("No results"));
        assert!(out.contains("SMOOTH_BRAVE_API_KEY"), "tells the user how to widen coverage");
    }

    #[test]
    fn merge_caps_a_single_source_at_half_the_page_when_others_answered() {
        let wiki: Vec<_> = (0..6).map(|i| r(&format!("https://w{i}.com"), "wikipedia")).collect();
        let hn = vec![r("https://hn.com", "hackernews")];
        let merged = merge_rank(vec![wiki, hn], 6);
        assert_eq!(merged.len(), 6, "overflow backfills the page");
        let hn_rank = merged.iter().position(|r| r.source == "hackernews").unwrap();
        assert!(hn_rank < 3, "the narrow source can't flood out the other one (rank {hn_rank})");
        assert_eq!(merged.iter().take(4).filter(|r| r.source == "wikipedia").count(), 3, "capped at ceil(6/2)");
    }

    #[test]
    fn merge_still_fills_the_page_from_one_source_when_it_is_the_only_one() {
        let only: Vec<_> = (0..6).map(|i| r(&format!("https://w{i}.com"), "wikipedia")).collect();
        assert_eq!(merge_rank(vec![only], 6).len(), 6);
    }

    #[test]
    fn strip_html_flattens_whitespace_and_entities() {
        assert_eq!(strip_html("<b>a</b>\n  b &amp; c"), "a b & c");
        assert_eq!(strip_html(""), "");
    }

    #[test]
    fn strip_html_decodes_padded_and_hex_numeric_entities() {
        // MediaWiki emits the zero-padded form; a `&#39;`-only replace misses it.
        assert_eq!(strip_html("Japan&#039;s chart"), "Japan's chart");
        assert_eq!(strip_html("&#39;a&#x2019;b"), "'a\u{2019}b");
    }

    #[test]
    fn strip_html_leaves_non_entities_alone() {
        assert_eq!(strip_html("a &# b"), "a &# b");
        assert_eq!(strip_html("issue &#notanumber; here"), "issue &#notanumber; here");
        assert_eq!(strip_html("&#999999999999;"), "&#999999999999;", "unparseable code point is left literal");
    }

    #[test]
    fn normalize_url_is_scheme_www_and_slash_insensitive() {
        assert_eq!(normalize_url("https://WWW.A.com/x/"), normalize_url("http://a.com/x"));
        assert_ne!(normalize_url("https://a.com/x"), normalize_url("https://a.com/y"));
    }

    #[tokio::test]
    async fn search_degrades_to_keyless_sources_without_a_key() {
        // No env keys → the keyed sources contribute nothing and never touch the
        // network. (The keyless ones are exercised by the #[ignore]d live test.)
        temp_env_unset(&["SMOOTH_BRAVE_API_KEY", "BRAVE_API_KEY", "BRAVE_SEARCH_API_KEY", "SMOOTH_SEARXNG_URL"]);
        assert!(brave_key().is_none() && searxng_base().is_none());
        assert!(!has_web_index(), "no keyed index configured");
        assert!(brave("q", 5).await.is_empty(), "brave is skipped, not attempted");
        assert!(searxng("q").await.is_empty(), "searxng is skipped, not attempted");
    }

    /// `std::env::remove_var` is unsafe in edition 2024 but not 2021; this crate
    /// forbids `unsafe`, so keep the calls behind one helper for when it moves.
    fn temp_env_unset(keys: &[&str]) {
        for k in keys {
            std::env::remove_var(k);
        }
    }

    #[tokio::test]
    #[ignore = "hits the live network; run with --ignored"]
    async fn live_keyless_search_returns_results() {
        let got = Box::pin(search("rust programming language", 5)).await;
        assert!(!got.is_empty(), "keyless sources should answer a mainstream query");
    }
}
