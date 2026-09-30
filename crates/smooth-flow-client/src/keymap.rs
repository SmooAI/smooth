//! The keymap (spec §9): every keyboard action, its default chord on each
//! platform, the `keybindings.toml` override file, and conflicts.
//!
//! Action names are the Mac app's `FlowAction` raw values, so one
//! `~/.smooth/smoothflow/keybindings.toml` overrides both apps by name. The
//! chord wire form (`ctrl+opt+shift+cmd+key`) is the Mac app's `KeyChord`
//! form too. On Linux and Windows `cmd` (alias `super`) is the Super key.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

/// Which default table applies.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Platform {
    /// ⌘ is primary; ⌘⌥ is the fleet-action family.
    Mac,
    /// Linux and Windows: Ctrl+Shift is primary, because bare Ctrl+letter
    /// belongs to the program in the terminal; Ctrl+Alt is the fleet family.
    Other,
}

impl Platform {
    /// The platform this build runs on.
    #[must_use]
    pub const fn current() -> Self {
        if cfg!(target_os = "macos") {
            Self::Mac
        } else {
            Self::Other
        }
    }
}

macro_rules! actions {
    ($($variant:ident => $name:literal, $title:literal;)*) => {
        /// Every keyboard-reachable action.
        #[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
        pub enum Action {
            $(#[serde(rename = $name)] $variant,)*
        }

        impl Action {
            /// Every action, in menu order.
            pub const ALL: &'static [Self] = &[$(Self::$variant,)*];

            /// The name the override file uses (the Mac's `FlowAction` raw value).
            #[must_use]
            pub const fn name(self) -> &'static str {
                match self { $(Self::$variant => $name,)* }
            }

            /// The menu title.
            #[must_use]
            pub const fn title(self) -> &'static str {
                match self { $(Self::$variant => $title,)* }
            }

            /// The action named `name`, if any.
            #[must_use]
            pub fn from_name(name: &str) -> Option<Self> {
                match name { $($name => Some(Self::$variant),)* _ => None }
            }
        }
    };
}

actions! {
    NewSession => "newSession", "New Session…";
    NewShell => "newShell", "New Shell Here";
    FanOut => "fanOut", "Fan Out…";
    SteerFocused => "steerFocused", "Steer Focused…";
    SteerAll => "steerAll", "Steer All Working";
    Allow => "allow", "Approve (Allow)";
    Deny => "deny", "Deny";
    KillResume => "killResume", "Kill & Resume";
    Kill => "kill", "Kill";
    CloseOut => "closeOut", "Close Out…";
    FocusSession1 => "focusSession1", "Focus Session 1";
    FocusSession2 => "focusSession2", "Focus Session 2";
    FocusSession3 => "focusSession3", "Focus Session 3";
    FocusSession4 => "focusSession4", "Focus Session 4";
    FocusSession5 => "focusSession5", "Focus Session 5";
    FocusSession6 => "focusSession6", "Focus Session 6";
    FocusSession7 => "focusSession7", "Focus Session 7";
    FocusSession8 => "focusSession8", "Focus Session 8";
    FocusSession9 => "focusSession9", "Focus Session 9";
    NewTab => "newTab", "New Tab";
    ClosePane => "closePane", "Close Pane";
    CloseTab => "closeTab", "Close Tab";
    PreviousTab => "previousTab", "Previous Tab";
    NextTab => "nextTab", "Next Tab";
    SplitRight => "splitRight", "Split Right";
    SplitDown => "splitDown", "Split Down";
    SplitLeft => "splitLeft", "Split Left";
    SplitUp => "splitUp", "Split Up";
    FocusPaneLeft => "focusPaneLeft", "Focus Pane Left";
    FocusPaneRight => "focusPaneRight", "Focus Pane Right";
    FocusPaneUp => "focusPaneUp", "Focus Pane Up";
    FocusPaneDown => "focusPaneDown", "Focus Pane Down";
    ZoomPane => "zoomPane", "Zoom Pane";
    EqualizePanes => "equalizePanes", "Equalize Panes";
    Inbox => "inbox", "Inbox";
    ViewTerminal => "viewTerminal", "Terminal";
    ViewDiff => "viewDiff", "Diff";
    ViewPr => "viewPR", "PR";
    ViewActivity => "viewActivity", "Activity";
    ToggleSidebar => "toggleSidebar", "Toggle Sidebar";
    TogglePearlRail => "togglePearlRail", "Toggle Pearl Rail";
    Settings => "settings", "Settings…";
}

