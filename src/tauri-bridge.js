// Core Tauri bridge for frontend modules.

import { SILLYTAVERN_COMPAT_VERSION } from './compat-version.js';

function detectTauriEnv() {
    if (typeof window === 'undefined') {
        return false;
    }

    // __TAURI_RUNNING__ is set in init.js before dynamic imports to avoid
    // startup races where mobile bridge internals are injected a bit later.
    return window.__TAURI_RUNNING__ === true
        || window.__TAURI_INTERNALS__ !== undefined
        || typeof window.__TAURI__?.core?.invoke === 'function';
}

export const isTauriEnv = detectTauriEnv();

function getTauri() {
    if (typeof window === 'undefined') {
        return null;
    }

    return window.__TAURI__ || null;
}

function isWebHost() {
    return typeof window !== 'undefined' && window.__TAURITAVERN_WEB__ === true;
}

async function invokeWeb(command, args, options) {
    if (command === 'get_client_version') {
        return {
            agent: 'SillyTavern:1.18.0:TauriTavern',
            pkgVersion: '1.18.0',
            tauriVersion: '2.3.0-web',
            gitRevision: null,
            gitBranch: null,
            defaultUpdateChannel: 'stable',
        };
    }
    if (command === 'get_notification_permission_state') {
        return typeof Notification === 'undefined'
            ? 'denied'
            : ({ granted: 'granted', denied: 'denied', default: 'prompt' }[Notification.permission] || 'denied');
    }
    if (command === 'request_notification_permission') {
        if (typeof Notification === 'undefined' || !window.isSecureContext) return 'denied';
        const permission = Notification.permission === 'default'
            ? await Notification.requestPermission()
            : Notification.permission;
        return ({ granted: 'granted', denied: 'denied', default: 'prompt' }[permission] || 'denied');
    }
    if (command === 'show_system_notification') {
        const { title = '', body = '' } = args?.dto || {};
        if (!String(title).trim() && !String(body).trim()) {
            throw new Error('Notification title and body cannot both be empty');
        }
        if (typeof Notification === 'undefined' || !window.isSecureContext || Notification.permission !== 'granted') {
            throw new Error('Browser notification permission is unavailable or not granted');
        }
        new Notification(String(title), { body: String(body) });
        return null;
    }
    const channels = [];
    let webArgs;
    let response;
    const binaryBody = args instanceof Uint8Array || args instanceof ArrayBuffer;
    try {
        webArgs = binaryBody ? args : await serializeWebChannels(args ?? {}, channels);
        const endpoint = binaryBody ? new URL('/__tt/invoke-binary', window.location.href) : new URL('/__tt/invoke', window.location.href);
        if (binaryBody) endpoint.searchParams.set('command', command);
        response = await fetch(endpoint, {
            method: 'POST',
            credentials: 'same-origin',
            headers: binaryBody
                ? { ...(options?.headers || {}) }
                : { 'Content-Type': 'application/json' },
            body: binaryBody ? webArgs : JSON.stringify({ command, args: webArgs }),
        });
    } catch (error) {
        channels.forEach(({ socket }) => socket.close(1000, 'invoke failed'));
        throw error;
    }
    let body;
    try {
        body = await response.json();
    } catch {
        body = null;
    }
    if (!response.ok && response.status === 403 && body?.requires_confirmation === true && typeof body.endpoint === 'string') {
        const endpoint = body.endpoint;
        const transportWarning = endpoint.startsWith('http://')
            ? '\n此 HTTP 连接未加密，传输内容可能被观察或修改。'
            : '';
        const approved = window.confirm(
            `允许连接到自定义端点？\n\n端点：${endpoint}\n\nTauriTavern 可能向它发送 API 密钥、提示词和聊天内容。只在你确认这是自己配置的服务时授权。${transportWarning}`,
        );
        if (approved) {
            const grantResponse = await fetch('/__tt/invoke', {
                method: 'POST',
                credentials: 'same-origin',
                headers: { 'Content-Type': 'application/json' },
                body: JSON.stringify({ command: 'grant_user_endpoint_access', args: { endpoint } }),
            });
            const grantBody = await grantResponse.json().catch(() => null);
            if (grantResponse.ok) {
                channels.forEach(({ socket }) => socket.close(1000, 'retry after endpoint approval'));
                return invokeWeb(command, args, options);
            }
            channels.forEach(({ socket }) => socket.close(1000, 'endpoint approval failed'));
            throw new Error(grantBody?.error || '无法授权此端点');
        }
    }
    if (!response.ok) {
        channels.forEach(({ socket }) => socket.close(1000, 'invoke failed'));
        const detail = body?.error || body?.message || `HTTP ${response.status}`;
        throw new Error(`${command}: ${detail}`);
    }
    if (!body || !Object.prototype.hasOwnProperty.call(body, 'result')) {
        channels.forEach(({ socket }) => socket.close(1000, 'invalid invoke response'));
        throw new Error(`${command}: malformed web host response`);
    }
    if (command === 'read_chat_bytes' && Array.isArray(body.result)) {
        return Uint8Array.from(body.result);
    }
    return body.result;
}

