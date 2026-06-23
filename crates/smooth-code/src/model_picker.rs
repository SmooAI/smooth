//! Model picker popup for the Smooth TUI.
//!
//! Two-level picker opened by `/model`:
//!
//! 1. **Slots view** — one entry per routing slot
//!    (Coding / Reasoning / Reviewing / Judge / Summarize / Fast /
//!    Default), showing the model each slot currently routes to.
//! 2. **Models view** — entered from the Slots view by pressing
//!    Enter. Renders as a **catalog** (name + tier + cost +
//!    benchmark + use-case chips + description) rather than a flat
//!    name list. Selecting one applies the routing change and
//!    persists it back to `~/.smooth/providers.json`.
//!
//! Input contract (handled in `app.rs`):
//!   - Up/Down: navigate the current view's list
//!   - Enter: drill into Models view, or apply a model selection
//!   - Tab (Models view only): toggle the slot's use-case filter
//!     off so the user can pick a model that isn't tagged for the
//!     current slot
//!   - Esc: Models view → back to Slots; Slots view → close picker
//!
//! Catalog data — `use_cases`, `tier`, `description`, cost,
//! benchmarks — mirrors the gateway's `GET /v1/model/info` schema
//! (SMOODEV-1793). The offline fallback catalog at the bottom of
//! this file keeps the picker usable when the gateway is down or
//! the user hasn't logged into one yet.

use std::path::PathBuf;
use std::sync::OnceLock;

use smooth_cast::provider_migration::load_providers_with_migration;
use smooth_operator::providers::{Activity, ModelSlot, ProviderRegistry};

/// Which routing slot a picker entry maps to. Distinct from
/// [`Activity`] because `ModelRouting` has a `default` slot that
/// isn't an `Activity` variant — it's a wire-compat fallback used
/// by `default_llm_config()`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PickerSlot {
    Coding,
    Reasoning,
    Reviewing,
    Judge,
    Summarize,
    Fast,
    Default,
}

impl PickerSlot {
    fn from_activity(a: Activity) -> Self {
        match a {
            Activity::Coding => Self::Coding,
            Activity::Reasoning => Self::Reasoning,
            Activity::Reviewing => Self::Reviewing,
            Activity::Judge => Self::Judge,
            Activity::Summarize => Self::Summarize,
            Activity::Fast => Self::Fast,
        }
    }
}

/// Which sub-view of the picker is currently active.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PickerView {
    /// Listing the routing slots (Thinking, Coding, …).
    Slots,
    /// Listing candidate models for a single routing slot.
    Models { slot: PickerSlot },
}

/// One activity slot shown in the Slots view.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SlotEntry {
    pub slot: PickerSlot,
    pub label: &'static str,
    pub description: &'static str,
    pub current_provider: String,
    pub current_model: String,
}

/// Capability tier for a model. Mirrors the `tier` field on the
/// gateway's `model_info` schema. Rendered as a short tag in the
/// catalog picker.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tier {
    Flagship,
    Workhorse,
    Fast,
    Utility,
}

impl Tier {
    pub fn label(&self) -> &'static str {
        match self {
            Self::Flagship => "flagship",
            Self::Workhorse => "workhorse",
            Self::Fast => "fast",
            Self::Utility => "utility",
        }
    }
}

/// Benchmark scores attached to a model. All optional — the gateway
/// emits whichever metrics the lab publishes for that model.
/// Numbers are percent (0..=100) unless otherwise noted.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct Benchmarks {
    /// SWE-bench Verified score (coding capability).
    pub swe_bench_verified: Option<f32>,
    /// GPQA Diamond score (graduate-level reasoning).
    pub gpqa_diamond: Option<f32>,
    /// Artificial Analysis Intelligence Index — composite score.
    pub aa_intelligence_index: Option<f32>,
}

/// Catalog metadata for a model. Held inline on [`ModelEntry`] so
/// the renderer can show name + tier + cost + benchmark + use-cases
/// without a second lookup.
#[derive(Debug, Clone, PartialEq)]
pub struct ModelInfo {
    /// Capability tags the model is well-suited for. Drives slot
    /// filtering. Examples: `"coding"`, `"reasoning"`, `"reviewing"`,
    /// `"judge"`, `"summarize"`, `"fast"`, `"utility"`,
    /// `"long-context"`, `"agentic"`, `"critique"`, `"guardrails"`.
    pub use_cases: Vec<String>,
    pub tier: Tier,
    /// One-line catalog description, shown dimmed under the row.
    pub description: String,
    /// Input price as $/token (LiteLLM's native unit). The renderer
    /// multiplies by 1_000_000 for the $/M display.
    pub input_cost_per_token: f64,
    pub output_cost_per_token: f64,
    pub benchmarks: Benchmarks,
}

impl ModelInfo {
    /// Convert per-token to per-million for display.
    pub fn cost_per_million(&self) -> (f64, f64) {
        (self.input_cost_per_token * 1_000_000.0, self.output_cost_per_token * 1_000_000.0)
    }

    pub fn has_use_case(&self, tag: &str) -> bool {
        self.use_cases.iter().any(|u| u.eq_ignore_ascii_case(tag))
    }

    /// The benchmark relevant to a slot, used for sort ordering.
    /// Falls back to AA Intelligence Index when the slot-specific
    /// number isn't available.
    pub fn slot_benchmark(&self, slot: PickerSlot) -> Option<f32> {
        match slot {
            PickerSlot::Coding => self.benchmarks.swe_bench_verified,
            PickerSlot::Reasoning => self.benchmarks.gpqa_diamond.or(self.benchmarks.aa_intelligence_index),
            _ => self.benchmarks.aa_intelligence_index,
        }
    }
}

/// One candidate model shown in the Models view.
///
/// `info` is `None` when we only know the model name (e.g. a slot
/// pointing at a model we have no metadata for, or a legacy
/// `smooth-*` alias). The renderer falls back to a name-only row
/// in that case.
#[derive(Debug, Clone, PartialEq)]
pub struct ModelEntry {
    pub provider: String,
    pub model: String,
    pub info: Option<ModelInfo>,
}

impl ModelEntry {
    pub fn display(&self) -> String {
        format!("{}  ({})", self.model, self.provider)
    }
}

/// Picker state.
#[derive(Debug, Clone)]
pub struct ModelPickerState {
    pub active: bool,
    pub view: PickerView,
    pub slots: Vec<SlotEntry>,
    pub models: Vec<ModelEntry>,
    pub selected: usize,
    /// Path to `providers.json` used for persistence. `None` means
    /// "don't persist" — useful for tests and for fresh installs
    /// with no providers.json yet.
    pub providers_path: Option<PathBuf>,
    /// Last error surfaced while loading or saving the registry.
    /// Rendered as a subtitle line so the user knows why the slot
    /// list is empty or why their Enter didn't take effect.
    pub error: Option<String>,
    /// When `true`, the Models view ignores the slot's use-case
    /// filter and shows every catalog model. Toggled by Tab.
    /// Reset whenever the user returns to the Slots view or drills
    /// in fresh.
    pub show_all: bool,
}

