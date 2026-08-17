use std::{collections::BTreeMap, path::Path};

use serde::{Deserialize, Serialize};

use crate::AppError;

/// Actions that may be bound to one terminal shortcut. Names are stable configuration keys.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum KeyAction {
    OpenHelp,
    OpenModelPicker,
    SendMessage,
    InterruptExecution,
    NewConversation,
    ToggleStatusPanel,
    OpenCommandList,
    HistoryPrevious,
    HistoryNext,
    Approve,
    Reject,
    ToggleVimMode,
}

impl KeyAction {
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::OpenHelp => "open-help",
            Self::OpenModelPicker => "open-model-picker",
            Self::SendMessage => "send-message",
            Self::InterruptExecution => "interrupt-execution",
            Self::NewConversation => "new-conversation",
            Self::ToggleStatusPanel => "toggle-status-panel",
            Self::OpenCommandList => "open-command-list",
            Self::HistoryPrevious => "history-previous",
            Self::HistoryNext => "history-next",
            Self::Approve => "approve",
            Self::Reject => "reject",
            Self::ToggleVimMode => "toggle-vim-mode",
        }
    }

    #[must_use]
    pub const fn all() -> &'static [Self] {
        &[
            Self::OpenHelp,
            Self::OpenModelPicker,
            Self::SendMessage,
            Self::InterruptExecution,
            Self::NewConversation,
            Self::ToggleStatusPanel,
            Self::OpenCommandList,
            Self::HistoryPrevious,
            Self::HistoryNext,
            Self::Approve,
            Self::Reject,
            Self::ToggleVimMode,
        ]
    }

    #[must_use]
    pub fn parse(value: &str) -> Option<Self> {
        Self::all()
            .iter()
            .copied()
            .find(|action| action.label() == value)
    }
}

/// A named visual palette. Definitions remain in the interface, only the name is persisted.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ThemeName {
    #[default]
    System,
    Dark,
    Light,
    HighContrast,
}

impl ThemeName {
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::System => "system",
            Self::Dark => "dark",
            Self::Light => "light",
            Self::HighContrast => "high-contrast",
        }
    }
    #[must_use]
    pub const fn all() -> &'static [Self] {
        &[Self::System, Self::Dark, Self::Light, Self::HighContrast]
    }
    #[must_use]
    pub fn parse(value: &str) -> Option<Self> {
        Self::all()
            .iter()
            .copied()
            .find(|theme| theme.label() == value)
    }
}

/// Presentation-only response styles; never grant capabilities or affect permissions.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum PersonalityName {
    #[default]
    Default,
    Concise,
    Explanatory,
    Pragmatic,
    Mentor,
}

impl PersonalityName {
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Default => "default",
            Self::Concise => "concise",
            Self::Explanatory => "explanatory",
            Self::Pragmatic => "pragmatic",
            Self::Mentor => "mentor",
        }
    }
    #[must_use]
    pub const fn all() -> &'static [Self] {
        &[
            Self::Default,
            Self::Concise,
            Self::Explanatory,
            Self::Pragmatic,
            Self::Mentor,
        ]
    }
    #[must_use]
    pub fn parse(value: &str) -> Option<Self> {
        Self::all()
            .iter()
            .copied()
            .find(|personality| personality.label() == value)
    }
}

/// Versioned global UI preferences. Unknown TOML fields are ignored for forward compatibility.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Preferences {
    pub schema_version: u32,
    #[serde(default)]
    pub keymap: BTreeMap<KeyAction, String>,
    #[serde(default)]
    pub theme: ThemeName,
    #[serde(default)]
    pub personality: PersonalityName,
}

impl Default for Preferences {
    fn default() -> Self {
        Self {
            schema_version: 1,
            keymap: default_keymap(),
            theme: ThemeName::System,
            personality: PersonalityName::Default,
        }
    }
}

#[must_use]
pub fn default_keymap() -> BTreeMap<KeyAction, String> {
    [
        (KeyAction::OpenHelp, "f1"),
        (KeyAction::OpenModelPicker, "ctrl+m"),
        (KeyAction::SendMessage, "enter"),
        (KeyAction::InterruptExecution, "esc"),
        (KeyAction::NewConversation, "ctrl+n"),
        (KeyAction::ToggleStatusPanel, "ctrl+s"),
        (KeyAction::OpenCommandList, "ctrl+p"),
        (KeyAction::HistoryPrevious, "up"),
        (KeyAction::HistoryNext, "down"),
        (KeyAction::Approve, "y"),
        (KeyAction::Reject, "n"),
        (KeyAction::ToggleVimMode, "ctrl+v"),
    ]
    .into_iter()
    .map(|(action, binding)| (action, binding.into()))
    .collect()
}

/// Result of loading preferences: malformed files never block startup or get replaced.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PreferencesLoad {
    pub preferences: Preferences,
    pub recovered_from_error: Option<String>,
}