impl Action {
    /// 0-based fleet index for the nine focus actions.
    #[must_use]
    pub fn focus_session_index(self) -> Option<usize> {
        self.name().strip_prefix("focusSession").and_then(|n| n.parse::<usize>().ok()).map(|n| n - 1)
    }

    /// The shipped chord on `platform`, or `None` when the action is menu-only.
    #[must_use]
    pub fn default_chord(self, platform: Platform) -> Option<Chord> {
        let c = |spec: &str| Chord::parse(spec);
        if let Some(i) = self.focus_session_index() {
            let n = i + 1;
            return match platform {
                Platform::Mac => c(&format!("cmd+{n}")),
                Platform::Other => c(&format!("alt+{n}")),
            };
        }
        let (mac, other) = match self {
            Self::NewSession => ("cmd+n", "ctrl+shift+n"),
            Self::NewShell => ("cmd+shift+t", "ctrl+shift+alt+t"),
            Self::FanOut => ("cmd+shift+n", "ctrl+shift+alt+n"),
            Self::SteerFocused => ("cmd+enter", "ctrl+enter"),
            Self::SteerAll => ("cmd+opt+enter", "ctrl+alt+enter"),
            Self::Allow => ("cmd+opt+y", "ctrl+alt+y"),
            Self::Deny => ("cmd+opt+n", "ctrl+alt+n"),
            Self::KillResume => ("cmd+opt+r", "ctrl+alt+r"),
            Self::Kill => ("cmd+opt+k", "ctrl+alt+k"),
            Self::CloseOut => ("cmd+opt+w", "ctrl+alt+w"),
            Self::NewTab => ("cmd+t", "ctrl+shift+t"),
            Self::ClosePane => ("cmd+w", "ctrl+shift+w"),
            Self::CloseTab => ("cmd+shift+w", "ctrl+shift+alt+w"),
            Self::PreviousTab => ("cmd+shift+[", "ctrl+pageup"),
            Self::NextTab => ("cmd+shift+]", "ctrl+pagedown"),
            Self::SplitRight => ("cmd+d", "ctrl+shift+d"),
            Self::SplitDown => ("cmd+shift+d", "ctrl+shift+alt+d"),
            Self::SplitLeft => ("cmd+shift+left", "ctrl+shift+left"),
            Self::SplitUp => ("cmd+shift+up", "ctrl+shift+up"),
            Self::FocusPaneLeft => ("cmd+opt+left", "ctrl+alt+left"),
            Self::FocusPaneRight => ("cmd+opt+right", "ctrl+alt+right"),
            Self::FocusPaneUp => ("cmd+opt+up", "ctrl+alt+up"),
            Self::FocusPaneDown => ("cmd+opt+down", "ctrl+alt+down"),
            Self::ZoomPane => ("cmd+shift+enter", "ctrl+shift+enter"),
            Self::EqualizePanes => ("cmd+opt+=", "ctrl+alt+="),
            Self::Inbox => ("cmd+i", "ctrl+shift+i"),
            Self::ViewTerminal => ("cmd+opt+1", "ctrl+alt+1"),
            Self::ViewDiff => ("cmd+opt+2", "ctrl+alt+2"),
            Self::ViewPr => ("cmd+opt+3", "ctrl+alt+3"),
            Self::ViewActivity => ("cmd+opt+4", "ctrl+alt+4"),
            Self::ToggleSidebar => ("ctrl+cmd+s", "ctrl+shift+alt+s"),
            Self::TogglePearlRail => ("ctrl+cmd+p", "ctrl+shift+alt+p"),
            Self::Settings => ("cmd+,", "ctrl+,"),
            _ => return None,
        };
        c(match platform {
            Platform::Mac => mac,
            Platform::Other => other,
        })
    }
}

/// Named keys a chord may use besides single characters and `f1`–`f20`.
const NAMED_KEYS: &[&str] = &[
    "enter",
    "tab",
    "space",
    "escape",
    "backspace",
    "delete",
    "left",
    "right",
    "up",
    "down",
    "home",
    "end",
    "pageup",
    "pagedown",
];

/// Spellings people type, folded onto the canonical name.
fn key_alias(k: &str) -> &str {
    match k {
        "return" | "cr" => "enter",
        "esc" => "escape",
        "bs" => "backspace",
        "del" => "delete",
        "arrowleft" => "left",
        "arrowright" => "right",
        "arrowup" => "up",
        "arrowdown" => "down",
        "pgup" => "pageup",
        "pgdn" | "pagedn" => "pagedown",
        "plus" => "+",
        "minus" => "-",
        "equal" => "=",
        other => other,
    }
}

