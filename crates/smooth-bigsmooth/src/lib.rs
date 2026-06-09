//! Big Smooth — orchestrator, policy generation, sandbox management, API server.

/// Tonic-generated proto types for the BigSmooth gRPC surface
/// (pearl th-893801). build.rs compiles proto/bigsmooth.proto with
/// the narc.proto types routed through smooth-narc's `pb` module.
#[allow(
    clippy::all,
    clippy::pedantic,
    clippy::nursery,
    clippy::cargo,
    unused_qualifications,
    missing_docs,
    clippy::derive_partial_eq_without_eq
)]
pub mod pb {
    tonic::include_proto!("smooth.bigsmooth.v1");
}

/// gRPC server adapter — wraps an `Orchestrator` trait
/// implementation as the proto-generated BigSmooth service.
/// Production wiring (linking the existing AppState into the trait)
/// lands in iter-3.
pub mod grpc;

/// Production Judge impl on SafehouseNarc + serve_uds helper.
/// Pearl th-893801 iter-3a.
pub mod narc_grpc;

/// Production Orchestrator impl over AccessStore. Pearl th-893801 iter-3d.
pub mod orchestrator_grpc;

/// Single-process gRPC cast bootstrap. Pearl th-893801 iter-3e.
pub mod single_process;

/// Tonic UDS client adapters for the in-VM cast. Pearl th-893801 iter-3f.
pub mod tonic_clients;

pub mod access;
pub mod audit;
pub mod safehouse;
pub mod safehouse_narc;

/// Phase 4 alias: `SafehouseNarc` keeps the legacy name on
/// the type but new code should prefer `Narc` — in the
/// single-VM model there's no "safehouse" anymore, just "the
/// Narc". Both names refer to the same struct. Pearl th-893801
/// Phase 4 iter-6a.
pub use safehouse_narc::SafehouseNarc as Narc;

/// Phase 4 module alias: in the single-VM model "safehouse"
/// is just "the cast running in the VM". `crate::vm_cast` is
/// the preferred path; `crate::safehouse` stays valid for
/// existing imports during the transition. Pearl th-893801
/// Phase 4 iter-6d.
pub use safehouse as vm_cast;

/// Phase 4 type alias: prefer `VmCastHandles` in new code.
/// Same struct as `SafehouseHandles`. Pearl th-893801 Phase 4
/// iter-6d.
pub use safehouse::SafehouseHandles as VmCastHandles;

/// Phase 4 function alias: prefer `spawn_vm_cast` in new
/// code. Same fn as `safehouse::spawn_safehouse_cast`. Pearl
/// th-893801 Phase 4 iter-6d.
pub use safehouse::spawn_safehouse_cast as spawn_vm_cast;

#[cfg(test)]
mod phase4_alias_smoke {
    //! Smoke checks that the Phase 4 aliases resolve to the
    //! same items they're meant to mirror. Pure compile-time
    //! plus a trivial runtime equality so a future rename
    //! that drops an alias produces an obvious test failure.

    #[test]
    fn narc_alias_resolves() {
        // Constructible via either name without surprises.
        let _via_alias: crate::Narc = crate::safehouse_narc::SafehouseNarc::without_llm();
    }

    #[test]
    fn vm_cast_module_is_the_safehouse_module() {
        // If `crate::vm_cast` and `crate::safehouse` diverge,
        // this assertion picks one or the other arbitrarily —
        // the real check is that both type paths resolve to
        // the same type via the alias.
        fn _accepts_handles(_h: crate::vm_cast::SafehouseHandles) {}
        fn _alias_accepts_handles(_h: crate::VmCastHandles) {}
    }
}
pub mod chat_tools;
pub mod creds;
pub mod host_tools;
pub mod teammates;

pub mod diver_client;
pub mod events;
pub mod jira;
pub mod operative_client;
pub mod orchestrator;
pub mod pearls;
pub mod policy;
pub mod pool;
pub mod port_cache;
pub mod sandbox;
pub mod search;
pub mod server;
pub mod session;
pub mod tailscale;
pub mod thoughts;
pub mod tool_api;
pub mod tools;
pub mod web_search;
pub mod wonk_grants;
pub mod ws;