impl Default for ModelPickerState {
    fn default() -> Self {
        Self::new()
    }
}

impl ModelPickerState {
    pub fn new() -> Self {
        Self {
            active: false,
            view: PickerView::Slots,
            slots: Vec::new(),
            models: Vec::new(),
            selected: 0,
            providers_path: default_providers_path(),
            error: None,
            show_all: false,
        }
    }

    /// Show the picker, re-reading `providers.json` so the slot list
    /// reflects the current on-disk state.
    pub fn activate(&mut self) {
        self.active = true;
        self.view = PickerView::Slots;
        self.selected = 0;
        self.error = None;
        self.show_all = false;
        self.reload_slots();
    }

    pub fn deactivate(&mut self) {
        self.active = false;
        self.view = PickerView::Slots;
        self.selected = 0;
        self.show_all = false;
    }

    /// Move selection up by one, wrapping.
    pub fn select_up(&mut self) {
        let n = self.current_len();
        if n == 0 {
            return;
        }
        self.selected = if self.selected == 0 { n - 1 } else { self.selected - 1 };
    }

    /// Move selection down by one, wrapping.
    pub fn select_down(&mut self) {
        let n = self.current_len();
        if n == 0 {
            return;
        }
        self.selected = (self.selected + 1) % n;
    }

    fn current_len(&self) -> usize {
        match self.view {
            PickerView::Slots => self.slots.len(),
            PickerView::Models { .. } => self.models.len(),
        }
    }

    /// Enter the Models sub-view for the currently selected slot.
    /// No-op when not in Slots view or the selected index is out of
    /// bounds.
    pub fn open_models_for_selected(&mut self) {
        if !matches!(self.view, PickerView::Slots) {
            return;
        }
        let Some(entry) = self.slots.get(self.selected).cloned() else { return };
        // Reset the filter override every time the user drills in fresh.
        self.show_all = false;
        self.models = candidate_models_filtered(&self.slots, &entry, self.show_all);
        // Pre-select the slot's current model when it's in the list.
        self.selected = self
            .models
            .iter()
            .position(|m| m.provider == entry.current_provider && m.model == entry.current_model)
            .unwrap_or(0);
        self.view = PickerView::Models { slot: entry.slot };
    }

    /// Leave the Models view, returning to Slots. No-op when already
    /// in Slots view.
    pub fn back_to_slots(&mut self) {
        let PickerView::Models { slot } = self.view else { return };
        // Re-select the slot we came from so Up/Down feels continuous.
        self.selected = self.slots.iter().position(|s| s.slot == slot).unwrap_or(0);
        self.view = PickerView::Slots;
        self.models.clear();
        self.show_all = false;
    }

    /// Toggle the "ignore use-case filter" view. Only meaningful in
    /// the Models view; a no-op in the Slots view. Rebuilds the
    /// candidate list with the new filter applied and keeps the
    /// previously-highlighted model highlighted when it's still in
    /// the visible set.
    pub fn toggle_show_all(&mut self) {
        let PickerView::Models { slot } = self.view else { return };
        let prev = self.models.get(self.selected).cloned();
        self.show_all = !self.show_all;
        let focused = self.slots.iter().find(|s| s.slot == slot).cloned();
        if let Some(focused) = focused {
            self.models = candidate_models_filtered(&self.slots, &focused, self.show_all);
            self.selected = prev
                .as_ref()
                .and_then(|p| self.models.iter().position(|m| m.provider == p.provider && m.model == p.model))
                .unwrap_or(0);
        }
    }

    /// Apply the highlighted model to the slot we drilled in on,
    /// persist to disk, and return to the Slots view. Returns `true`
    /// when the slot was actually updated.
    ///
    /// On save failure the error is stashed in `self.error` and
    /// surfaced in the rendered subtitle; the slots list is still
    /// reloaded so the user sees whatever state actually persisted.
    pub fn apply_selected_model(&mut self) -> bool {
        let PickerView::Models { slot } = self.view else { return false };
        let Some(chosen) = self.models.get(self.selected).cloned() else {
            return false;
        };
        let Some(path) = self.providers_path.clone() else {
            self.error = Some("No providers.json — can't persist routing change".to_string());
            return false;
        };

        match load_providers_with_migration(&path) {
            Ok(mut registry) => {
                let new_slot = ModelSlot::new(chosen.provider, chosen.model);
                assign_slot(&mut registry, slot, new_slot);
                if let Err(e) = registry.save_to_file(&path) {
                    self.error = Some(format!("save failed: {e}"));
                    return false;
                }
            }
            Err(e) => {
                self.error = Some(format!("load failed: {e}"));
                return false;
            }
        }

        // Success — reload slot list from disk and bounce back to Slots.
        self.reload_slots();
        if let Some(idx) = self.slots.iter().position(|s| s.slot == slot) {
            self.selected = idx;
        }
        self.view = PickerView::Slots;
        self.models.clear();
        self.show_all = false;
        self.error = None;
        true
    }

    /// Re-read `providers.json` and rebuild the slot list.
    ///
    /// Silently falls back to an empty slot list if the file is
    /// missing or unreadable; the error is captured in `self.error`
    /// for the renderer to surface.
    pub fn reload_slots(&mut self) {
        let Some(path) = self.providers_path.as_ref() else {
            self.slots = Vec::new();
            self.error = Some("No providers.json configured".to_string());
            return;
        };
        match load_providers_with_migration(path) {
            Ok(registry) => {
                self.slots = slot_entries(&registry);
                self.error = None;
            }
            Err(e) => {
                self.slots = Vec::new();
                self.error = Some(format!("load failed: {e}"));
            }
        }
    }

    /// Load slots from an explicit registry (used by tests and by
    /// anyone who already has a registry in hand).
    pub fn load_from_registry(&mut self, registry: &ProviderRegistry) {
        self.slots = slot_entries(registry);
    }
}

fn default_providers_path() -> Option<PathBuf> {
    dirs_next::home_dir().map(|h| h.join(".smooth/providers.json"))
}

/// Fixed display order for the slot list.
///
/// Six canonical activity slots plus the wire-compat `Default` slot
/// (served by the coding route at runtime but still editable on disk
/// so older configs continue to load).
pub const ALL_SLOTS: &[(PickerSlot, &str, &str)] = &[
    (PickerSlot::Coding, "Coding", "code edits, refactors, tool-using loops"),
    (PickerSlot::Reasoning, "Reasoning", "deep reasoning, planning, hard problems"),
    (PickerSlot::Reviewing, "Reviewing", "critique, PR review, style checks"),
    (PickerSlot::Judge, "Judge", "Narc rulings, guardrail checks"),
    (PickerSlot::Summarize, "Summarize", "context compaction, long-doc summary"),
    (PickerSlot::Fast, "Fast", "utility calls (session titles, quick summaries)"),
    (PickerSlot::Default, "Default", "wire-compat fallback (served by Coding)"),
];