async function serializeWebChannels(value, channels, seen = new WeakSet()) {
    if (value instanceof WebInvokeChannel) {
        const socket = new WebSocket(getWebSocketUrl('/__tt/channels', { id: value.id }));
        const channel = { socket, value };
        channels.push(channel);
        socket.addEventListener('message', (event) => {
            let envelope;
            try {
                envelope = JSON.parse(event.data);
            } catch {
                console.error('Invalid web channel message');
                return;
            }
            if (envelope?.type === 'message') value.onmessage?.(envelope.payload);
            if (envelope?.type === 'close') socket.close(1000, 'channel complete');
        });
        await new Promise((resolve, reject) => {
            const timeout = setTimeout(() => reject(new Error('Web channel connection timed out')), 10_000);
            socket.addEventListener('open', () => {
                clearTimeout(timeout);
                resolve();
            }, { once: true });
            socket.addEventListener('error', () => {
                clearTimeout(timeout);
                reject(new Error('Unable to connect web channel'));
            }, { once: true });
        });
        return { __ttWebChannel: value.id };
    }
    if (Array.isArray(value)) {
        if (seen.has(value)) throw new Error('Cyclic web invoke arguments are not supported');
        seen.add(value);
        const result = await Promise.all(value.map((item) => serializeWebChannels(item, channels, seen)));
        seen.delete(value);
        return result;
    }
    if (isPlainObject(value)) {
        if (seen.has(value)) throw new Error('Cyclic web invoke arguments are not supported');
        seen.add(value);
        const entries = await Promise.all(Object.entries(value).map(async ([key, item]) => [
            key,
            await serializeWebChannels(item, channels, seen),
        ]));
        seen.delete(value);
        return Object.fromEntries(entries);
    }
    return value;
}

class WebInvokeChannel {
    constructor(onmessage) {
        this.id = globalThis.crypto?.randomUUID?.() || `${Date.now()}-${Math.random().toString(36).slice(2)}`;
        this.onmessage = onmessage;
    }
}

function getWebSocketUrl(pathname, params = {}) {
    const url = new URL(pathname, window.location.href);
    url.protocol = url.protocol === 'https:' ? 'wss:' : 'ws:';
    for (const [key, value] of Object.entries(params)) {
        url.searchParams.set(key, String(value));
    }
    return url;
}

function getInvokeFn() {
    const fn = getTauri()?.core?.invoke;
    if (typeof fn === 'function') return fn;
    return isWebHost() ? invokeWeb : null;
}

function isPlainObject(value) {
    if (Object.prototype.toString.call(value) !== '[object Object]') {
        return false;
    }
    const prototype = Object.getPrototypeOf(value);
    return prototype === null || prototype === Object.prototype;
}

