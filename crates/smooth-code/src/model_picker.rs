//! Model picker popup for switching LLM models in the TUI.
//!
//! Provides a [`ModelPickerState`] that manages a filterable list of
//! [`ProviderOption`]s. The picker is activated via `/model` (no args)
//! and renders as a centered overlay.

/// A single option in the model picker list.
#[derive(Debug, Clone)]
pub struct ProviderOption {
    /// Human-readable name shown in the picker.
    pub display_name: String,
    /// Provider identifier (e.g. `"openrouter"`, `"ollama"`).
    pub provider: String,
    /// Model identifier passed to the LLM client.
    pub model: String,
    /// If set, only changes the model for this activity slot.
    pub activity: Option<String>,
}

/// State for the model picker popup overlay.
#[derive(Debug, Clone)]
pub struct ModelPickerState {
    /// Whether the picker is currently visible.
    pub active: bool,
    /// Available model options.
    pub providers: Vec<ProviderOption>,
    /// Currently highlighted index.
    pub selected: usize,
    /// Text filter (not yet wired to UI, reserved for future use).
    pub filter: String,
}

impl Default for ModelPickerState {
    fn default() -> Self {
        Self::new()
    }
}

impl ModelPickerState {
    /// Create a new inactive picker with default model options.
    pub fn new() -> Self {
        let mut state = Self {
            active: false,
            providers: Vec::new(),
            selected: 0,
            filter: String::new(),
        };
        state.populate_defaults();
        state
    }

    /// Show the picker overlay.
    pub fn activate(&mut self) {
        self.active = true;
        self.selected = 0;
        self.filter.clear();
    }

    /// Hide the picker overlay.
    pub fn deactivate(&mut self) {
        self.active = false;
    }

    /// Move selection up by one, wrapping to the bottom.
    pub fn select_up(&mut self) {
        if self.providers.is_empty() {
            return;
        }
        if self.selected == 0 {
            self.selected = self.providers.len() - 1;
        } else {
            self.selected -= 1;
        }
    }

    /// Move selection down by one, wrapping to the top.
    pub fn select_down(&mut self) {
        if self.providers.is_empty() {
            return;
        }
        self.selected = (self.selected + 1) % self.providers.len();
    }

    /// Return a reference to the currently highlighted option.
    pub fn selected_option(&self) -> Option<&ProviderOption> {
        self.providers.get(self.selected)
    }

    /// Populate the picker with common model options.
    pub fn populate_defaults(&mut self) {
        self.providers = vec![
            ProviderOption {
                display_name: "GPT-4o".into(),
                provider: "openrouter".into(),
                model: "openai/gpt-4o".into(),
                activity: None,
            },
            ProviderOption {
                display_name: "DeepSeek R1".into(),
                provider: "openrouter".into(),
                model: "deepseek/deepseek-r1".into(),
                activity: None,
            },
            ProviderOption {
                display_name: "DeepSeek V3".into(),
                provider: "openrouter".into(),
                model: "deepseek/deepseek-v3".into(),
                activity: None,
            },
            ProviderOption {
                display_name: "Kimi K2.5".into(),
                provider: "openrouter".into(),
                model: "moonshot/kimi-k2.5".into(),
                activity: None,
            },
            ProviderOption {
                display_name: "MiniMax M2.5".into(),
                provider: "openrouter".into(),
                model: "minimax/minimax-m2.5".into(),
                activity: None,
            },
            ProviderOption {
                display_name: "GLM-5.1".into(),
                provider: "openrouter".into(),
                model: "zhipu/glm-5.1".into(),
                activity: None,
            },
            ProviderOption {
                display_name: "Gemini Flash".into(),
                provider: "openrouter".into(),
                model: "google/gemini-flash-2.0".into(),
                activity: None,
            },
            ProviderOption {
                display_name: "GPT-4o-mini".into(),
                provider: "openrouter".into(),
                model: "openai/gpt-4o-mini".into(),
                activity: None,
            },
            ProviderOption {
                display_name: "Local: Llama 3.1".into(),
                provider: "ollama".into(),
                model: "llama3.1".into(),
                activity: None,
            },
        ];
        self.selected = 0;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn model_picker_state_creation_with_defaults() {
        let picker = ModelPickerState::new();
        assert!(!picker.active);
        assert!(!picker.providers.is_empty());
        assert_eq!(picker.selected, 0);
        assert!(picker.filter.is_empty());
    }

    #[test]
    fn populate_defaults_has_expected_models() {
        let picker = ModelPickerState::new();
        let names: Vec<&str> = picker.providers.iter().map(|p| p.display_name.as_str()).collect();
        assert!(names.contains(&"GPT-4o"));
        assert!(names.contains(&"DeepSeek R1"));
        assert!(names.contains(&"DeepSeek V3"));
        assert!(names.contains(&"Kimi K2.5"));
        assert!(names.contains(&"MiniMax M2.5"));
        assert!(names.contains(&"GLM-5.1"));
        assert!(names.contains(&"Gemini Flash"));
        assert!(names.contains(&"GPT-4o-mini"));
        assert!(names.contains(&"Local: Llama 3.1"));
        assert_eq!(picker.providers.len(), 9);
    }

    #[test]
    fn select_up_down_navigation() {
        let mut picker = ModelPickerState::new();
        assert_eq!(picker.selected, 0);

        picker.select_down();
        assert_eq!(picker.selected, 1);

        picker.select_down();
        assert_eq!(picker.selected, 2);

        picker.select_up();
        assert_eq!(picker.selected, 1);

        // Wrap around at top
        picker.selected = 0;
        picker.select_up();
        assert_eq!(picker.selected, picker.providers.len() - 1);

        // Wrap around at bottom
        picker.selected = picker.providers.len() - 1;
        picker.select_down();
        assert_eq!(picker.selected, 0);
    }

    #[test]
    fn selected_option_returns_correct_item() {
        let mut picker = ModelPickerState::new();
        let first = picker.selected_option().unwrap();
        assert_eq!(first.display_name, "GPT-4o");

        picker.select_down();
        let second = picker.selected_option().unwrap();
        assert_eq!(second.display_name, "DeepSeek R1");

        // Out-of-bounds returns None
        let mut empty_picker = ModelPickerState::new();
        empty_picker.providers.clear();
        assert!(empty_picker.selected_option().is_none());
    }

    #[test]
    fn activate_deactivate_toggle() {
        let mut picker = ModelPickerState::new();
        assert!(!picker.active);

        picker.activate();
        assert!(picker.active);
        assert_eq!(picker.selected, 0);

        picker.deactivate();
        assert!(!picker.active);
    }

    #[test]
    fn provider_option_has_all_fields() {
        let option = ProviderOption {
            display_name: "Test Model".into(),
            provider: "test-provider".into(),
            model: "test/model-v1".into(),
            activity: Some("coding".into()),
        };
        assert_eq!(option.display_name, "Test Model");
        assert_eq!(option.provider, "test-provider");
        assert_eq!(option.model, "test/model-v1");
        assert_eq!(option.activity, Some("coding".into()));

        let option_no_activity = ProviderOption {
            display_name: "Generic".into(),
            provider: "openrouter".into(),
            model: "openai/gpt-4o".into(),
            activity: None,
        };
        assert!(option_no_activity.activity.is_none());
    }
}
