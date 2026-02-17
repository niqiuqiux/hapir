import { isObject } from './utils'

export interface RoleWrappedRecord {
    role: string
    content: unknown
    meta?: unknown
}

function extractRoleWrapped(value: unknown): RoleWrappedRecord | null {
    if (!isObject(value)) return null
    if (typeof value.role !== 'string' || !('content' in value)) return null
    return {
        role: value.role,
        content: value.content,
        meta: value.meta,
    }
}

/**
 * Unwrap a role-wrapped record from various envelope formats.
 * Checks: direct, .message, .data.message, .payload.message
 */
export function unwrapRoleWrappedRecordEnvelope(value: unknown): RoleWrappedRecord | null {
    // Direct
    const direct = extractRoleWrapped(value)
    if (direct) return direct

    if (!isObject(value)) return null

    // .message
    if (isObject(value.message)) {
        const r = extractRoleWrapped(value.message)
        if (r) return r
    }

    // .data.message
    if (isObject(value.data) && isObject((value.data as Record<string, unknown>).message)) {
        const r = extractRoleWrapped((value.data as Record<string, unknown>).message)
        if (r) return r
    }

    // .payload.message
    if (isObject(value.payload) && isObject((value.payload as Record<string, unknown>).message)) {
        const r = extractRoleWrapped((value.payload as Record<string, unknown>).message)
        if (r) return r
    }

    return null
}
