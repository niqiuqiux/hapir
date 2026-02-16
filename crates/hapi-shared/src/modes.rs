use serde::{Deserialize, Serialize};

// --- Permission Modes ---

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum PermissionMode {
    #[serde(rename = "default")]
    Default,
    #[serde(rename = "acceptEdits")]
    AcceptEdits,
    #[serde(rename = "bypassPermissions")]
    BypassPermissions,
    #[serde(rename = "plan")]
    Plan,
    #[serde(rename = "read-only")]
    ReadOnly,
    #[serde(rename = "safe-yolo")]
    SafeYolo,
    #[serde(rename = "yolo")]
    Yolo,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum ModelMode {
    #[serde(rename = "default")]
    Default,
    #[serde(rename = "sonnet")]
    Sonnet,
    #[serde(rename = "opus")]
    Opus,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AgentFlavor {
    Claude,
    Codex,
    Gemini,
    Opencode,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PermissionModeTone {
    Neutral,
    Info,
    Warning,
    Danger,
}

#[derive(Debug, Clone)]
pub struct PermissionModeOption {
    pub mode: PermissionMode,
    pub label: &'static str,
    pub tone: PermissionModeTone,
}

// --- Constants ---

pub const CLAUDE_PERMISSION_MODES: &[PermissionMode] = &[
    PermissionMode::Default,
    PermissionMode::AcceptEdits,
    PermissionMode::BypassPermissions,
    PermissionMode::Plan,
];

pub const CODEX_PERMISSION_MODES: &[PermissionMode] = &[
    PermissionMode::Default,
    PermissionMode::ReadOnly,
    PermissionMode::SafeYolo,
    PermissionMode::Yolo,
];

pub const GEMINI_PERMISSION_MODES: &[PermissionMode] = &[
    PermissionMode::Default,
    PermissionMode::ReadOnly,
    PermissionMode::SafeYolo,
    PermissionMode::Yolo,
];

pub const OPENCODE_PERMISSION_MODES: &[PermissionMode] =
    &[PermissionMode::Default, PermissionMode::Yolo];

pub const MODEL_MODES: &[ModelMode] = &[ModelMode::Default, ModelMode::Sonnet, ModelMode::Opus];

impl PermissionMode {
    pub fn label(self) -> &'static str {
        match self {
            Self::Default => "Default",
            Self::AcceptEdits => "Accept Edits",
            Self::Plan => "Plan Mode",
            Self::BypassPermissions => "Yolo",
            Self::ReadOnly => "Read Only",
            Self::SafeYolo => "Safe Yolo",
            Self::Yolo => "Yolo",
        }
    }

    pub fn tone(self) -> PermissionModeTone {
        match self {
            Self::Default => PermissionModeTone::Neutral,
            Self::AcceptEdits => PermissionModeTone::Warning,
            Self::Plan => PermissionModeTone::Info,
            Self::BypassPermissions => PermissionModeTone::Danger,
            Self::ReadOnly => PermissionModeTone::Warning,
            Self::SafeYolo => PermissionModeTone::Warning,
            Self::Yolo => PermissionModeTone::Danger,
        }
    }

    pub fn option(self) -> PermissionModeOption {
        PermissionModeOption {
            mode: self,
            label: self.label(),
            tone: self.tone(),
        }
    }
}

impl ModelMode {
    pub fn label(self) -> &'static str {
        match self {
            Self::Default => "Default",
            Self::Sonnet => "Sonnet",
            Self::Opus => "Opus",
        }
    }
}

pub fn permission_modes_for_flavor(flavor: Option<AgentFlavor>) -> &'static [PermissionMode] {
    match flavor {
        Some(AgentFlavor::Codex) => CODEX_PERMISSION_MODES,
        Some(AgentFlavor::Gemini) => GEMINI_PERMISSION_MODES,
        Some(AgentFlavor::Opencode) => OPENCODE_PERMISSION_MODES,
        _ => CLAUDE_PERMISSION_MODES,
    }
}

pub fn permission_mode_options_for_flavor(
    flavor: Option<AgentFlavor>,
) -> Vec<PermissionModeOption> {
    permission_modes_for_flavor(flavor)
        .iter()
        .map(|m| m.option())
        .collect()
}

pub fn is_permission_mode_allowed_for_flavor(
    mode: PermissionMode,
    flavor: Option<AgentFlavor>,
) -> bool {
    permission_modes_for_flavor(flavor).contains(&mode)
}

pub fn model_modes_for_flavor(flavor: Option<AgentFlavor>) -> &'static [ModelMode] {
    match flavor {
        Some(AgentFlavor::Codex | AgentFlavor::Gemini | AgentFlavor::Opencode) => &[],
        _ => MODEL_MODES,
    }
}

pub fn is_model_mode_allowed_for_flavor(mode: ModelMode, flavor: Option<AgentFlavor>) -> bool {
    model_modes_for_flavor(flavor).contains(&mode)
}

// --- Tests ---

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn permission_mode_serde_roundtrip() {
        for mode in [
            PermissionMode::Default,
            PermissionMode::AcceptEdits,
            PermissionMode::BypassPermissions,
            PermissionMode::Plan,
            PermissionMode::ReadOnly,
            PermissionMode::SafeYolo,
            PermissionMode::Yolo,
        ] {
            let json = serde_json::to_string(&mode).unwrap();
            let back: PermissionMode = serde_json::from_str(&json).unwrap();
            assert_eq!(mode, back);
        }
    }

    #[test]
    fn model_mode_serde_roundtrip() {
        for mode in [ModelMode::Default, ModelMode::Sonnet, ModelMode::Opus] {
            let json = serde_json::to_string(&mode).unwrap();
            let back: ModelMode = serde_json::from_str(&json).unwrap();
            assert_eq!(mode, back);
        }
    }

    #[test]
    fn agent_flavor_serde_roundtrip() {
        for flavor in [
            AgentFlavor::Claude,
            AgentFlavor::Codex,
            AgentFlavor::Gemini,
            AgentFlavor::Opencode,
        ] {
            let json = serde_json::to_string(&flavor).unwrap();
            let back: AgentFlavor = serde_json::from_str(&json).unwrap();
            assert_eq!(flavor, back);
        }
    }

    #[test]
    fn permission_mode_labels() {
        assert_eq!(PermissionMode::Default.label(), "Default");
        assert_eq!(PermissionMode::AcceptEdits.label(), "Accept Edits");
        assert_eq!(PermissionMode::BypassPermissions.label(), "Yolo");
    }

    #[test]
    fn flavor_permission_modes() {
        assert_eq!(
            permission_modes_for_flavor(Some(AgentFlavor::Claude)),
            CLAUDE_PERMISSION_MODES
        );
        assert_eq!(
            permission_modes_for_flavor(Some(AgentFlavor::Codex)),
            CODEX_PERMISSION_MODES
        );
        assert!(is_permission_mode_allowed_for_flavor(
            PermissionMode::Plan,
            Some(AgentFlavor::Claude)
        ));
        assert!(!is_permission_mode_allowed_for_flavor(
            PermissionMode::Plan,
            Some(AgentFlavor::Codex)
        ));
    }

    #[test]
    fn flavor_model_modes() {
        assert_eq!(model_modes_for_flavor(Some(AgentFlavor::Claude)), MODEL_MODES);
        assert!(model_modes_for_flavor(Some(AgentFlavor::Codex)).is_empty());
    }
}
