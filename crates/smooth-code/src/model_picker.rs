//! Model picker popup for the Smooth TUI.
//!
//! Two-level picker opened by `/model`:
//!
//! 1. **Slots view** — one entry per routing slot
//!    (Coding / Reasoning / Reviewing / Judge / Summarize / Fast /
//!    Default), showing the model each slot currently routes to.
//! 2. **Models view** — entered from the Slots view by pressing
//!    Enter. Lists candidate models for that slot. Selecting one
//!    applies the routing change and persists it back to
//!    `~/.smooth/providers.json`.
//!
//! Input contract (handled in `app.rs`):
//!   - Up/Down: navigate the current view's list
//!   - Enter: drill into Models view, or apply a model selection
//!   - Esc: Models view → back to Slots; Slots view → close picker

use std::path::PathBuf;

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

/// One candidate model shown in the Models view.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModelEntry {
    pub provider: String,
    pub model: String,
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
        }
    }

    /// Show the picker, re-reading `providers.json` so the slot list
    /// reflects the current on-disk state.
    pub fn activate(&mut self) {
        self.active = true;
        self.view = PickerView::Slots;
        self.selected = 0;
        self.error = None;
        self.reload_slots();
    }

    pub fn deactivate(&mut self) {
        self.active = false;
        self.view = PickerView::Slots;
        self.selected = 0;
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
        self.models = candidate_models(&self.slots, &entry);
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

        match ProviderRegistry::load_from_file(&path) {
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
        match ProviderRegistry::load_from_file(path) {
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

/// Build the candidate list for a slot. Starts with every model
/// currently routed anywhere (so swapping between slots works out
/// of the box), then appends the known `smooth-*` semantic aliases
/// for llm.smoo.ai users. De-duplicates by (provider, model).
fn candidate_models(slots: &[SlotEntry], focused: &SlotEntry) -> Vec<ModelEntry> {
    let mut out: Vec<ModelEntry> = Vec::new();
    let mut push = |provider: String, model: String| {
        if !out.iter().any(|e| e.provider == provider && e.model == model) {
            out.push(ModelEntry { provider, model });
        }
    };

    // Current assignments first — the fast way to swap, say,
    // smooth-thinking into the Reviewing slot.
    for s in slots {
        push(s.current_provider.clone(), s.current_model.clone());
    }

    // Standard llm.smoo.ai aliases. Use the focused slot's provider
    // when it looks like a Smoo AI provider; otherwise fall back
    // to literal "smooth".
    let provider = if focused.current_provider == "smooai-gateway" {
        "smooai-gateway".to_string()
    } else {
        "smooth".to_string()
    };
    for alias in SMOOTH_ALIASES {
        push(provider.clone(), (*alias).to_string());
    }

    out
}

const SMOOTH_ALIASES: &[&str] = &[
    "smooth-coding",
    "smooth-reasoning",
    "smooth-reviewing",
    "smooth-judge",
    "smooth-summarize",
    "smooth-fast",
    "smooth-default",
];

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

#[cfg(test)]
mod tests {
    use std::path::Path;

    use smooth_operator::providers::{ModelRouting, Preset, ProviderConfig};

    use super::*;

    fn test_registry() -> ProviderRegistry {
        let mut r = ProviderRegistry::new();
        r.register_provider(ProviderConfig {
            id: "smooth".into(),
            api_url: "https://llm.smoo.ai/v1".into(),
            api_key: "test".into(),
            api_format: smooth_operator::llm::ApiFormat::OpenAiCompat,
            default_model: "smooth-default".into(),
        });
        r = r.with_routing(ModelRouting {
            coding: ModelSlot::new("smooth", "smooth-coding"),
            reasoning: Some(ModelSlot::new("smooth", "smooth-reasoning")),
            reviewing: ModelSlot::new("smooth", "smooth-reviewing"),
            judge: ModelSlot::new("smooth", "smooth-judge"),
            summarize: ModelSlot::new("smooth", "smooth-summarize"),
            default: ModelSlot::new("smooth", "smooth-default"),
            fast: Some(ModelSlot::new("smooth", "smooth-fast")),
            planning: None,
        });
        r
    }

    #[test]
    fn new_picker_is_inactive_and_on_slots_view() {
        let p = ModelPickerState::new();
        assert!(!p.active);
        assert_eq!(p.view, PickerView::Slots);
    }

    #[test]
    fn load_from_registry_populates_all_slots() {
        let mut p = ModelPickerState::new();
        p.load_from_registry(&test_registry());
        assert_eq!(p.slots.len(), ALL_SLOTS.len());
        assert_eq!(p.slots[0].slot, PickerSlot::Coding);
        assert_eq!(p.slots[0].current_model, "smooth-coding");
        let fast = p.slots.iter().find(|s| s.slot == PickerSlot::Fast).expect("fast slot");
        assert_eq!(fast.current_model, "smooth-fast");
        let reasoning = p.slots.iter().find(|s| s.slot == PickerSlot::Reasoning).expect("reasoning slot");
        assert_eq!(reasoning.current_model, "smooth-reasoning");
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
    }

    #[test]
    fn open_models_preselects_current_model() {
        let mut p = ModelPickerState::new();
        p.load_from_registry(&test_registry());
        // Drill into Reasoning — its current model is "smooth-reasoning".
        let idx = ALL_SLOTS.iter().position(|(s, _, _)| *s == PickerSlot::Reasoning).expect("reasoning slot");
        p.selected = idx;
        p.open_models_for_selected();
        let chosen = &p.models[p.selected];
        assert_eq!(chosen.model, "smooth-reasoning");
    }

    #[test]
    fn apply_selected_model_persists_and_returns_to_slots() {
        let tmp = tempfile::tempdir().expect("tmpdir");
        let path = tmp.path().join("providers.json");
        test_registry().save_to_file(&path).expect("seed file");

        let mut p = ModelPickerState::new();
        p.providers_path = Some(path.clone());
        p.reload_slots();

        // Drill into Coding; pick smooth-reasoning instead of smooth-coding.
        let coding_idx = ALL_SLOTS.iter().position(|(s, _, _)| *s == PickerSlot::Coding).expect("coding slot");
        p.selected = coding_idx;
        p.open_models_for_selected();
        let idx = p
            .models
            .iter()
            .position(|m| m.model == "smooth-reasoning")
            .expect("smooth-reasoning is always a candidate");
        p.selected = idx;
        assert!(p.apply_selected_model());

        // Back on Slots view, Coding now points at smooth-reasoning.
        assert_eq!(p.view, PickerView::Slots);
        let coding = p.slots.iter().find(|s| s.slot == PickerSlot::Coding).expect("coding");
        assert_eq!(coding.current_model, "smooth-reasoning");

        // And the on-disk file actually changed.
        let reloaded = ProviderRegistry::load_from_file(&path).expect("reload");
        assert_eq!(reloaded.routing.coding.model, "smooth-reasoning");
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
        p.deactivate();
        assert!(!p.active);
        assert_eq!(p.view, PickerView::Slots);
        assert_eq!(p.selected, 0);
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
}