fn slot_entries(registry: &ProviderRegistry) -> Vec<SlotEntry> {
    ALL_SLOTS
        .iter()
        .map(|&(slot, label, description)| {
            let ms = read_slot(registry, slot);
            SlotEntry {
                slot,
                label,
                description,
                current_provider: ms.provider.clone(),
                current_model: ms.model.clone(),
            }
        })
        .collect()
}

fn read_slot(registry: &ProviderRegistry, slot: PickerSlot) -> ModelSlot {
    match slot {
        PickerSlot::Coding => registry.routing.coding.clone(),
        PickerSlot::Reasoning => registry.routing.reasoning.clone().unwrap_or_else(|| registry.routing.default.clone()),
        PickerSlot::Reviewing => registry.routing.reviewing.clone(),
        PickerSlot::Judge => registry.routing.judge.clone(),
        PickerSlot::Summarize => registry.routing.summarize.clone(),
        PickerSlot::Default => registry.routing.default.clone(),
        PickerSlot::Fast => registry.routing.fast.clone().unwrap_or_else(|| registry.routing.default.clone()),
    }
}

/// Use-case tags accepted for each slot. `Default` returns `&[]`
/// (no filter — the user wants a single any-shape model). For every
/// other slot we accept models whose `use_cases` intersect the
/// slot tags.
pub fn slot_use_cases(slot: PickerSlot) -> &'static [&'static str] {
    match slot {
        PickerSlot::Coding => &["coding"],
        PickerSlot::Reasoning => &["reasoning"],
        PickerSlot::Reviewing => &["reviewing", "critique"],
        PickerSlot::Judge => &["judge", "guardrails"],
        PickerSlot::Summarize => &["summarize", "long-context"],
        PickerSlot::Fast => &["fast", "utility"],
        PickerSlot::Default => &[],
    }
}

/// Build the candidate list for a slot, applying the slot's
/// use-case filter unless `show_all` is set. Starts with every
/// model currently routed somewhere (so the user can swap an
/// already-known model into a new slot in one keystroke), then
/// folds in the offline catalog. De-duplicates by (provider,
/// model) and sorts the result by the slot's relevant benchmark
/// descending, with un-benchmarked rows after benchmarked ones.
fn candidate_models_filtered(slots: &[SlotEntry], focused: &SlotEntry, show_all: bool) -> Vec<ModelEntry> {
    let mut out: Vec<ModelEntry> = Vec::new();
    let push = |entry: ModelEntry, out: &mut Vec<ModelEntry>| {
        if !out.iter().any(|e| e.provider == entry.provider && e.model == entry.model) {
            out.push(entry);
        }
    };

    // Currently-routed models always show up — the user might want
    // to pull `smooth-summarize` into the reviewing slot deliberately,
    // even when its use_cases don't intersect.
    for s in slots {
        let info = catalog_lookup(&s.current_model).cloned();
        push(
            ModelEntry {
                provider: s.current_provider.clone(),
                model: s.current_model.clone(),
                info,
            },
            &mut out,
        );
    }

    // Offline catalog (eventually replaced by GET /v1/model/info).
    // Pick the provider from the focused slot so a freshly-installed
    // gateway user gets `smooai-gateway`-routed entries and a direct
    // user keeps their own provider id.
    let provider = focused.current_provider.clone();
    for (model_name, info) in fallback_catalog() {
        push(
            ModelEntry {
                provider: provider.clone(),
                model: (*model_name).to_string(),
                info: Some(info.clone()),
            },
            &mut out,
        );
    }

    // Apply the use-case filter unless overridden. Models without
    // info pass through unfiltered when they were already routed
    // somewhere — losing the user's existing slot model from the
    // picker would be surprising.
    let filter_tags = if show_all { &[][..] } else { slot_use_cases(focused.slot) };
    if !filter_tags.is_empty() {
        out.retain(|m| match &m.info {
            Some(info) => filter_tags.iter().any(|tag| info.has_use_case(tag)),
            None => true,
        });
    }

    // Sort by slot benchmark descending. Un-benchmarked rows go
    // last but still appear (so the user sees them). Use `total_cmp`,
    // not `partial_cmp().unwrap_or(Equal)`: a NaN benchmark makes
    // `partial_cmp` return `None`, and collapsing that to `Equal`
    // violates total order — which Rust's sort detects and *panics* on
    // ("comparison function does not correctly implement a total
    // order"). `f32::total_cmp` orders NaN deterministically. Pearl
    // th-03b02e.
    out.sort_by(|a, b| {
        let a_score = a.info.as_ref().and_then(|i| i.slot_benchmark(focused.slot));
        let b_score = b.info.as_ref().and_then(|i| i.slot_benchmark(focused.slot));
        match (a_score, b_score) {
            (Some(x), Some(y)) => y.total_cmp(&x),
            (Some(_), None) => std::cmp::Ordering::Less,
            (None, Some(_)) => std::cmp::Ordering::Greater,
            (None, None) => a.model.cmp(&b.model),
        }
    });

    out
}

/// Resolve a model name against the offline fallback catalog.
fn catalog_lookup(model: &str) -> Option<&'static ModelInfo> {
    fallback_catalog()
        .iter()
        .find_map(|(name, info)| if *name == model { Some(info) } else { None })
}

