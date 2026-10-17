//! Prepaid AI credits (smooai SMOODEV-3966 / ADR-155): the top-up link every
//! Smooth surface hands a user who has run out of AI budget, and the pack
//! sizes the copy names.
//!
//! When an org's gateway budget (its monthly allowance plus any credit) runs
//! out, LiteLLM refuses the call with `Budget has been exceeded! Team=…`. Since
//! smooai SMOODEV-3967 the gateway answers that as a **402** whose message ends
//! with the top-up URL; before it, a 429 with the raw text. [`explain`] turns
//! either into one actionable line, so `th code`, `th harness add --agentic` and
//! every other canonical-protocol client say what to do instead of dumping the
//! gateway's JSON.

/// The dashboard's AI credits card — smooai `AI_CREDITS_TOP_UP_PATH` on the
/// production web origin. Used when the error carries no link of its own.
pub const TOP_UP_URL: &str = "https://smoo.ai/apps/llm-gateway#ai-credits";

/// The packs a customer can buy, in cents — smooai `AI_CREDIT_PACKS_CENTS`. The
/// server validates the pack, so a drift here is a refused request, not a wrong charge.
pub const PACKS_CENTS: [u32; 2] = [500, 1000];

/// The phrase LiteLLM puts on every budget refusal (key, team, user, org).
const BUDGET_EXCEEDED: &str = "Budget has been exceeded";
/// The error `type` LiteLLM sets on the same refusal.
const BUDGET_EXCEEDED_TYPE: &str = "\"budget_exceeded\"";

/// `$5` for 500 cents, `$2.50` for 250.
#[must_use]
pub fn dollars(cents: u32) -> String {
    if cents % 100 == 0 {
        format!("${}", cents / 100)
    } else {
        format!("${}.{:02}", cents / 100, cents % 100)
    }
}

/// "$5 or $10" — the packs, in words (smooai `aiCreditPacksInWords`).
#[must_use]
pub fn packs_in_words() -> String {
    let amounts: Vec<String> = PACKS_CENTS.iter().map(|c| dollars(*c)).collect();
    match amounts.split_last() {
        Some((last, rest)) if !rest.is_empty() => format!("{} or {last}", rest.join(", ")),
        Some((last, _)) => last.clone(),
        None => String::new(),
    }
}

/// "Top up $5 or $10 anytime; auto-reload optional" (smooai `aiCreditsTopUpLine`).
#[must_use]
pub fn top_up_line(qualifier: &str) -> String {
    format!("Top up {} anytime{qualifier}; auto-reload optional", packs_in_words())
}

/// Is this a gateway budget refusal?
#[must_use]
pub fn is_budget_exceeded(text: &str) -> bool {
    text.contains(BUDGET_EXCEEDED) || text.contains(BUDGET_EXCEEDED_TYPE)
}

/// The top-up link the gateway put in the error, else [`TOP_UP_URL`].
#[must_use]
pub fn top_up_url_in(text: &str) -> &str {
    text.match_indices("https://")
        .map(|(i, _)| {
            let rest = &text[i..];
            let end = rest
                .find(|c: char| c.is_whitespace() || matches!(c, '"' | '\'' | '\\' | ')' | ']' | '}' | ','))
                .unwrap_or(rest.len());
            &rest[..end]
        })
        .find(|url| url.contains("#ai-credits"))
        .unwrap_or(TOP_UP_URL)
}

/// The one line to show for a budget refusal, or `None` when `text` is not one.
#[must_use]
pub fn budget_exceeded_hint(text: &str) -> Option<String> {
    is_budget_exceeded(text).then(|| {
        format!(
            "Out of AI budget on the Smoo AI gateway. Add AI credits ({}; auto-reload optional): {} or `smoo credits buy`",
            packs_in_words(),
            top_up_url_in(text)
        )
    })
}

