//! SmoothFlow Desktop's toolkit-free core (th-032792): everything the window
//! does that isn't drawing. The GPUI binary (`src/main.rs`, `view`,
//! `workspace`) wraps [`app_core::Core`], and the end-to-end test
//! (`tests/e2e.rs`) drives the very same `Core` against a real daemon, so the
//! test exercises the code the GUI runs, not a copy of it.

pub mod app_core;
pub mod discovery;
pub mod field;
pub mod frames;
pub mod ghostty;
pub mod http;
pub mod keys;
pub mod layout;
pub mod net;
pub mod sheet;
pub mod terminal;
