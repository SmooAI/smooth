//! `present_plan` — surface a proposed plan to the user for revision/acceptance.
//!
//! Big Smooth's **Plan mode** is read-only (the daemon filters every mutating
//! tool out of the turn). The agent researches, then calls `present_plan` with
//! the plan as markdown. We write a `present_plan` directive onto the turn's
//! directive sink — the per-turn channel the engine drains onto
//! `eventual_response.directive` — which every face renders as an
//! accept / revise card (protocol contract:
//! `{ "type": "present_plan", "plan": "<markdown>" }`). Accepting flips the
//! conversation to Auto mode and continues execution; revising sends feedback
//! back as a normal (still read-only) Plan-mode turn.
//!
//! Same sink mechanism as `send_file` — the sink comes from
//! `ToolProviderContext.directive_sink`, which only the daemon's `ToolProvider`
//! sees, so the tool is constructed fresh each turn with the sink captured.

use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use serde_json::{json, Value};
use smooth_operator::{Tool, ToolSchema};

use crate::util::req_str;

/// `present_plan` — hand a proposed plan to the user.
pub struct PresentPlanTool {
    /// The turn's directive sink, drained by the engine onto
    /// `eventual_response.directive`.
    sink: Arc<Mutex<Value>>,
}

impl PresentPlanTool {
    pub fn new(sink: Arc<Mutex<Value>>) -> Self {
        Self { sink }
    }
}

#[async_trait]
impl Tool for PresentPlanTool {
    fn schema(&self) -> ToolSchema {
        ToolSchema {
            name: "present_plan".into(),
            description: "Present your plan to the user for review when you are in Plan mode. Pass the full plan as markdown. The user sees an accept/revise card: accepting switches the conversation to Auto mode and you carry out the plan; revising sends you feedback to refine it (still read-only). Call this once your research is done and you have a concrete plan — do NOT attempt edits in Plan mode, they are blocked.".into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "plan": { "type": "string", "description": "The proposed plan, as markdown. Be concrete: what you'll change, which files, and how you'll verify." }
                },
                "required": ["plan"]
            }),
        }
    }

    async fn execute(&self, arguments: Value) -> anyhow::Result<String> {
        let plan = req_str(&arguments, "plan")?;
        // Last-write-wins sink: a later present_plan supersedes an earlier one in
        // the same turn (the newest plan is the one the user should see). Recover
        // a poisoned lock rather than panic — the directive isn't
        // security-critical and shouldn't kill the turn.
        {
            let mut guard = self.sink.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
            *guard = json!({ "type": "present_plan", "plan": plan });
        }
        Ok("Presented the plan to the user. Wait for them to accept (which switches to Auto mode and you execute it) or send revisions.".into())
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, reason = "unwrap/expect are the idiom for test assertions")]
mod tests {
    use super::*;

    fn sink() -> Arc<Mutex<Value>> {
        Arc::new(Mutex::new(Value::Null))
    }

    #[tokio::test]
    async fn writes_a_present_plan_directive() {
        let s = sink();
        let tool = PresentPlanTool::new(Arc::clone(&s));
        let out = tool.execute(json!({ "plan": "1. Do X\n2. Do Y" })).await.unwrap();
        assert!(out.contains("Presented"), "result: {out}");
        let d = s.lock().unwrap().clone();
        assert_eq!(d["type"], "present_plan");
        assert_eq!(d["plan"], "1. Do X\n2. Do Y");
    }

    #[tokio::test]
    async fn a_missing_plan_is_an_error() {
        let s = sink();
        let tool = PresentPlanTool::new(Arc::clone(&s));
        assert!(tool.execute(json!({})).await.is_err());
        assert!(s.lock().unwrap().is_null(), "no directive when the call is invalid");
    }

    #[tokio::test]
    async fn newest_plan_wins() {
        let s = sink();
        let tool = PresentPlanTool::new(Arc::clone(&s));
        tool.execute(json!({ "plan": "first" })).await.unwrap();
        tool.execute(json!({ "plan": "second" })).await.unwrap();
        assert_eq!(s.lock().unwrap()["plan"], "second");
    }
}