function withTauriArgumentAliases(args) {
    if (!isPlainObject(args)) {
        return args;
    }

    const aliased = { ...args };
    for (const [key, value] of Object.entries(args)) {
        if (!key.includes('_')) {
            continue;
        }

        const camelCaseKey = key.replace(/_+([a-zA-Z0-9])/g, (_, char) => char.toUpperCase());
        if (!Object.prototype.hasOwnProperty.call(aliased, camelCaseKey)) {
            aliased[camelCaseKey] = value;
        }
    }

    return aliased;
}

export const invoke = (...args) => {
    const fn = getInvokeFn();
    if (typeof fn !== 'function') {
        throw new Error('Tauri invoke is unavailable');
    }

    if (args.length === 2 && isPlainObject(args[1])) {
        return fn(args[0], withTauriArgumentAliases(args[1]));
    }

    return fn(...args);
};

function getHostSafeInvoke() {
    if (typeof window === 'undefined') {
        return null;
    }

    const fn = window.__TAURITAVERN__?.invoke?.safeInvoke;
    return typeof fn === 'function' ? fn : null;
}

async function invokeWithHostNormalization(command, args) {
    const safeInvoke = getHostSafeInvoke();
    if (safeInvoke) {
        return safeInvoke(command, args);
    }

    return args === undefined ? invoke(command) : invoke(command, args);
}

export const listen = (...args) => {
    const fn = getTauri()?.event?.listen;
    if (typeof fn === 'function') return fn(...args);
    if (!isWebHost()) throw new Error('Tauri listen is unavailable');

    const [eventName, handler] = args;
    if (typeof eventName !== 'string' || typeof handler !== 'function') {
        throw new Error('Invalid web event subscription');
    }

    return new Promise((resolve, reject) => {
        const socket = new WebSocket(getWebSocketUrl('/__tt/events', { event: eventName }));
        let opened = false;
        socket.addEventListener('open', () => {
            opened = true;
            resolve(() => socket.close(1000, 'unlisten'));
        }, { once: true });
        socket.addEventListener('message', (message) => {
            try {
                handler(JSON.parse(message.data));
            } catch (error) {
                console.error(`Invalid event payload for ${eventName}`, error);
            }
        });
        socket.addEventListener('error', () => {
            if (!opened) reject(new Error(`Unable to subscribe to ${eventName}`));
        }, { once: true });
        socket.addEventListener('close', () => {
            if (!opened) reject(new Error(`Event subscription closed before ready: ${eventName}`));
        }, { once: true });
    });
};

export function createChannel(onmessage) {
    const Channel = getTauri()?.core?.Channel;
    if (typeof Channel === 'function') return new Channel(onmessage);
    if (isWebHost()) return new WebInvokeChannel(onmessage);
    throw new Error('Tauri Channel is unavailable');
}

export const convertFileSrc = (path, protocol = 'asset') => {
    const fn = getTauri()?.core?.convertFileSrc;
    if (typeof fn === 'function') return fn(path, protocol);
    if (isWebHost()) {
        const url = new URL('/__tt/resource', window.location.href);
        url.searchParams.set('path', String(path));
        url.searchParams.set('protocol', protocol);
        return url.pathname + url.search;
    }
    throw new Error('Tauri convertFileSrc is unavailable');
};

export function isTauri() {
    return detectTauriEnv();
}

export function isWebRuntime() {
    return isWebHost();
}

export async function initializeBridge() {
    const invokeFn = getInvokeFn();
    if (!invokeFn) {
        return false;
    }

    try {
        if (isWebHost()) {
            const response = await fetch('/__tt/ready', { credentials: 'same-origin', cache: 'no-store' });
            return response.ok;
        }
        return await invokeFn('is_ready');
    } catch (error) {
        console.error('Failed to initialize Tauri bridge:', error);
        return false;
    }
}