fn is_function_key(k: &str) -> bool {
    k.strip_prefix('f').and_then(|n| n.parse::<u8>().ok()).is_some_and(|n| (1..=20).contains(&n))
}

/// One shortcut: a key plus modifiers. `cmd` is ⌘ on the Mac and Super
/// elsewhere; `alt` is ⌥ on the Mac.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[allow(clippy::struct_excessive_bools, reason = "one flag per modifier key, as the wire form spells them")]
pub struct Chord {
    /// A single lowercase character, a named key, or `f1`–`f20`.
    pub key: String,
    #[serde(default)]
    pub ctrl: bool,
    #[serde(default)]
    pub alt: bool,
    #[serde(default)]
    pub shift: bool,
    #[serde(default)]
    pub cmd: bool,
}

impl Chord {
    /// `"ctrl+shift+["` → a chord; `None` when the text names no key, two
    /// keys, a repeated modifier, or a key it doesn't know. Case- and
    /// space-insensitive; `+` is both separator and key (`ctrl++` is Ctrl and +).
    #[must_use]
    pub fn parse(raw: &str) -> Option<Self> {
        let text = raw.trim().to_lowercase();
        if text.is_empty() {
            return None;
        }
        let mut parts: Vec<String> = text.split('+').map(str::to_string).collect();
        // `ctrl++` splits to ["ctrl", "", ""]: two trailing empties are the
        // `+` key. One (`ctrl+`) is a chord with no key.
        let n = parts.len();
        if n >= 2 && parts[n - 1].is_empty() && parts[n - 2].is_empty() {
            parts.pop();
            if let Some(last) = parts.last_mut() {
                *last = "+".to_string();
            }
        }
        let mut chord = Self {
            key: String::new(),
            ctrl: false,
            alt: false,
            shift: false,
            cmd: false,
        };
        let mut key: Option<String> = None;
        for part in &parts {
            let p = part.trim();
            if p.is_empty() {
                return None;
            }
            let flag = match p {
                "cmd" | "command" | "super" | "meta" | "win" => Some(&mut chord.cmd),
                "shift" => Some(&mut chord.shift),
                "opt" | "option" | "alt" => Some(&mut chord.alt),
                "ctrl" | "control" => Some(&mut chord.ctrl),
                _ => None,
            };
            if let Some(flag) = flag {
                if *flag {
                    return None;
                }
                *flag = true;
                continue;
            }
            if key.is_some() {
                return None;
            }
            key = Some(key_alias(p).to_string());
        }
        let k = key?;
        if !NAMED_KEYS.contains(&k.as_str()) && !is_function_key(&k) && k.chars().count() != 1 {
            return None;
        }
        chord.key = k;
        Some(chord)
    }

    #[must_use]
    pub const fn has_modifier(&self) -> bool {
        self.ctrl || self.alt || self.shift || self.cmd
    }

    /// The canonical file form, the Mac's order: `ctrl+opt+shift+cmd+key`.
    #[must_use]
    pub fn wire(&self) -> String {
        let mut out: Vec<&str> = Vec::new();
        if self.ctrl {
            out.push("ctrl");
        }
        if self.alt {
            out.push("opt");
        }
        if self.shift {
            out.push("shift");
        }
        if self.cmd {
            out.push("cmd");
        }
        out.push(&self.key);
        out.join("+")
    }

    /// How a menu shows it: `⌃⌥⇧⌘↩` on the Mac, `Ctrl+Shift+Alt+T` elsewhere
    /// (the spec table's order).
    #[must_use]
    pub fn display(&self, platform: Platform) -> String {
        match platform {
            Platform::Mac => {
                let mut s = String::new();
                for (on, glyph) in [(self.ctrl, "⌃"), (self.alt, "⌥"), (self.shift, "⇧"), (self.cmd, "⌘")] {
                    if on {
                        s.push_str(glyph);
                    }
                }
                s.push_str(&mac_glyph(&self.key));
                s
            }
            Platform::Other => {
                let mut parts: Vec<String> = Vec::new();
                for (on, name) in [(self.ctrl, "Ctrl"), (self.shift, "Shift"), (self.alt, "Alt"), (self.cmd, "Super")] {
                    if on {
                        parts.push(name.to_string());
                    }
                }
                parts.push(key_label(&self.key));
                parts.join("+")
            }
        }
    }
}

fn mac_glyph(key: &str) -> String {
    match key {
        "enter" => "↩".into(),
        "tab" => "⇥".into(),
        "space" => "␣".into(),
        "escape" => "⎋".into(),
        "backspace" => "⌫".into(),
        "delete" => "⌦".into(),
        "left" => "←".into(),
        "right" => "→".into(),
        "up" => "↑".into(),
        "down" => "↓".into(),
        "home" => "↖".into(),
        "end" => "↘".into(),
        "pageup" => "⇞".into(),
        "pagedown" => "⇟".into(),
        other => other.to_uppercase(),
    }
}

