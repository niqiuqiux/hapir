export const ELEVENLABS_API_BASE = 'https://api.elevenlabs.io/v1'
export const VOICE_AGENT_NAME = 'Hapi Voice Assistant'
export const VOICE_FIRST_MESSAGE = 'Hey! Hapi here.'

export const VOICE_SYSTEM_PROMPT = `# Identity

You are Hapi Voice Assistant. You bridge voice communication between users and their AI coding agents in the Hapi ecosystem.

You are friendly, proactive, and highly intelligent with a world-class engineering background. Your approach is warm, witty, and relaxed, balancing professionalism with an approachable vibe.`

export interface VoiceTool {
    type: string
    name: string
    description: string
    expects_response: boolean
    response_timeout_secs: number
    parameters: Record<string, unknown>
}

export interface VoiceAgentConfig {
    name: string
    conversation_config: {
        agent: {
            first_message: string
            language: string
            prompt: {
                prompt: string
                llm: string
                temperature: number
                max_tokens: number
                tools: VoiceTool[]
            }
        }
        turn: {
            turn_timeout: number
            silence_end_call_timeout: number
        }
        tts: {
            voice_id: string
            model_id: string
            speed: number
        }
    }
    platform_settings?: {
        overrides?: {
            conversation_config_override?: {
                agent?: {
                    language?: boolean
                    first_message?: boolean
                }
            }
        }
    }
}

function voiceTools(): VoiceTool[] {
    return [
        {
            type: 'client',
            name: 'messageCodingAgent',
            description: 'Send a message to the active coding agent.',
            expects_response: true,
            response_timeout_secs: 120,
            parameters: {
                type: 'object',
                required: ['message'],
                properties: {
                    message: {
                        type: 'string',
                        description: 'The message to send to the coding agent.',
                    },
                },
            },
        },
        {
            type: 'client',
            name: 'processPermissionRequest',
            description: 'Process a permission request from the coding agent.',
            expects_response: true,
            response_timeout_secs: 30,
            parameters: {
                type: 'object',
                required: ['decision'],
                properties: {
                    decision: {
                        type: 'string',
                        description: "The user's decision: must be either 'allow' or 'deny'",
                    },
                },
            },
        },
    ]
}

export function buildVoiceAgentConfig(): VoiceAgentConfig {
    return {
        name: VOICE_AGENT_NAME,
        conversation_config: {
            agent: {
                first_message: VOICE_FIRST_MESSAGE,
                language: 'en',
                prompt: {
                    prompt: VOICE_SYSTEM_PROMPT,
                    llm: 'gemini-2.5-flash',
                    temperature: 0.7,
                    max_tokens: 1024,
                    tools: voiceTools(),
                },
            },
            turn: {
                turn_timeout: 30,
                silence_end_call_timeout: 600,
            },
            tts: {
                voice_id: 'cgSgspJ2msm6clMCkdW9',
                model_id: 'eleven_flash_v2',
                speed: 1.1,
            },
        },
        platform_settings: {
            overrides: {
                conversation_config_override: {
                    agent: {
                        language: true,
                    },
                },
            },
        },
    }
}
