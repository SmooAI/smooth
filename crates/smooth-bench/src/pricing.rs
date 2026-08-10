//! Per-model token pricing, fetched live from the LiteLLM gateway.
//!
//! # Why this exists
//!
//! The bench used to publish a `$cost` column measured as the gateway
//! KEY's spend delta across a model's run. That number is wrong in two
//! independent ways, and a 13-model sweep shipped both (th-adf614):
//!
//! 1. **The key is shared.** Anything else billing during the window — a
//!    `th code` session, the smoo-hub daemon, another bench — lands in the
//!    figure. Measured deltas came in up to **1,324x** the cost actually
//!    attributable to the model's own calls.
//! 2. **Most routes report no cost at all.** 8 of 13 models had every
//!    per-scenario `cost_usd` at exactly `0.0`, because the gateway only
//!    returns its cost header on some routes (th-11f9bb). The delta was
//!    the ONLY signal, so the contamination was the whole number.
//!
//! Tokens do not have either problem: the engine accumulates them per turn
//! and reports them on `eventual_response`, so they are attributable by
//! construction. Multiply by the gateway's own published price and the
//! result is a real per-model cost that no concurrent traffic can move.
//!
//! When a model has no published price, cost is reported as UNKNOWN rather
//! than 0.0 — a blank cell is honest, a zero is a lie that ranks first.

use std::collections::HashMap;

use anyhow::{Context, Result};
use serde::Deserialize;

/// Input/output price in USD per token for one model.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ModelPrice {
    pub input_per_token: f64,
    pub output_per_token: f64,
}

impl ModelPrice {
    /// Cost in USD of a turn that consumed these tokens.
    #[must_use]
    #[allow(clippy::cast_precision_loss, reason = "token counts are far below f64's exact-integer range")]
    pub fn cost(self, prompt_tokens: u64, completion_tokens: u64) -> f64 {
        prompt_tokens as f64 * self.input_per_token + completion_tokens as f64 * self.output_per_token
    }
}

/// Every model the gateway will price, keyed by the id you pass as `--model`.
#[derive(Debug, Clone, Default)]
pub struct PriceBook(HashMap<String, ModelPrice>);

impl PriceBook {
    /// Price for `model`, or `None` when the gateway publishes none.
    #[must_use]
    pub fn get(&self, model: &str) -> Option<ModelPrice> {
        self.0.get(model).copied()
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.0.len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// Build directly from pairs — the seam tests use instead of the network.
    #[must_use]
    pub fn from_pairs(pairs: impl IntoIterator<Item = (String, ModelPrice)>) -> Self {
        Self(pairs.into_iter().collect())
    }

    /// Parse LiteLLM's `/v1/model/info` payload.
    ///
    /// # Errors
    /// Errors when the body isn't the expected JSON shape.
    pub fn from_model_info(body: &str) -> Result<Self> {
        #[derive(Deserialize)]
        struct Info {
            input_cost_per_token: Option<f64>,
            output_cost_per_token: Option<f64>,
        }
        #[derive(Deserialize)]
        struct Row {
            model_name: String,
            model_info: Option<Info>,
        }
        #[derive(Deserialize)]
        struct Payload {
            data: Vec<Row>,
        }
        let p: Payload = serde_json::from_str(body).context("parsing /v1/model/info")?;
        let mut map = HashMap::new();
        for r in p.data {
            if let Some(i) = r.model_info {
                // A model priced at 0/0 is unpriced, not free — skip it so it
                // reports UNKNOWN instead of winning every cost ranking.
                match (i.input_cost_per_token, i.output_cost_per_token) {
                    (Some(inp), Some(out)) if inp > 0.0 || out > 0.0 => {
                        map.insert(
                            r.model_name,
                            ModelPrice {
                                input_per_token: inp,
                                output_per_token: out,
                            },
                        );
                    }
                    _ => {}
                }
            }
        }
        Ok(Self(map))
    }

    /// Fetch the price book from a running gateway.
    ///
    /// # Errors
    /// Errors when the request fails or the body doesn't parse.
    pub async fn fetch(gateway_url: &str, key: Option<&str>) -> Result<Self> {
        // `gateway_url` is the OpenAI-compatible base (".../v1"); model info
        // hangs off it as `/v1/model/info`.
        let url = format!("{}/model/info", gateway_url.trim_end_matches('/'));
        let client = reqwest::Client::new();
        let mut req = client.get(&url);
        if let Some(k) = key {
            req = req.bearer_auth(k);
        }
        let body = req
            .send()
            .await
            .context("GET /v1/model/info")?
            .text()
            .await
            .context("reading /v1/model/info body")?;
        Self::from_model_info(&body)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = r#"{"data":[
        {"model_name":"cheap","model_info":{"input_cost_per_token":0.00000014,"output_cost_per_token":0.00000028}},
        {"model_name":"dear","model_info":{"input_cost_per_token":0.00001,"output_cost_per_token":0.00005}},
        {"model_name":"unpriced","model_info":{"input_cost_per_token":0.0,"output_cost_per_token":0.0}},
        {"model_name":"nulls","model_info":{"input_cost_per_token":null,"output_cost_per_token":null}},
        {"model_name":"no-info"}
    ]}"#;

    #[test]
    fn parses_priced_models_only() {
        let b = PriceBook::from_model_info(SAMPLE).unwrap();
        assert_eq!(b.len(), 2, "only the two genuinely priced rows");
        assert!(b.get("cheap").is_some());
        assert!(b.get("dear").is_some());
    }

    /// A 0/0 row must be UNKNOWN, never free — otherwise it sorts first on
    /// every cost ranking and looks like the best value in the lineup.
    #[test]
    fn zero_priced_model_is_unknown_not_free() {
        let b = PriceBook::from_model_info(SAMPLE).unwrap();
        assert_eq!(b.get("unpriced"), None);
        assert_eq!(b.get("nulls"), None);
        assert_eq!(b.get("no-info"), None);
    }

    #[test]
    fn costs_tokens_at_the_published_rate() {
        let b = PriceBook::from_model_info(SAMPLE).unwrap();
        let p = b.get("dear").unwrap();
        // 1000 in @ $10/M + 100 out @ $50/M = 0.01 + 0.005
        assert!((p.cost(1000, 100) - 0.015).abs() < 1e-12);
    }

    #[test]
    fn zero_tokens_cost_nothing() {
        let p = ModelPrice {
            input_per_token: 1.0,
            output_per_token: 1.0,
        };
        assert!((p.cost(0, 0) - 0.0).abs() < f64::EPSILON);
    }

    #[test]
    fn unknown_model_has_no_price() {
        let b = PriceBook::from_model_info(SAMPLE).unwrap();
        assert_eq!(b.get("nope"), None);
    }

    #[test]
    fn rejects_junk_payload() {
        assert!(PriceBook::from_model_info("not json").is_err());
    }
}