export async function getCsrfToken() {
    return 'tauri-dummy-token';
}

export async function initializeApp() {
    return initializeBridge();
}

export async function getVersion() {
    const invokeFn = getInvokeFn();
    if (!invokeFn) {
        const response = await fetch('/version');
        return response.json();
    }

    return invokeFn('get_version');
}

export async function getClientVersion() {
    const invokeFn = getInvokeFn();
    if (!invokeFn) {
        const response = await fetch('/version');
        return response.json();
    }

    try {
        return await invokeFn('get_client_version');
    } catch (error) {
        console.error('Error getting client version from Tauri backend:', error);
        const version = await invokeFn('get_version');
        return {
            agent: `SillyTavern:${SILLYTAVERN_COMPAT_VERSION}:TauriTavern`,
            pkgVersion: SILLYTAVERN_COMPAT_VERSION,
            tauriVersion: version,
            gitRevision: null,
            gitBranch: null,
            defaultUpdateChannel: 'stable',
        };
    }
}

export async function checkForUpdate(channel) {
    return invokeWithHostNormalization('check_for_update', { channel });
}

export async function getTauriTavernSettings() {
    const invokeFn = getInvokeFn();
    if (!invokeFn) {
        throw new Error('Tauri invoke is unavailable');
    }

    return invokeFn('get_tauritavern_settings');
}

export async function getChatBackupStorageStats() {
    const invokeFn = getInvokeFn();
    if (!invokeFn) {
        throw new Error('Tauri invoke is unavailable');
    }

    return invokeFn('get_chat_backup_storage_stats');
}

export async function updateTauriTavernSettings(dto) {
    if (!isPlainObject(dto)) {
        throw new Error('Invalid TauriTavern settings DTO');
    }

    return invokeWithHostNormalization('update_tauritavern_settings', { dto });
}

export async function getRuntimePaths() {
    const invokeFn = getInvokeFn();
    if (!invokeFn) {
        throw new Error('Tauri invoke is unavailable');
    }

    return invokeFn('get_runtime_paths');
}

export async function setDataRoot(dataRoot) {
    if (typeof dataRoot !== 'string' || dataRoot.trim() === '') {
        throw new Error('Invalid data root path');
    }
    return invokeWithHostNormalization('set_data_root', { data_root: dataRoot });
}

export async function openDialog(options = {}) {
    const invokeFn = getInvokeFn();
    if (!invokeFn) {
        throw new Error('Tauri invoke is unavailable');
    }

    if (!isPlainObject(options)) {
        throw new Error('Invalid dialog options: expected an object');
    }

    Object.freeze(options);

    return invokeWithHostNormalization('plugin:dialog|open', { options });
}

export function writeClipboardText(text) {
    return invoke('plugin:clipboard-manager|write_text', { text });
}

function normalizeExternalUrl(url) {
    const value = String(url instanceof URL ? url.href : url ?? '').trim();
    if (!value) {
        throw new Error('External URL is required');
    }

    try {
        return new URL(value, window.location.href).toString();
    } catch {
        throw new Error(`Invalid external URL: ${value}`);
    }
}

export async function openExternalUrl(url, openWith) {
    const href = normalizeExternalUrl(url);
    const invokeFn = getInvokeFn();

    if (invokeFn) {
        return invokeFn('plugin:opener|open_url', {
            url: href,
            with: openWith,
        });
    }

    const openedWindow = typeof window.open === 'function'
        ? window.open(href, '_blank', 'noopener,noreferrer')
        : null;

    if (openedWindow) {
        return;
    }

    if (typeof window.location?.assign === 'function') {
        window.location.assign(href);
        return;
    }

    throw new Error('Unable to open external URL');
}

export function getAssetUrl(path) {
    if (!isTauriEnv || !convertFileSrc || !path) {
        return path;
    }

    try {
        return convertFileSrc(path, 'asset');
    } catch {
        return path;
    }
}
