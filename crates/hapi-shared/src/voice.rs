use serde::{Deserialize, Serialize};

pub const ELEVENLABS_API_BASE: &str = "https://api.elevenlabs.io/v1";
pub const VOICE_AGENT_NAME: &str = "Hapi Voice Assistant";
pub const VOICE_FIRST_MESSAGE: &str = "Hey! Hapi here.";

pub const VOICE_SYSTEM_PROMPT: &str = include_str!("voice_prompt.txt");

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VoiceTool {
    #[serde(rename = "type")]
    pub tool_type: String,
    pub name: String,
    pub description: String,
    pub expects_response: bool,
    pub response_timeout_secs: u32,
    pub parameters: serde_json::Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VoiceAgentConfig {
    pub name: String,
    pub conversation_config: ConversationConfig,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub platform_settings: Option<PlatformSettings>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConversationConfig {
    pub agent: AgentConfig,
    pub turn: TurnConfig,
    pub tts: TtsConfig,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentConfig {
    pub first_message: String,
    pub language: String,
    pub prompt: PromptConfig,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PromptConfig {
    pub prompt: String,
    pub llm: String,
    pub temperature: f64,
    pub max_tokens: u32,
    pub tools: Vec<VoiceTool>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TurnConfig {
    pub turn_timeout: f64,
    pub silence_end_call_timeout: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TtsConfig {
    pub voice_id: String,
    pub model_id: String,
    pub speed: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlatformSettings {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub overrides: Option<PlatformOverrides>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlatformOverrides {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub conversation_config_override: Option<ConversationConfigOverride>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConversationConfigOverride {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub agent: Option<AgentOverride>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentOverride {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub language: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub first_message: Option<bool>,
}

pub fn voice_tools() -> Vec<VoiceTool> {
    vec![
        VoiceTool {
            tool_type: "client".into(),
            name: "messageCodingAgent".into(),
            description: "Send a message to the active coding agent.".into(),
            expects_response: true,
            response_timeout_secs: 120,
            parameters: serde_json::json!({
                "type": "object",
                "required": ["message"],
                "properties": {
                    "message": {
                        "type": "string",
                        "description": "The message to send to the coding agent."
                    }
                }
            }),
        },
        VoiceTool {
            tool_type: "client".into(),
            name: "processPermissionRequest".into(),
            description: "Process a permission request from the coding agent.".into(),
            expects_response: true,
            response_timeout_secs: 30,
            parameters: serde_json::json!({
                "type": "object",
                "required": ["decision"],
                "properties": {
                    "decision": {
                        "type": "string",
                        "description": "The user's decision: must be either 'allow' or 'deny'"
                    }
                }
            }),
        },
    ]
}

pub fn build_voice_agent_config() -> VoiceAgentConfig {
    VoiceAgentConfig {
        name: VOICE_AGENT_NAME.into(),
        conversation_config: ConversationConfig {
            agent: AgentConfig {
                first_message: VOICE_FIRST_MESSAGE.into(),
                language: "en".into(),
                prompt: PromptConfig {
                    prompt: VOICE_SYSTEM_PROMPT.into(),
                    llm: "gemini-2.5-flash".into(),
                    temperature: 0.7,
                    max_tokens: 1024,
                    tools: voice_tools(),
                },
            },
            turn: TurnConfig {
                turn_timeout: 30.0,
                silence_end_call_timeout: 600.0,
            },
            tts: TtsConfig {
                voice_id: "cgSgspJ2msm6clMCkdW9".into(),
                model_id: "eleven_flash_v2".into(),
                speed: 1.1,
            },
        },
        platform_settings: Some(PlatformSettings {
            overrides: Some(PlatformOverrides {
                conversation_config_override: Some(ConversationConfigOverride {
                    agent: Some(AgentOverride {
                        language: Some(true),
                        first_message: None,
                    }),
                }),
            }),
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn voice_config_serializes() {
        let config = build_voice_agent_config();
        let json = serde_json::to_string(&config).unwrap();
        assert!(json.contains("Hapi Voice Assistant"));
        assert!(json.contains("gemini-2.5-flash"));
    }
}