fn key_label(key: &str) -> String {
    match key {
        "enter" => "Enter".into(),
        "tab" => "Tab".into(),
        "space" => "Space".into(),
        "escape" => "Esc".into(),
        "backspace" => "Backspace".into(),
        "delete" => "Delete".into(),
        "left" => "←".into(),
        "right" => "→".into(),
        "up" => "↑".into(),
        "down" => "↓".into(),
        "home" => "Home".into(),
        "end" => "End".into(),
        "pageup" => "PageUp".into(),
        "pagedown" => "PageDown".into(),
        other => other.to_uppercase(),
    }
}

/// The resolved keymap: defaults for a platform plus the user's overrides.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Keymap {
    pub platform: Platform,
    /// Only what differs from the defaults; `None` unbinds.
    pub overrides: BTreeMap<Action, Option<Chord>>,
    /// Lines of the file that could not be used, surfaced rather than swallowed.
    pub problems: Vec<String>,
}

impl Keymap {
    /// The shipped keymap.
    #[must_use]
    pub const fn defaults(platform: Platform) -> Self {
        Self {
            platform,
            overrides: BTreeMap::new(),
            problems: Vec::new(),
        }
    }

    /// The chord that fires `action` now.
    #[must_use]
    pub fn chord(&self, action: Action) -> Option<Chord> {
        self.overrides.get(&action).map_or_else(|| action.default_chord(self.platform), Clone::clone)
    }

    /// The action `chord` fires. With a conflict, the first in menu order —
    /// stable, and the conflict is reported by [`Self::conflicts`].
    #[must_use]
    pub fn action_for(&self, chord: &Chord) -> Option<Action> {
        Action::ALL.iter().copied().find(|a| self.chord(*a).as_ref() == Some(chord))
    }

    /// Chords bound to more than one action (menu order within each). Reported,
    /// never auto-resolved.
    #[must_use]
    pub fn conflicts(&self) -> Vec<(Chord, Vec<Action>)> {
        let mut by: Vec<(Chord, Vec<Action>)> = Vec::new();
        for a in Action::ALL {
            let Some(c) = self.chord(*a) else { continue };
            match by.iter_mut().find(|(k, _)| *k == c) {
                Some((_, v)) => v.push(*a),
                None => by.push((c, vec![*a])),
            }
        }
        by.retain(|(_, v)| v.len() > 1);
        by
    }

    /// The `[keys]` table of `keybindings.toml`: `action = "chord"`, or
    /// `action = ""` to unbind. Unknown actions, unparseable chords and
    /// modifier-less chords (they would swallow terminal input) become
    /// problems; everything else still loads. A default typed out by hand is
    /// not an override.
    #[must_use]
    pub fn parse(text: &str, platform: Platform) -> Self {
        let mut map = Self::defaults(platform);
        let mut in_keys = false;
        for (i, raw) in text.lines().enumerate() {
            let line = strip_comment(raw).trim();
            if line.is_empty() {
                continue;
            }
            if line.starts_with('[') {
                in_keys = line == "[keys]";
                continue;
            }
            if !in_keys {
                continue;
            }
            let n = i + 1;
            let Some((name, value)) = key_value(line) else {
                map.problems.push(format!("line {n}: not `action = \"chord\"` — {line}"));
                continue;
            };
            let Some(action) = Action::from_name(&name) else {
                map.problems.push(format!("line {n}: unknown action `{name}`"));
                continue;
            };
            if value.is_empty() {
                map.overrides.insert(action, None);
                continue;
            }
            let Some(chord) = Chord::parse(&value) else {
                map.problems.push(format!("line {n}: `{value}` is not a shortcut (try `ctrl+shift+d`)"));
                continue;
            };
            if !chord.has_modifier() {
                map.problems
                    .push(format!("line {n}: `{value}` has no modifier — a bare key would swallow terminal input"));
                continue;
            }
            map.overrides.insert(action, Some(chord));
        }
        map.overrides.retain(|a, c| *c != a.default_chord(platform));
        map
    }
}

/// Drop a trailing `#` comment, respecting a quoted `#`.
fn strip_comment(line: &str) -> &str {
    let mut in_quotes = false;
    for (i, ch) in line.char_indices() {
        if ch == '"' {
            in_quotes = !in_quotes;
        }
        if ch == '#' && !in_quotes {
            return &line[..i];
        }
    }
    line
}

