import { getActiveChatSnapshot } from '../../adapters/st/active-chat-ref.js';

export function getWebChatKey() {
    try { return JSON.stringify(getActiveChatSnapshot().ref); }
    catch { return null; }
}

export async function getWebGenerationContext() {
    if (typeof window === 'undefined' || window.__TAURITAVERN_WEB__ !== true) return null;
    try {
        const chatKey = getWebChatKey();
        if (!chatKey) return null;
        const chat = window.SillyTavern?.getContext?.()?.chat;
        const raw = JSON.stringify(chat);
        if (!raw) return null;
        const bytes = new TextEncoder().encode(raw);
        if (crypto?.subtle) {
            const digest = await crypto.subtle.digest('SHA-256', bytes);
            const revision = Array.from(new Uint8Array(digest), (byte) => byte.toString(16).padStart(2, '0')).join('');
            return { chatKey, revision };
        }
        let hash = 2166136261;
        for (const byte of bytes) hash = Math.imul(hash ^ byte, 16777619);
        return { chatKey, revision: `${bytes.length}-${(hash >>> 0).toString(16)}` };
    } catch {
        return null;
    }
}
