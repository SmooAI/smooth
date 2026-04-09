//! forward_port tool — lets the operator expose a guest port to the host.
//!
//! At VM creation, Big Smooth maps declared ports and injects them as
//! SMOOTH_PORT_MAP=guest:host,guest:host. This tool reads that map and
//! tells the LLM agent which host port corresponds to a guest port.

use anyhow::Result;
use async_trait::async_trait;
use serde_json::json;
use smooth_operator::tool::{Tool, ToolSchema};
use std::collections::HashMap;
use std::sync::OnceLock;

pub struct ForwardPortTool;

static PORT_MAP: OnceLock<HashMap<u16, u16>> = OnceLock::new();

fn load_port_map() -> &'static HashMap<u16, u16> {
    PORT_MAP.get_or_init(|| {
        let raw = std::env::var("SMOOTH_PORT_MAP").unwrap_or_default();
        let mut map = HashMap::new();
        for pair in raw.split(',') {
            let pair = pair.trim();
            if pair.is_empty() {
                continue;
            }
            let parts: Vec<&str> = pair.split(':').collect();
            if parts.len() == 2 {
                if let (Ok(guest), Ok(host)) = (parts[0].parse::<u16>(), parts[1].parse::<u16>()) {
                    map.insert(guest, host);
                }
            }
        }
        map
    })
}

#[async_trait]
impl Tool for ForwardPortTool {
    fn schema(&self) -> ToolSchema {
        ToolSchema {
            name: "forward_port".into(),
            description: "Expose a port running inside this sandbox to the host network. Returns the host port that external clients can connect to. Use this after starting a dev server or any network service.".into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "guest_port": {
                        "type": "integer",
                        "description": "The port number your service is listening on inside this sandbox (e.g. 3000 for a Node.js dev server)"
                    }
                },
                "required": ["guest_port"]
            }),
        }
    }

    async fn execute(&self, arguments: serde_json::Value) -> Result<String> {
        let guest_port = arguments
            .get("guest_port")
            .and_then(|v| v.as_u64())
            .map(|v| v as u16)
            .ok_or_else(|| anyhow::anyhow!("guest_port is required and must be a number"))?;

        let port_map = load_port_map();

        if let Some(&host_port) = port_map.get(&guest_port) {
            Ok(format!(
                "Port {guest_port} is forwarded to host port {host_port}. External clients can connect to http://localhost:{host_port}"
            ))
        } else {
            let available: Vec<String> = port_map.iter().map(|(g, h)| format!("{g} -> host:{h}")).collect();
            if available.is_empty() {
                Err(anyhow::anyhow!(
                    "No port forwards are configured for this sandbox. Port forwarding must be declared in the task policy."
                ))
            } else {
                Err(anyhow::anyhow!(
                    "Port {guest_port} is not forwarded. Available forwards: {}",
                    available.join(", ")
                ))
            }
        }
    }

    fn is_read_only(&self) -> bool {
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use smooth_operator::tool::Tool;

    #[tokio::test]
    async fn forward_port_no_env_var() {
        // No SMOOTH_PORT_MAP set — should error
        let tool = ForwardPortTool;
        let result = tool.execute(json!({"guest_port": 3000})).await;
        assert!(result.is_err());
    }

    #[test]
    fn schema_has_required_fields() {
        let tool = ForwardPortTool;
        let schema = tool.schema();
        assert_eq!(schema.name, "forward_port");
        assert!(schema.description.contains("sandbox"));
    }
}
