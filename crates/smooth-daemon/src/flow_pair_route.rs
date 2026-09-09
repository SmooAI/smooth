//! `/api/flow/pair*` — phone pairing for end-to-end encrypted relay frames
//! (pearl th-d98fde). The macOS app's Settings ▸ Phones pane and
//! `th flow pair` are the clients.
//!
//! - `POST /api/flow/pair` → mint a QR: `{pairing_id, url, code, device,
//!   label, daemon_public_key, expires_at, relay_enabled}`
//! - `GET /api/flow/pair/{id}` → `{state: pending|paired|expired|unknown, …}`
//! - `GET /api/flow/pairings` → `{pairings: [{device, label, platform,
//!   public_key, created_at, last_seen_at}]}` (keys never leave the daemon)
//! - `DELETE /api/flow/pairings/{device}` → `{revoked: bool}`
//!
//! All gated by the daemon's local token, like every other flow route.

use std::collections::HashMap;
use std::sync::Arc;

use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::routing::{delete, get, post};
use axum::{Json, Router};
use serde_json::{json, Value};

use crate::flow_e2e::PairingState;
use crate::flow_route::authorized;

type ApiErr = (StatusCode, Json<Value>);

/// Shared state for the pair routes.
#[derive(Clone)]
pub struct PairRouteState {
    pairing: Arc<PairingState>,
    token: Option<Arc<String>>,
    relay_enabled: bool,
}

/// The router. `token = None` disables the gate (tests only).
pub fn pair_router(pairing: Arc<PairingState>, token: Option<String>, relay_enabled: bool) -> Router {
    let state = PairRouteState {
        pairing,
        token: token.map(Arc::new),
        relay_enabled,
    };
    Router::new()
        .route("/api/flow/pair", post(begin))
        .route("/api/flow/pair/{id}", get(status))
        .route("/api/flow/pairings", get(list))
        .route("/api/flow/pairings/{device}", delete(revoke))
        .with_state(state)
}

fn gate(state: &PairRouteState, headers: &HeaderMap, query: &HashMap<String, String>) -> Result<(), ApiErr> {
    if authorized(state.token.as_deref().map(String::as_str), headers, query) {
        Ok(())
    } else {
        Err((StatusCode::UNAUTHORIZED, Json(json!({"error":"missing or invalid local token"}))))
    }
}

async fn begin(State(st): State<PairRouteState>, headers: HeaderMap, Query(q): Query<HashMap<String, String>>) -> Result<Json<Value>, ApiErr> {
    gate(&st, &headers, &q)?;
    let qr = st.pairing.begin();
    let expires_at = match st.pairing.status(&qr.pairing_id) {
        crate::flow_e2e::PairStatus::Pending { expires_at } => Some(expires_at),
        _ => None,
    };
    Ok(Json(json!({
        "pairing_id": qr.pairing_id,
        "url": qr.to_url(),
        "code": qr.code,
        "device": qr.daemon_device,
        "label": qr.label,
        "daemon_public_key": qr.daemon_public_key,
        "expires_at": expires_at,
        "relay_enabled": st.relay_enabled,
    })))
}

async fn status(
    State(st): State<PairRouteState>,
    Path(id): Path<String>,
    headers: HeaderMap,
    Query(q): Query<HashMap<String, String>>,
) -> Result<Json<Value>, ApiErr> {
    gate(&st, &headers, &q)?;
    let s = st.pairing.status(&id);
    let mut v = serde_json::to_value(&s).unwrap_or_else(|_| json!({"state":"unknown"}));
    if let Some(obj) = v.as_object_mut() {
        obj.insert("pairing_id".into(), Value::String(id));
    }
    Ok(Json(v))
}

async fn list(State(st): State<PairRouteState>, headers: HeaderMap, Query(q): Query<HashMap<String, String>>) -> Result<Json<Value>, ApiErr> {
    gate(&st, &headers, &q)?;
    Ok(Json(json!({
        "device": st.pairing.daemon_device(),
        "label": st.pairing.daemon_label(),
        "relay_enabled": st.relay_enabled,
        "pairings": st.pairing.list(),
    })))
}

async fn revoke(
    State(st): State<PairRouteState>,
    Path(device): Path<String>,
    headers: HeaderMap,
    Query(q): Query<HashMap<String, String>>,
) -> Result<Json<Value>, ApiErr> {
    gate(&st, &headers, &q)?;
    let revoked = st
        .pairing
        .revoke(&device)
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({"error": e.to_string()}))))?;
    Ok(Json(json!({ "device": device, "revoked": revoked })))
}