/// `key = "value"` → (key, value); a bare value is accepted too.
fn key_value(line: &str) -> Option<(String, String)> {
    let (k, v) = line.split_once('=')?;
    let key = k.trim();
    let mut value = v.trim();
    if key.is_empty() {
        return None;
    }
    if value.len() >= 2 && value.starts_with('"') && value.ends_with('"') {
        value = &value[1..value.len() - 1];
    } else if value.contains('"') {
        return None;
    }
    Some((key.to_string(), value.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ch(s: &str) -> Chord {
        Chord::parse(s).unwrap_or_else(|| panic!("{s} parses"))
    }

    #[test]
    fn chords_parse_and_print_canonically() {
        assert_eq!(ch("Ctrl + Shift + N").wire(), "ctrl+shift+n");
        assert_eq!(ch("alt+ctrl+=").wire(), "ctrl+opt+=");
        assert_eq!(ch("ctrl++").key, "+");
        assert_eq!(ch("ctrl+plus").key, "+");
        assert_eq!(ch("super+return").wire(), "cmd+enter");
        assert_eq!(ch("ctrl+pgdn").key, "pagedown");
        assert_eq!(ch("ctrl+f12").key, "f12");
        assert_eq!(Chord::parse("ctrl+ctrl+a"), None, "a repeated modifier");
        assert_eq!(Chord::parse("ctrl+a+b"), None, "two keys");
        assert_eq!(Chord::parse("ctrl+banana"), None);
        assert_eq!(Chord::parse("ctrl+"), None, "no key");
        assert_eq!(Chord::parse(""), None);
        assert_eq!(ch("ctrl+shift+alt+t").display(Platform::Other), "Ctrl+Shift+Alt+T");
        assert_eq!(ch("cmd+shift+enter").display(Platform::Mac), "⇧⌘↩");
    }

    #[test]
    fn linux_defaults_follow_the_spec_table_and_never_conflict() {
        let k = Keymap::defaults(Platform::Other);
        assert_eq!(k.chord(Action::NewSession), Some(ch("ctrl+shift+n")));
        assert_eq!(k.chord(Action::FocusSession3), Some(ch("alt+3")));
        assert_eq!(k.chord(Action::FocusPaneLeft), Some(ch("ctrl+alt+left")));
        assert_eq!(k.action_for(&ch("ctrl+alt+y")), Some(Action::Allow));
        assert!(k.conflicts().is_empty(), "{:?}", k.conflicts());
        assert!(Keymap::defaults(Platform::Mac).conflicts().is_empty());
        for a in Action::ALL {
            let c = k.chord(*a).unwrap_or_else(|| panic!("{a:?} has a default"));
            assert!(c.ctrl || c.alt, "{a:?}: every Linux default carries Ctrl or Alt");
            assert!(!c.cmd, "{a:?}: Super is the desktop's");
        }
    }

    #[test]
    fn the_override_file_rebinds_unbinds_and_reports() {
        let text = "# mine\n[other]\nnewTab = \"ctrl+q\"\n[keys]\nnewTab = \"ctrl+shift+y\" # comment\nclosePane = \"\"\nsplitRight = \"ctrl+shift+d\"\nbogus = \"ctrl+x\"\nkill = \"k\"\ninbox = \"ctrl+nope\"\nnot a line\n";
        let k = Keymap::parse(text, Platform::Other);
        assert_eq!(k.chord(Action::NewTab), Some(ch("ctrl+shift+y")));
        assert_eq!(k.chord(Action::ClosePane), None);
        assert!(!k.overrides.contains_key(&Action::SplitRight), "a re-typed default is no override");
        assert_eq!(k.chord(Action::Kill), Some(ch("ctrl+alt+k")), "a bare key is refused");
        assert_eq!(k.problems.len(), 4, "{:?}", k.problems);
        let k = Keymap::parse("[keys]\nnewTab = \"ctrl+shift+w\"\n", Platform::Other);
        assert_eq!(k.conflicts(), vec![(ch("ctrl+shift+w"), vec![Action::NewTab, Action::ClosePane])]);
        assert_eq!(k.action_for(&ch("ctrl+shift+w")), Some(Action::NewTab), "menu order wins a conflict");
    }

    #[test]
    fn names_round_trip() {
        for a in Action::ALL {
            assert_eq!(Action::from_name(a.name()), Some(*a));
        }
        assert_eq!(Action::FocusSession9.focus_session_index(), Some(8));
        assert_eq!(Action::NewTab.focus_session_index(), None);
        assert_eq!(Action::ViewPr.name(), "viewPR");
    }
}