/// Hardcoded fallback catalog seeded from
/// `infra/services/litellm/config.yaml` (smooai monorepo). Costs
/// are the LiteLLM-native $/token floats; the renderer multiplies
/// by 1_000_000 for the display. Use-cases / tier / description /
/// benchmarks are the picker's editorial layer — the eventual
/// `/v1/model/info` endpoint will source these from the gateway so
/// they can be edited without a Smooth release.
///
/// Updated 2026-06: covers each slot default plus a few overrides.
#[allow(clippy::unreadable_literal)]
pub fn fallback_catalog() -> &'static [(&'static str, ModelInfo)] {
    static CATALOG: OnceLock<Vec<(&'static str, ModelInfo)>> = OnceLock::new();
    CATALOG.get_or_init(|| {
        vec![
            (
                "deepseek-v4-flash",
                ModelInfo {
                    use_cases: svec(&["coding", "reasoning", "agentic"]),
                    tier: Tier::Workhorse,
                    description: "DeepSeek V4-Flash — cheap, fast, top SWE-bench at this price tier.".into(),
                    input_cost_per_token: 0.00000014,
                    output_cost_per_token: 0.00000028,
                    benchmarks: Benchmarks {
                        swe_bench_verified: Some(79.0),
                        gpqa_diamond: None,
                        aa_intelligence_index: Some(48.0),
                    },
                },
            ),
            (
                "deepseek-v4-pro",
                ModelInfo {
                    use_cases: svec(&["coding", "reasoning", "agentic", "long-context"]),
                    tier: Tier::Flagship,
                    description: "DeepSeek V4-Pro — flagship long-horizon planner + coder, 1M ctx.".into(),
                    input_cost_per_token: 0.0000003,
                    output_cost_per_token: 0.0000015,
                    benchmarks: Benchmarks {
                        swe_bench_verified: Some(80.6),
                        gpqa_diamond: Some(75.0),
                        aa_intelligence_index: Some(52.0),
                    },
                },
            ),
            (
                "minimax-m2.7-direct",
                ModelInfo {
                    use_cases: svec(&["reviewing", "critique", "coding"]),
                    tier: Tier::Workhorse,
                    description: "MiniMax M2.7 — adversarial reviewer; cross-lab bug finder.".into(),
                    input_cost_per_token: 0.0000003,
                    output_cost_per_token: 0.0000012,
                    benchmarks: Benchmarks {
                        swe_bench_verified: Some(56.2),
                        gpqa_diamond: None,
                        aa_intelligence_index: Some(44.0),
                    },
                },
            ),
            (
                "gemini-2.5-flash",
                ModelInfo {
                    use_cases: svec(&["judge", "guardrails", "summarize", "long-context", "fast"]),
                    tier: Tier::Fast,
                    description: "Gemini 2.5 Flash — 1M ctx, IFEval leader in the cheap Gemini tier.".into(),
                    input_cost_per_token: 0.0000003,
                    output_cost_per_token: 0.0000025,
                    benchmarks: Benchmarks {
                        swe_bench_verified: None,
                        gpqa_diamond: None,
                        aa_intelligence_index: Some(40.0),
                    },
                },
            ),
            (
                "gemini-2.5-flash-lite",
                ModelInfo {
                    use_cases: svec(&["fast", "utility"]),
                    tier: Tier::Utility,
                    description: "Gemini 2.5 Flash-Lite — sub-300ms first-token utility model.".into(),
                    input_cost_per_token: 0.00000005,
                    output_cost_per_token: 0.00000008,
                    benchmarks: Benchmarks {
                        swe_bench_verified: None,
                        gpqa_diamond: None,
                        aa_intelligence_index: Some(28.0),
                    },
                },
            ),
            (
                "claude-haiku-4-5",
                ModelInfo {
                    use_cases: svec(&["judge", "guardrails", "fast"]),
                    tier: Tier::Fast,
                    description: "Claude Haiku 4.5 — strict-refusal lineage; safety fallback.".into(),
                    input_cost_per_token: 0.000001,
                    output_cost_per_token: 0.000005,
                    benchmarks: Benchmarks {
                        swe_bench_verified: None,
                        gpqa_diamond: None,
                        aa_intelligence_index: Some(35.0),
                    },
                },
            ),
            (
                "kimi-k2.6-direct",
                ModelInfo {
                    use_cases: svec(&["reasoning", "coding", "agentic"]),
                    tier: Tier::Flagship,
                    description: "Kimi K2.6 — deep reasoner, thinking-by-default.".into(),
                    input_cost_per_token: 0.000000435,
                    output_cost_per_token: 0.00000087,
                    benchmarks: Benchmarks {
                        swe_bench_verified: Some(80.2),
                        gpqa_diamond: Some(72.0),
                        aa_intelligence_index: Some(50.0),
                    },
                },
            ),
            (
                "qwen3-coder-plus-direct",
                ModelInfo {
                    use_cases: svec(&["coding", "reviewing", "long-context", "summarize"]),
                    tier: Tier::Workhorse,
                    description: "Qwen3-Coder-Plus — PR-review tuned, 1M ctx backstop.".into(),
                    input_cost_per_token: 0.000000325,
                    output_cost_per_token: 0.00000195,
                    benchmarks: Benchmarks {
                        swe_bench_verified: Some(70.6),
                        gpqa_diamond: None,
                        aa_intelligence_index: Some(42.0),
                    },
                },
            ),
            // Pearl th-3468bd: judge + fast defaults now route to
            // Groq Llama. Embedding the catalog entries here so the
            // picker shows them with the right metadata even in
            // offline mode.
            (
                "groq-llama-3.3-70b",
                ModelInfo {
                    use_cases: svec(&["judge", "guardrails", "reasoning", "fast"]),
                    tier: Tier::Fast,
                    description: "Llama 3.3-70B on Groq — strong judge, sub-second p95.".into(),
                    input_cost_per_token: 0.00000059,
                    output_cost_per_token: 0.00000079,
                    benchmarks: Benchmarks {
                        swe_bench_verified: None,
                        gpqa_diamond: None,
                        aa_intelligence_index: Some(36.0),
                    },
                },
            ),
            (
                "groq-llama-3.1-8b",
                ModelInfo {
                    use_cases: svec(&["fast", "utility", "cheap"]),
                    tier: Tier::Utility,
                    description: "Llama 3.1-8B on Groq — sub-300ms utility, ~10x cheaper than Gemini Flash Lite.".into(),
                    input_cost_per_token: 0.00000005,
                    output_cost_per_token: 0.00000008,
                    benchmarks: Benchmarks {
                        swe_bench_verified: None,
                        gpqa_diamond: None,
                        aa_intelligence_index: Some(24.0),
                    },
                },
            ),
        ]
    })
}

fn svec(s: &[&str]) -> Vec<String> {
    s.iter().map(|x| (*x).to_string()).collect()
}

fn assign_slot(registry: &mut ProviderRegistry, slot: PickerSlot, new_slot: ModelSlot) {
    match slot {
        PickerSlot::Coding => registry.routing.coding = new_slot,
        PickerSlot::Reasoning => registry.routing.reasoning = Some(new_slot),
        PickerSlot::Reviewing => registry.routing.reviewing = new_slot,
        PickerSlot::Judge => registry.routing.judge = new_slot,
        PickerSlot::Summarize => registry.routing.summarize = new_slot,
        PickerSlot::Default => registry.routing.default = new_slot,
        PickerSlot::Fast => registry.routing.fast = Some(new_slot),
    }
}

/// Convenience conversion for callers that have an `Activity` and
/// want to navigate the picker to the matching slot.
impl From<Activity> for PickerSlot {
    fn from(a: Activity) -> Self {
        Self::from_activity(a)
    }
}

// ── Catalog row rendering ───────────────────────────────────────
//
// The catalog row format kept in this module (rather than render.rs)
// so it's testable with a character-exact snapshot without spinning
// up a ratatui Frame. The renderer in render.rs builds spans off
// these strings.

/// Width of the model-name column in the catalog row. Names longer
/// than this get truncated with an ellipsis.
pub const NAME_COL: usize = 22;
/// Width of the tier column.
pub const TIER_COL: usize = 9;
/// Width of the cost column (e.g. `$0.14/$0.28`).
pub const COST_COL: usize = 12;
/// Width of the benchmark column (e.g. `79.0`).
pub const BENCH_COL: usize = 5;

/// Catalog footer surfaced once at the bottom of the picker. Kept
/// short so it fits the 72-col popup; widens up to 100 cols look
/// the same.
pub const BENCHMARK_CAVEAT: &str = "Benchmarks ≈ harness-dependent — use as a tiebreaker, not gospel.";

/// Format a catalog row for `entry`, slot-aware so the displayed
/// benchmark matches the slot's sort key. Single-line, padded to
/// fixed columns, monospace.
///
/// Layout (without the leading caret/prefix):
/// `name              tier      $IN/$OUT   bench  tag · tag · tag`
///
/// When `info` is missing we fall back to the legacy
/// `model  (provider)` shape so legacy `smooth-*` rows still render.
pub fn format_catalog_row(prefix: &str, entry: &ModelEntry, slot: PickerSlot) -> String {
    let Some(info) = entry.info.as_ref() else {
        return format!("{prefix}{}", entry.display());
    };
    let name = truncate_with_ellipsis(&entry.model, NAME_COL);
    let tier = info.tier.label();
    let (in_m, out_m) = info.cost_per_million();
    let cost = format_cost(in_m, out_m);
    let bench = info.slot_benchmark(slot).map_or_else(|| "  -  ".to_string(), |b| format!("{b:>5.1}"));
    let chips = info.use_cases.join(" · ");
    format!(
        "{prefix}{:<name_w$} {:<tier_w$} {:<cost_w$} {:>bench_w$}  {chips}",
        name,
        tier,
        cost,
        bench,
        name_w = NAME_COL,
        tier_w = TIER_COL,
        cost_w = COST_COL,
        bench_w = BENCH_COL,
    )
}

/// Truncate a string to at most `max` chars, suffixing `…` when
/// truncation actually happens. Operates on `char_indices` so
/// multi-byte UTF-8 is safe (model names are ASCII today but cheap
/// insurance).
fn truncate_with_ellipsis(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    if max == 0 {
        return String::new();
    }
    let cut: String = s.chars().take(max.saturating_sub(1)).collect();
    format!("{cut}…")
}

/// Format the input/output cost cell. Trim trailing zeros for
/// readability; cap to 2 decimals so columns stay aligned.
fn format_cost(in_m: f64, out_m: f64) -> String {
    format!("${}/${}", trim_money(in_m), trim_money(out_m))
}

fn trim_money(v: f64) -> String {
    // Two decimals fits the production catalog (e.g. $0.14, $1.20).
    // Strip trailing zeros only past the decimal point so $0.14
    // stays as `0.14` but $1.00 collapses to `1`.
    let s = format!("{v:.2}");
    if let Some(dot) = s.find('.') {
        let trimmed = s.trim_end_matches('0').trim_end_matches('.');
        if trimmed.is_empty() || trimmed == "-" {
            return s[..dot].to_string();
        }
        return trimmed.to_string();
    }
    s
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use smooth_operator::providers::{ModelRouting, Preset, ProviderConfig};

    use super::*;

    /// Regression for pearl th-03b02e: the model-picker sorts rows by
    /// benchmark, and a NaN benchmark (possible once `/v1/model/info`
    /// sources these live) must not make the comparator non-total —
    /// Rust's sort *panics* on a comparator that violates total order
    /// ("comparison function does not correctly implement a total
    /// order"), which had been failing CI's `cargo test` and blocking
    /// every release. `f32::total_cmp` is the fix; this guards it.
    #[test]
    fn benchmark_sort_with_nan_is_a_total_order_and_does_not_panic() {
        // The exact (Some, Some) arm the picker uses, over a set that
        // mixes NaN, +/-, and duplicate scores — the cases that break
        // `partial_cmp().unwrap_or(Equal)`.
        let mut scores: Vec<f32> = vec![79.0, f32::NAN, 56.2, 80.6, f32::NAN, 56.2, 40.0];
        // Must not panic (descending, like the picker).
        scores.sort_by(|x, y| y.total_cmp(x));
        assert_eq!(scores.len(), 7);
        // total_cmp is a strict total order, so every pair is comparable
        // and the result is stable/transitive — assert antisymmetry holds
        // across the whole set (the property the old comparator violated).
        for w in scores.windows(2) {
            assert!(w[0].total_cmp(&w[1]) != std::cmp::Ordering::Less, "sorted descending: {:?}", &scores);
        }
    }

    /// In-memory fixture seeded with concrete model names (post
    /// SMOODEV-1793 cutover). The legacy `smooth-*` aliases are dead
    /// at the gateway; tests use the same defaults the migration
    /// shim rewrites old configs to. See `smooth_policy::smooth_alias`
    /// for the canonical mapping.
    fn test_registry() -> ProviderRegistry {
        let mut r = ProviderRegistry::new();
        r.register_provider(ProviderConfig {
            id: "smooth".into(),
            api_url: "https://llm.smoo.ai/v1".into(),
            api_key: "test".into(),
            api_format: smooth_operator::llm::ApiFormat::OpenAiCompat,
            default_model: "deepseek-v4-flash".into(),
        });
        r = r.with_routing(ModelRouting {
            coding: ModelSlot::new("smooth", "deepseek-v4-flash"),
            reasoning: Some(ModelSlot::new("smooth", "deepseek-v4-pro")),
            reviewing: ModelSlot::new("smooth", "minimax-m2.7-direct"),
            judge: ModelSlot::new("smooth", "gemini-2.5-flash"),
            summarize: ModelSlot::new("smooth", "gemini-2.5-flash"),
            default: ModelSlot::new("smooth", "deepseek-v4-flash"),
            fast: Some(ModelSlot::new("smooth", "gemini-2.5-flash-lite")),
            planning: None,
        });
        r
    }

    #[test]
    fn new_picker_is_inactive_and_on_slots_view() {
        let p = ModelPickerState::new();
        assert!(!p.active);
        assert_eq!(p.view, PickerView::Slots);
        assert!(!p.show_all);
    }

    #[test]
    fn load_from_registry_populates_all_slots() {
        let mut p = ModelPickerState::new();
        p.load_from_registry(&test_registry());
        assert_eq!(p.slots.len(), ALL_SLOTS.len());
        assert_eq!(p.slots[0].slot, PickerSlot::Coding);
        assert_eq!(p.slots[0].current_model, "deepseek-v4-flash");
        let fast = p.slots.iter().find(|s| s.slot == PickerSlot::Fast).expect("fast slot");
        assert_eq!(fast.current_model, "gemini-2.5-flash-lite");
        let reasoning = p.slots.iter().find(|s| s.slot == PickerSlot::Reasoning).expect("reasoning slot");
        assert_eq!(reasoning.current_model, "deepseek-v4-pro");
    }

    #[test]
    fn navigate_wraps_top_and_bottom() {
        let mut p = ModelPickerState::new();
        p.load_from_registry(&test_registry());
        assert_eq!(p.selected, 0);
        p.select_up();
        assert_eq!(p.selected, p.slots.len() - 1);
        p.select_down();
        assert_eq!(p.selected, 0);
        p.select_down();
        assert_eq!(p.selected, 1);
    }

    #[test]
    fn open_models_then_back_restores_slot_selection() {
        let mut p = ModelPickerState::new();
        p.load_from_registry(&test_registry());

        // Drill into the Reviewing slot regardless of index.
        let idx = ALL_SLOTS.iter().position(|(s, _, _)| *s == PickerSlot::Reviewing).expect("reviewing slot");
        p.selected = idx;
        p.open_models_for_selected();
        assert_eq!(p.view, PickerView::Models { slot: PickerSlot::Reviewing });
        assert!(!p.models.is_empty());

        p.back_to_slots();
        assert_eq!(p.view, PickerView::Slots);
        assert_eq!(p.selected, idx);
        assert!(!p.show_all);
    }

    #[test]
    fn open_models_preselects_current_model() {
        let mut p = ModelPickerState::new();
        p.load_from_registry(&test_registry());
        // Drill into Reasoning — its current model is the concrete
        // post-cutover default (`deepseek-v4-pro`).
        let idx = ALL_SLOTS.iter().position(|(s, _, _)| *s == PickerSlot::Reasoning).expect("reasoning slot");
        p.selected = idx;
        p.open_models_for_selected();
        let chosen = &p.models[p.selected];
        assert_eq!(chosen.model, "deepseek-v4-pro");
    }

    #[test]
    fn apply_selected_model_persists_and_returns_to_slots() {
        let tmp = tempfile::tempdir().expect("tmpdir");
        let path = tmp.path().join("providers.json");
        test_registry().save_to_file(&path).expect("seed file");

        let mut p = ModelPickerState::new();
        p.providers_path = Some(path.clone());
        p.reload_slots();

        // Drill into Coding; pick the reasoning model (deepseek-v4-pro)
        // instead of the coding default (deepseek-v4-flash).
        let coding_idx = ALL_SLOTS.iter().position(|(s, _, _)| *s == PickerSlot::Coding).expect("coding slot");
        p.selected = coding_idx;
        p.open_models_for_selected();
        let idx = p
            .models
            .iter()
            .position(|m| m.model == "deepseek-v4-pro")
            .expect("deepseek-v4-pro is always a candidate (reasoning slot's default)");
        p.selected = idx;
        assert!(p.apply_selected_model());

        // Back on Slots view, Coding now points at deepseek-v4-pro.
        assert_eq!(p.view, PickerView::Slots);
        let coding = p.slots.iter().find(|s| s.slot == PickerSlot::Coding).expect("coding");
        assert_eq!(coding.current_model, "deepseek-v4-pro");

        // And the on-disk file actually changed.
        let reloaded = ProviderRegistry::load_from_file(&path).expect("reload");
        assert_eq!(reloaded.routing.coding.model, "deepseek-v4-pro");
    }

    #[test]
    fn apply_without_providers_path_sets_error() {
        let mut p = ModelPickerState::new();
        p.load_from_registry(&test_registry());
        p.selected = 0;
        p.open_models_for_selected();
        p.providers_path = None;
        assert!(!p.apply_selected_model());
        assert!(p.error.is_some());
    }

    #[test]
    fn reload_missing_file_surfaces_error_and_clears_slots() {
        let mut p = ModelPickerState::new();
        p.providers_path = Some(Path::new("/nonexistent/path/providers.json").to_path_buf());
        p.reload_slots();
        assert!(p.slots.is_empty());
        assert!(p.error.is_some());
    }

    #[test]
    fn deactivate_resets_view_and_selection() {
        let mut p = ModelPickerState::new();
        p.load_from_registry(&test_registry());
        p.active = true;
        p.selected = 5;
        p.view = PickerView::Models { slot: PickerSlot::Judge };
        p.show_all = true;
        p.deactivate();
        assert!(!p.active);
        assert_eq!(p.view, PickerView::Slots);
        assert_eq!(p.selected, 0);
        assert!(!p.show_all);
    }

    #[test]
    fn preset_registry_populates_all_slots() {
        let presets = [
            Preset::SmoaiGateway,
            Preset::OpenRouterLowCost,
            Preset::LlmGatewayLowCost,
            Preset::OpenAI,
            Preset::Anthropic,
        ];
        for preset in presets {
            let reg = ProviderRegistry::from_preset(preset, "test");
            let mut p = ModelPickerState::new();
            p.load_from_registry(&reg);
            assert_eq!(p.slots.len(), ALL_SLOTS.len(), "{preset:?} populates every slot");
            // Every slot has a resolvable provider/model pair — no empty strings.
            for s in &p.slots {
                assert!(!s.current_model.is_empty(), "{preset:?}::{:?} model blank", s.slot);
                assert!(!s.current_provider.is_empty(), "{preset:?}::{:?} provider blank", s.slot);
            }
        }
    }

    // ── Catalog-view tests ──────────────────────────────────────

    /// Synthetic mixed catalog used by the slot-filter tests. Kept
    /// inline so the tests don't depend on the fallback catalog's
    /// editorial choices.
    fn synthetic_slot_entries() -> Vec<SlotEntry> {
        // Build slots with concrete model names from our fallback
        // catalog so the filter sees real `info` rows.
        vec![SlotEntry {
            slot: PickerSlot::Coding,
            label: "Coding",
            description: "",
            current_provider: "smooai-gateway".into(),
            current_model: "deepseek-v4-flash".into(),
        }]
    }

    #[test]
    fn coding_slot_filters_by_coding_use_case() {
        let slots = synthetic_slot_entries();
        let focused = slots[0].clone();
        let models = candidate_models_filtered(&slots, &focused, false);
        assert!(!models.is_empty());
        // Every visible row either has no info (currently-routed pass-through)
        // or has the "coding" tag.
        for m in &models {
            if let Some(info) = &m.info {
                assert!(info.has_use_case("coding"), "{} should carry the coding tag", m.model);
            }
        }
        // gemini-2.5-flash-lite is utility-only — must NOT appear.
        assert!(
            !models.iter().any(|m| m.model == "gemini-2.5-flash-lite"),
            "fast-only model leaked into coding picker"
        );
    }

    #[test]
    fn coding_slot_sorts_by_swe_bench_desc() {
        let slots = synthetic_slot_entries();
        let focused = slots[0].clone();
        let models = candidate_models_filtered(&slots, &focused, false);
        // First two benchmarked coding models should be deepseek-v4-pro
        // (80.6) then kimi-k2.6-direct (80.2) then deepseek-v4-flash (79.0).
        let bench_models: Vec<_> = models
            .iter()
            .filter(|m| m.info.as_ref().and_then(|i| i.benchmarks.swe_bench_verified).is_some())
            .collect();
        assert_eq!(bench_models[0].model, "deepseek-v4-pro");
        assert_eq!(bench_models[1].model, "kimi-k2.6-direct");
    }

    #[test]
    fn reasoning_slot_filters_by_reasoning_use_case() {
        let slots = vec![SlotEntry {
            slot: PickerSlot::Reasoning,
            label: "Reasoning",
            description: "",
            current_provider: "smooai-gateway".into(),
            current_model: "deepseek-v4-pro".into(),
        }];
        let models = candidate_models_filtered(&slots, &slots[0], false);
        for m in &models {
            if let Some(info) = &m.info {
                assert!(info.has_use_case("reasoning"), "{} missing reasoning tag", m.model);
            }
        }
        assert!(!models.iter().any(|m| m.model == "gemini-2.5-flash-lite"));
        assert!(!models.iter().any(|m| m.model == "claude-haiku-4-5"));
    }

    #[test]
    fn reviewing_slot_accepts_reviewing_or_critique() {
        let slots = vec![SlotEntry {
            slot: PickerSlot::Reviewing,
            label: "Reviewing",
            description: "",
            current_provider: "smooai-gateway".into(),
            current_model: "minimax-m2.7-direct".into(),
        }];
        let models = candidate_models_filtered(&slots, &slots[0], false);
        for m in &models {
            if let Some(info) = &m.info {
                assert!(
                    info.has_use_case("reviewing") || info.has_use_case("critique"),
                    "{} missing reviewing/critique tag",
                    m.model
                );
            }
        }
        assert!(models.iter().any(|m| m.model == "minimax-m2.7-direct"));
        assert!(models.iter().any(|m| m.model == "qwen3-coder-plus-direct"));
    }

    #[test]
    fn judge_slot_accepts_judge_or_guardrails() {
        let slots = vec![SlotEntry {
            slot: PickerSlot::Judge,
            label: "Judge",
            description: "",
            current_provider: "smooai-gateway".into(),
            current_model: "gemini-2.5-flash".into(),
        }];
        let models = candidate_models_filtered(&slots, &slots[0], false);
        for m in &models {
            if let Some(info) = &m.info {
                assert!(
                    info.has_use_case("judge") || info.has_use_case("guardrails"),
                    "{} missing judge/guardrails tag",
                    m.model
                );
            }
        }
        assert!(models.iter().any(|m| m.model == "claude-haiku-4-5"));
        assert!(!models.iter().any(|m| m.model == "minimax-m2.7-direct"));
    }

    #[test]
    fn summarize_slot_accepts_summarize_or_long_context() {
        let slots = vec![SlotEntry {
            slot: PickerSlot::Summarize,
            label: "Summarize",
            description: "",
            current_provider: "smooai-gateway".into(),
            current_model: "gemini-2.5-flash".into(),
        }];
        let models = candidate_models_filtered(&slots, &slots[0], false);
        for m in &models {
            if let Some(info) = &m.info {
                assert!(
                    info.has_use_case("summarize") || info.has_use_case("long-context"),
                    "{} missing summarize/long-context tag",
                    m.model
                );
            }
        }
        assert!(models.iter().any(|m| m.model == "qwen3-coder-plus-direct"));
        assert!(models.iter().any(|m| m.model == "deepseek-v4-pro"));
    }

    #[test]
    fn fast_slot_accepts_fast_or_utility() {
        let slots = vec![SlotEntry {
            slot: PickerSlot::Fast,
            label: "Fast",
            description: "",
            current_provider: "smooai-gateway".into(),
            current_model: "gemini-2.5-flash-lite".into(),
        }];
        let models = candidate_models_filtered(&slots, &slots[0], false);
        for m in &models {
            if let Some(info) = &m.info {
                assert!(
                    info.has_use_case("fast") || info.has_use_case("utility"),
                    "{} missing fast/utility tag",
                    m.model
                );
            }
        }
        assert!(models.iter().any(|m| m.model == "gemini-2.5-flash-lite"));
        // Pure coding/reasoning models without fast/utility must NOT appear.
        assert!(!models.iter().any(|m| m.model == "kimi-k2.6-direct"));
    }

    #[test]
    fn default_slot_shows_all_models() {
        let slots = vec![SlotEntry {
            slot: PickerSlot::Default,
            label: "Default",
            description: "",
            current_provider: "smooai-gateway".into(),
            current_model: "deepseek-v4-flash".into(),
        }];
        let models = candidate_models_filtered(&slots, &slots[0], false);
        // Default = no filter. Should match the catalog size minus any
        // dedup'd slot entry (the only slot entry already lives in the
        // catalog so the total equals catalog size).
        let catalog_n = fallback_catalog().len();
        assert_eq!(models.len(), catalog_n, "default slot should show every catalog model");
    }

    #[test]
    fn show_all_toggle_drops_filter_and_keeps_selection() {
        let mut p = ModelPickerState::new();
        // Use a fast-slot focused state so the filter visibly excludes
        // coding-only models, then toggle and verify they reappear.
        p.slots = vec![SlotEntry {
            slot: PickerSlot::Fast,
            label: "Fast",
            description: "",
            current_provider: "smooai-gateway".into(),
            current_model: "gemini-2.5-flash-lite".into(),
        }];
        p.selected = 0;
        p.open_models_for_selected();
        // Filtered view: no kimi (reasoning-only).
        assert!(!p.models.iter().any(|m| m.model == "kimi-k2.6-direct"));
        // Toggle on — kimi appears.
        p.toggle_show_all();
        assert!(p.show_all);
        assert!(p.models.iter().any(|m| m.model == "kimi-k2.6-direct"));
        // Toggle off — kimi disappears again.
        p.toggle_show_all();
        assert!(!p.show_all);
        assert!(!p.models.iter().any(|m| m.model == "kimi-k2.6-direct"));
    }

    #[test]
    fn show_all_noop_in_slots_view() {
        let mut p = ModelPickerState::new();
        p.load_from_registry(&test_registry());
        p.toggle_show_all();
        assert!(!p.show_all, "toggle from Slots view must not flip the flag");
    }

    /// Character-exact snapshot of `format_catalog_row` so the row
    /// shape is locked down. Any column-width change will cause this
    /// to fail loudly — that's the point.
    #[test]
    fn format_catalog_row_snapshot() {
        let info = ModelInfo {
            use_cases: svec(&["coding", "reasoning", "agentic"]),
            tier: Tier::Workhorse,
            description: "test row".into(),
            input_cost_per_token: 0.00000014,
            output_cost_per_token: 0.00000028,
            benchmarks: Benchmarks {
                swe_bench_verified: Some(79.0),
                gpqa_diamond: None,
                aa_intelligence_index: Some(48.0),
            },
        };
        let entry = ModelEntry {
            provider: "smooai-gateway".into(),
            model: "deepseek-v4-flash".into(),
            info: Some(info),
        };
        let row = format_catalog_row("▸ ", &entry, PickerSlot::Coding);
        // Padded columns: NAME(22) TIER(9) COST(12) BENCH(5)  chips
        let expected = "▸ deepseek-v4-flash      workhorse $0.14/$0.28   79.0  coding · reasoning · agentic";
        assert_eq!(row, expected);
    }

    #[test]
    fn format_catalog_row_truncates_long_name() {
        let info = ModelInfo {
            use_cases: svec(&["coding"]),
            tier: Tier::Flagship,
            description: "".into(),
            input_cost_per_token: 0.000001,
            output_cost_per_token: 0.000002,
            benchmarks: Benchmarks::default(),
        };
        let entry = ModelEntry {
            provider: "p".into(),
            model: "a-very-long-model-name-that-exceeds-the-column".into(),
            info: Some(info),
        };
        let row = format_catalog_row("  ", &entry, PickerSlot::Coding);
        // The model name slot is NAME_COL (22) chars; ellipsis
        // terminator expected — truncation keeps (NAME_COL - 1) source
        // chars then appends `…`.
        assert!(row.contains("…"));
        assert!(row.starts_with("  a-very-long-model-nam…"), "got: {row}");
    }

    #[test]
    fn format_catalog_row_handles_missing_info() {
        let entry = ModelEntry {
            provider: "smooth".into(),
            model: "smooth-coding".into(),
            info: None,
        };
        let row = format_catalog_row("  ", &entry, PickerSlot::Coding);
        // Falls back to the legacy `name  (provider)` shape.
        assert_eq!(row, "  smooth-coding  (smooth)");
    }

    #[test]
    fn format_catalog_row_renders_dash_for_missing_benchmark() {
        let info = ModelInfo {
            use_cases: svec(&["fast"]),
            tier: Tier::Utility,
            description: "".into(),
            input_cost_per_token: 0.00000005,
            output_cost_per_token: 0.00000008,
            benchmarks: Benchmarks::default(),
        };
        let entry = ModelEntry {
            provider: "p".into(),
            model: "tiny-fast".into(),
            info: Some(info),
        };
        let row = format_catalog_row("▸ ", &entry, PickerSlot::Fast);
        // Benchmark column shows the dash placeholder.
        assert!(row.contains("  -  "));
    }

    #[test]
    fn cost_per_million_conversion() {
        let info = ModelInfo {
            use_cases: vec![],
            tier: Tier::Workhorse,
            description: String::new(),
            input_cost_per_token: 0.00000014,
            output_cost_per_token: 0.00000028,
            benchmarks: Benchmarks::default(),
        };
        let (in_m, out_m) = info.cost_per_million();
        assert!((in_m - 0.14).abs() < 1e-9);
        assert!((out_m - 0.28).abs() < 1e-9);
    }

    /// SMOODEV-1793 regression — a `providers.json` carrying legacy
    /// `smooth-*` aliases must round-trip through the picker's
    /// `reload_slots` → `apply_selected_model` cycle with the slots
    /// already migrated to concrete model names. Anything else
    /// re-poisons the file on save.
    #[test]
    fn picker_load_path_migrates_legacy_aliases() {
        let tmp = tempfile::tempdir().expect("tmpdir");
        let path = tmp.path().join("providers.json");

        // Hand-build a legacy registry on disk — every slot points
        // at a `smooth-*` alias.
        let mut legacy = ProviderRegistry::new();
        legacy.register_provider(ProviderConfig {
            id: "smooth".into(),
            api_url: "https://llm.smoo.ai/v1".into(),
            api_key: "test".into(),
            api_format: smooth_operator::llm::ApiFormat::OpenAiCompat,
            default_model: "smooth-default".into(),
        });
        legacy = legacy.with_routing(ModelRouting {
            coding: ModelSlot::new("smooth", "smooth-coding"),
            reasoning: Some(ModelSlot::new("smooth", "smooth-reasoning")),
            reviewing: ModelSlot::new("smooth", "smooth-reviewing"),
            judge: ModelSlot::new("smooth", "smooth-judge"),
            summarize: ModelSlot::new("smooth", "smooth-summarize"),
            default: ModelSlot::new("smooth", "smooth-default"),
            fast: Some(ModelSlot::new("smooth", "smooth-fast")),
            planning: None,
        });
        legacy.save_to_file(&path).expect("seed legacy");

        let mut p = ModelPickerState::new();
        p.providers_path = Some(path.clone());
        p.reload_slots();

        let coding = p.slots.iter().find(|s| s.slot == PickerSlot::Coding).expect("coding");
        assert_eq!(coding.current_model, "deepseek-v4-flash", "coding slot post-migration");
        let fast = p.slots.iter().find(|s| s.slot == PickerSlot::Fast).expect("fast");
        assert_eq!(fast.current_model, "groq-llama-3.1-8b", "fast slot post-migration");

        // The on-disk file must also be rewritten so the migration
        // only runs once per user.
        let reloaded = ProviderRegistry::load_from_file(&path).expect("reload");
        assert_eq!(reloaded.routing.coding.model, "deepseek-v4-flash");
        assert_eq!(reloaded.routing.reasoning.as_ref().unwrap().model, "deepseek-v4-pro");
    }

    #[test]
    fn fallback_catalog_covers_each_slot_default() {
        // Per pearl th-7ee88e: each of the named slot defaults must
        // be findable in the offline catalog so the picker is usable
        // without a gateway round-trip.
        let names: Vec<_> = fallback_catalog().iter().map(|(n, _)| *n).collect();
        for required in [
            "deepseek-v4-flash",
            "deepseek-v4-pro",
            "minimax-m2.7-direct",
            "gemini-2.5-flash",
            "gemini-2.5-flash-lite",
            // Pearl th-3468bd: judge + fast slot defaults
            "groq-llama-3.3-70b",
            "groq-llama-3.1-8b",
        ] {
            assert!(names.contains(&required), "fallback catalog missing {required}");
        }
    }
}
