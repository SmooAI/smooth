//! Client-side reverse-tunnel daemon for th.smoo.ai.
//!
//! # Product shape
//!
//! The user runs Big Smooth locally on `127.0.0.1:4400`. They want a
//! publicly-reachable URL that proxies HTTP + WebSocket traffic back to
//! that local process — to hand a pearl off to a remote reviewer, to
//! drive Smooth from a phone, or to let a teammate join a session
//! without a VPN handshake.
//!
//! Spiritually: ngrok, Cloudflare Tunnel, Tailscale Funnel. The `th`
//! daemon opens **one** outbound persistent connection to th.smoo.ai;
//! the rendezvous service gives back an ephemeral subdomain
//! (`scratch-3f21.th.smoo.ai`); inbound requests get multiplexed back
//! through that one connection onto the local Big Smooth.
//!
//! # Two processes, one contract
//!
//! This crate is only the client half. The tunnel service lives in the
//! smooai monorepo (Jira SMOODEV-637, ECS Fargate). Both sides speak
//! the wire protocol in [`protocol`] — that's the stable seam.
//!
//! # Cast placement
//!
//! Big Smooth never talks to th.smoo.ai directly — he stays on the
//! host. The `th` daemon is the one walking out to the public internet
//! and back (same pattern as Bootstrap Bill walks between worlds for
//! the microVM boundary, but on the network edge instead of the
//! hypervisor edge).
//!
//! # Scope of THIS crate today
//!
//! - Wire protocol types + serde ([`protocol`]).
//! - Slug generation + validation ([`slug`]).
//! - Client config + builder ([`client::TunnelClient`]).
//!
//! The dial + handshake + HTTP multiplex loop is live
//! ([`client::TunnelClient::run`]); WebSocket-stream proxying is the
//! remaining follow-up (inbound WS streams are rejected with a
//! `StreamClose`). Pearl th-e82dac.

#![forbid(unsafe_code)]

pub mod client;
pub mod protocol;
pub mod slug;

pub use client::{TunnelClient, TunnelConfig};
pub use protocol::{ClientHello, ControlFrame, ServerHello, StreamClose, StreamData, StreamKind, StreamOpen, WireFrame};
pub use slug::{SlugError, SlugPreference};

/// Every error the client can surface to the CLI. Kept narrow on
/// purpose — the CLI turns these into human messages, and the wire
/// errors come through `ControlFrame::Error`.
#[derive(Debug, thiserror::Error)]
pub enum TunnelError {
    /// Config validation failed before any network work.
    #[error("tunnel config invalid: {0}")]
    InvalidConfig(String),

    /// The slug the user requested didn't pass local validation.
    #[error("slug preference: {0}")]
    InvalidSlug(#[from] SlugError),

    /// The tunnel service replied with a protocol-level error.
    #[error("tunnel service error ({code}): {message}")]
    ServiceError { code: String, message: String },

    /// Transport died. Kept as a catch-all so callers don't need to
    /// enumerate every `tokio-tungstenite` error variant.
    #[error("tunnel transport: {0}")]
    Transport(String),
}

/// Result alias used across the tunnel client.
pub type Result<T> = std::result::Result<T, TunnelError>;