#[must_use]
pub fn load_or_default_preferences(path: &Path) -> PreferencesLoad {
    match std::fs::read_to_string(path) {
        Ok(contents) => match toml::from_str::<Preferences>(&contents) {
            Ok(preferences) if preferences.schema_version == 1 => PreferencesLoad {
                preferences: normalize(preferences),
                recovered_from_error: None,
            },
            Ok(preferences) => PreferencesLoad {
                preferences: Preferences::default(),
                recovered_from_error: Some(format!(
                    "preferences.toml schema_version {} is unsupported",
                    preferences.schema_version
                )),
            },
            Err(error) => PreferencesLoad {
                preferences: Preferences::default(),
                recovered_from_error: Some(format!("could not parse preferences.toml: {error}")),
            },
        },
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => PreferencesLoad {
            preferences: Preferences::default(),
            recovered_from_error: None,
        },
        Err(error) => PreferencesLoad {
            preferences: Preferences::default(),
            recovered_from_error: Some(format!("could not read preferences.toml: {error}")),
        },
    }
}

fn normalize(mut preferences: Preferences) -> Preferences {
    for (action, binding) in default_keymap() {
        preferences.keymap.entry(action).or_insert(binding);
    }
    preferences
}

/// # Errors
///
/// Returns an error if `preferences` cannot be serialized to TOML or if the
/// atomic write to `path` fails.
pub fn save_preferences(path: &Path, preferences: &Preferences) -> Result<(), AppError> {
    let contents = toml::to_string_pretty(preferences).map_err(|error| {
        AppError::Configuration(format!("could not serialize preferences.toml: {error}"))
    })?;
    crate::atomic_write(path, &contents)
}

/// Basic, portable shortcut syntax used by `/keymap`: modifiers plus a named or single-char key.
#[must_use]
pub fn valid_shortcut(shortcut: &str) -> bool {
    let normalized = shortcut.to_ascii_lowercase();
    let parts: Vec<_> = normalized.split('+').map(str::trim).collect();
    if parts.is_empty() || parts.iter().any(|part| part.is_empty()) {
        return false;
    }
    let key = parts.last().copied().unwrap_or_default();
    let modifiers_ok = parts[..parts.len().saturating_sub(1)]
        .iter()
        .all(|modifier| matches!(*modifier, "ctrl" | "alt" | "shift"));
    let key_ok = key.chars().count() == 1
        || matches!(
            key,
            "enter"
                | "esc"
                | "tab"
                | "up"
                | "down"
                | "left"
                | "right"
                | "pageup"
                | "pagedown"
                | "home"
                | "end"
                | "f1"
                | "f2"
                | "f3"
                | "f4"
                | "f5"
                | "f6"
                | "f7"
                | "f8"
                | "f9"
                | "f10"
                | "f11"
                | "f12"
        );
    modifiers_ok && key_ok
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        fs,
        time::{SystemTime, UNIX_EPOCH},
    };

    fn fixture() -> std::path::PathBuf {
        let id = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock")
            .as_nanos();
        let path = std::env::temp_dir().join(format!("gocode-preferences-{id}"));
        fs::create_dir_all(&path).expect("fixture");
        path
    }

    #[test]
    fn custom_keymap_and_theme_persist_and_reload() {
        let root = fixture();
        let path = root.join("preferences.toml");
        let mut preferences = Preferences::default();
        preferences
            .keymap
            .insert(KeyAction::SendMessage, "ctrl+enter".into());
        preferences.theme = ThemeName::Light;
        save_preferences(&path, &preferences).expect("save");
        assert_eq!(load_or_default_preferences(&path).preferences, preferences);
        fs::remove_dir_all(root).expect("cleanup");
    }

    #[test]
    fn invalid_preferences_fall_back_without_overwriting_the_file() {
        let root = fixture();
        let path = root.join("preferences.toml");
        fs::write(&path, "not = [valid").expect("write invalid");
        let loaded = load_or_default_preferences(&path);
        assert!(loaded.recovered_from_error.is_some());
        assert_eq!(loaded.preferences, Preferences::default());
        assert_eq!(fs::read_to_string(&path).expect("read"), "not = [valid");
        fs::remove_dir_all(root).expect("cleanup");
    }

    #[test]
    fn shortcut_validation_rejects_invalid_forms() {
        assert!(valid_shortcut("ctrl+enter"));
        assert!(!valid_shortcut("hyper+z"));
    }

    #[test]
    fn unknown_preference_fields_are_ignored_for_forward_compatibility() {
        let root = fixture();
        let path = root.join("preferences.toml");
        fs::write(
            &path,
            "schema_version = 1\nfuture_option = true\ntheme = 'light'\n",
        )
        .expect("write preferences");
        let loaded = load_or_default_preferences(&path);
        assert_eq!(loaded.preferences.theme, ThemeName::Light);
        assert!(loaded.recovered_from_error.is_none());
        fs::remove_dir_all(root).expect("cleanup");
    }
}