#[cfg(test)]
#[allow(clippy::unwrap_used, reason = "unwrap is the idiom for test assertions")]
mod tests {
    use axum::body::Body;
    use axum::http::Request;
    use http_body_util::BodyExt;
    use tower::ServiceExt;

    use super::*;
    use crate::flow_e2e::tests::{phone_pair_frame, state, PHONE_DEVICE};
    use crate::flow_e2e::{classify, Inbound, QrPayload};

    async fn call(router: &Router, method: &str, path: &str, token: Option<&str>) -> (StatusCode, Value) {
        let mut req = Request::builder().method(method).uri(path);
        if let Some(t) = token {
            req = req.header("x-smooth-token", t);
        }
        let resp = router.clone().oneshot(req.body(Body::empty()).unwrap()).await.unwrap();
        let status = resp.status();
        let bytes = resp.into_body().collect().await.unwrap().to_bytes();
        let v: Value = if bytes.is_empty() {
            Value::Null
        } else {
            serde_json::from_slice(&bytes).unwrap()
        };
        (status, v)
    }

    #[tokio::test]
    async fn routes_are_token_gated() {
        let router = pair_router(Arc::new(state()), Some("secret".into()), true);
        assert_eq!(call(&router, "POST", "/api/flow/pair", None).await.0, StatusCode::UNAUTHORIZED);
        assert_eq!(call(&router, "GET", "/api/flow/pairings", Some("wrong")).await.0, StatusCode::UNAUTHORIZED);
        assert_eq!(call(&router, "GET", "/api/flow/pairings", Some("secret")).await.0, StatusCode::OK);
        // Query-string token works too (the app's WS style).
        assert_eq!(call(&router, "GET", "/api/flow/pairings?token=secret", None).await.0, StatusCode::OK);
    }

    #[tokio::test]
    async fn begin_status_list_revoke_round_trip() {
        let st = Arc::new(state());
        let router = pair_router(st.clone(), None, false);
        let (code, v) = call(&router, "POST", "/api/flow/pair", None).await;
        assert_eq!(code, StatusCode::OK);
        let id = v["pairing_id"].as_str().unwrap().to_string();
        assert_eq!(v["device"], st.daemon_device());
        assert_eq!(v["relay_enabled"], false);
        assert!(v["expires_at"].is_string());
        let qr = QrPayload::parse(v["url"].as_str().unwrap()).unwrap();
        assert_eq!(qr.pairing_id, id);
        assert_eq!(qr.code, v["code"]);

        let (_, s) = call(&router, "GET", &format!("/api/flow/pair/{id}"), None).await;
        assert_eq!(s["state"], "pending");
        assert_eq!(s["pairing_id"], id);
        let (_, s) = call(&router, "GET", "/api/flow/pair/nope", None).await;
        assert_eq!(s["state"], "unknown");

        // A phone pairs (the relay path, exercised directly).
        let (frame, _) = phone_pair_frame(&qr, r#"{"type":"flow.pair.hello","label":"Pixel","platform":"android"}"#);
        let Inbound::Pair {
            pairing_id,
            phone_public_key,
            n,
            ct,
        } = classify(&frame)
        else {
            panic!()
        };
        st.complete(PHONE_DEVICE, &pairing_id, &phone_public_key, n, &ct).unwrap();

        let (_, s) = call(&router, "GET", &format!("/api/flow/pair/{id}"), None).await;
        assert_eq!(s["state"], "paired");
        assert_eq!(s["device"], PHONE_DEVICE);
        assert_eq!(s["label"], "Pixel");

        let (_, l) = call(&router, "GET", "/api/flow/pairings", None).await;
        let rows = l["pairings"].as_array().unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0]["device"], PHONE_DEVICE);
        assert_eq!(rows[0]["platform"], "android");
        assert!(rows[0].get("key_hex").is_none(), "the key must never be served: {l}");
        assert!(rows[0]["public_key"].is_string());

        let (code, r) = call(&router, "DELETE", &format!("/api/flow/pairings/{PHONE_DEVICE}"), None).await;
        assert_eq!(code, StatusCode::OK);
        assert_eq!(r["revoked"], true);
        let (_, r) = call(&router, "DELETE", &format!("/api/flow/pairings/{PHONE_DEVICE}"), None).await;
        assert_eq!(r["revoked"], false);
        let (_, l) = call(&router, "GET", "/api/flow/pairings", None).await;
        assert!(l["pairings"].as_array().unwrap().is_empty());
    }
}