/// A user-facing error line: the top-up hint for a budget refusal, `text` unchanged otherwise.
#[must_use]
pub fn explain(text: &str) -> String {
    budget_exceeded_hint(text).unwrap_or_else(|| text.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The error string `smooth-operator`'s LLM client builds from the post-SMOODEV-3967 gateway.
    const GATEWAY_402: &str = r#"LLM API error 402 Payment Required: {"error":{"message":"Budget has been exceeded! Team=org-1 Current cost: 2.31, Max budget: 2.3 Out of AI budget — top up AI credits: https://smoo.ai/apps/llm-gateway#ai-credits","type":"budget_exceeded","param":null,"code":"402"}}"#;
    /// The same refusal from a gateway that predates the callback.
    const GATEWAY_429: &str = r#"LLM API error 429 Too Many Requests: {"error":{"message":"Budget has been exceeded! Team=org-1 Current cost: 2.31, Max budget: 2.3","type":"budget_exceeded","param":null,"code":"429"}}"#;

    #[test]
    fn dollars_formats_whole_and_fractional_amounts() {
        assert_eq!(dollars(500), "$5");
        assert_eq!(dollars(1000), "$10");
        assert_eq!(dollars(250), "$2.50");
        assert_eq!(dollars(5), "$0.05");
    }

    #[test]
    fn packs_read_as_words() {
        assert_eq!(packs_in_words(), "$5 or $10");
        assert_eq!(
            top_up_line(" during the trial"),
            "Top up $5 or $10 anytime during the trial; auto-reload optional"
        );
    }

    #[test]
    fn top_up_url_mirrors_the_dashboard_path() {
        assert!(TOP_UP_URL.starts_with("https://smoo.ai/"));
        assert!(TOP_UP_URL.ends_with("/apps/llm-gateway#ai-credits"));
    }

    #[test]
    fn recognises_both_gateway_shapes() {
        assert!(is_budget_exceeded(GATEWAY_402));
        assert!(is_budget_exceeded(GATEWAY_429));
        assert!(is_budget_exceeded(r#"{"type":"budget_exceeded"}"#));
    }

    #[test]
    fn leaves_other_errors_alone() {
        for other in [
            "LLM API error 401 Unauthorized: invalid api key",
            "LLM API error 429 Too Many Requests: rate limit reached for requests",
            "VALIDATION_ERROR: missing 'action' field",
            "",
        ] {
            assert!(!is_budget_exceeded(other), "{other}");
            assert_eq!(explain(other), other);
        }
    }

    #[test]
    fn prefers_the_link_the_gateway_sent() {
        let custom = GATEWAY_402.replace("https://smoo.ai/", "https://staging.smoo.ai/");
        assert_eq!(top_up_url_in(&custom), "https://staging.smoo.ai/apps/llm-gateway#ai-credits");
        assert_eq!(top_up_url_in(GATEWAY_402), TOP_UP_URL);
    }

    #[test]
    fn falls_back_to_the_dashboard_when_the_error_has_no_link() {
        assert_eq!(top_up_url_in(GATEWAY_429), TOP_UP_URL);
        // A link that is not the credits card is not a top-up link.
        assert_eq!(top_up_url_in("Budget has been exceeded, see https://docs.litellm.ai/budgets"), TOP_UP_URL);
    }

    #[test]
    fn hint_is_one_actionable_line() {
        for raw in [GATEWAY_402, GATEWAY_429] {
            let hint = explain(raw);
            assert!(hint.starts_with("Out of AI budget"), "{hint}");
            assert!(hint.contains(TOP_UP_URL), "{hint}");
            assert!(hint.contains("$5 or $10"), "{hint}");
            assert!(hint.contains("smoo credits buy"), "{hint}");
            assert!(!hint.contains('{'), "no raw JSON reaches the user: {hint}");
            assert!(!hint.contains('\n'), "{hint}");
        }
    }

    #[test]
    fn explaining_twice_is_stable() {
        let once = explain(GATEWAY_429);
        // The hint itself is not a budget refusal, so a second pass leaves it be.
        assert_eq!(explain(&once), once);
    }
}
