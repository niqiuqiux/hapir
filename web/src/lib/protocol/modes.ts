import type { AgentFlavor, ModelMode, PermissionMode, PermissionModeTone } from '@/types/generated'

// --- Permission Mode metadata ---

const PERMISSION_MODE_LABELS: Record<PermissionMode, string> = {
    'default': 'Default',
    'acceptEdits': 'Accept Edits',
    'plan': 'Plan Mode',
    'bypassPermissions': 'Yolo',
    'read-only': 'Read Only',
    'safe-yolo': 'Safe Yolo',
    'yolo': 'Yolo',
}

const PERMISSION_MODE_TONES: Record<PermissionMode, PermissionModeTone> = {
    'default': 'neutral',
    'acceptEdits': 'warning',
    'plan': 'info',
    'bypassPermissions': 'danger',
    'read-only': 'warning',
    'safe-yolo': 'warning',
    'yolo': 'danger',
}

// --- Per-flavor permission modes ---

const CLAUDE_PERMISSION_MODES: PermissionMode[] = ['default', 'acceptEdits', 'bypassPermissions', 'plan']
const CODEX_PERMISSION_MODES: PermissionMode[] = ['default', 'read-only', 'safe-yolo', 'yolo']
const GEMINI_PERMISSION_MODES: PermissionMode[] = ['default', 'read-only', 'safe-yolo', 'yolo']
const OPENCODE_PERMISSION_MODES: PermissionMode[] = ['default', 'yolo']

// --- Model modes ---

export const MODEL_MODES: readonly ModelMode[] = ['default', 'sonnet', 'opus'] as const

export const MODEL_MODE_LABELS: Record<ModelMode, string> = {
    'default': 'Default',
    'sonnet': 'Sonnet',
    'opus': 'Opus',
}

// --- Functions ---

function parseFlavorString(flavor: string | null | undefined): AgentFlavor | null {
    if (flavor === 'claude' || flavor === 'codex' || flavor === 'gemini' || flavor === 'opencode') {
        return flavor
    }
    return null
}

export function getPermissionModesForFlavor(flavor?: string | null): PermissionMode[] {
    const parsed = parseFlavorString(flavor)
    switch (parsed) {
        case 'codex': return CODEX_PERMISSION_MODES
        case 'gemini': return GEMINI_PERMISSION_MODES
        case 'opencode': return OPENCODE_PERMISSION_MODES
        default: return CLAUDE_PERMISSION_MODES
    }
}

export interface PermissionModeOption {
    mode: PermissionMode
    label: string
    tone: PermissionModeTone
}

export function getPermissionModeOptionsForFlavor(flavor?: string | null): PermissionModeOption[] {
    return getPermissionModesForFlavor(flavor).map((mode) => ({
        mode,
        label: PERMISSION_MODE_LABELS[mode],
        tone: PERMISSION_MODE_TONES[mode],
    }))
}

export function isPermissionModeAllowedForFlavor(mode: PermissionMode, flavor?: string | null): boolean {
    return getPermissionModesForFlavor(flavor).includes(mode)
}

export function getPermissionModeLabel(mode: PermissionMode): string {
    return PERMISSION_MODE_LABELS[mode]
}

export function getPermissionModeTone(mode: PermissionMode): PermissionModeTone {
    return PERMISSION_MODE_TONES[mode]
}

export type { PermissionModeTone } from '@/types/generated'
