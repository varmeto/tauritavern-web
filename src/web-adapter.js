const build = window.__TT_WEB_BUILD__ || { buildId: 'development' };
const databaseName = 'tt-web-resume-v1';
let status = 'recovering';
let pendingRecovery = null;
let recoveryTimer = null;
let activeChatKey = null;
let saveTimer = null;
let databasePromise = null;

function openDatabase() {
    if (!databasePromise) {
        databasePromise = new Promise((resolve, reject) => {
            const request = indexedDB.open(databaseName, 1);
            request.onupgradeneeded = () => request.result.createObjectStore('chat');
            request.onsuccess = () => resolve(request.result);
            request.onerror = () => reject(request.error);
        });
    }
    return databasePromise;
}

async function readRecord(key) {
    const database = await openDatabase();
    return new Promise((resolve, reject) => {
        const request = database.transaction('chat').objectStore('chat').get(key);
        request.onsuccess = () => resolve(request.result || null);
        request.onerror = () => reject(request.error);
    });
}

async function writeRecord(key, record) {
    const database = await openDatabase();
    return new Promise((resolve, reject) => {
        const transaction = database.transaction('chat', 'readwrite');
        transaction.objectStore('chat').put(record, key);
        transaction.oncomplete = resolve;
        transaction.onerror = () => reject(transaction.error);
    });
}

function currentChatKey() {
    try {
        const context = window.SillyTavern?.getContext?.();
        if (!context?.chatId) return null;
        const character = context.characters?.[context.characterId];
        const identity = context.groupId ? `group:${context.groupId}` : `character:${character?.avatar || character?.name}`;
        if (!identity || identity.endsWith(':undefined')) return null;
        return `${identity}:${context.chatId}`;
    } catch {
        return null;
    }
}

function deviceId() {
    const key = 'tt-web-device-id';
    let id = localStorage.getItem(key);
    if (!id) {
        id = createUuid();
        localStorage.setItem(key, id);
    }
    return id;
}

function createUuid() {
    if (typeof crypto?.randomUUID === 'function') return crypto.randomUUID();
    const bytes = new Uint8Array(16);
    if (typeof crypto?.getRandomValues === 'function') crypto.getRandomValues(bytes);
    else bytes.forEach((_, index) => { bytes[index] = Math.random() * 256; });
    bytes[6] = (bytes[6] & 0x0f) | 0x40;
    bytes[8] = (bytes[8] & 0x3f) | 0x80;
    const hex = Array.from(bytes, (value) => value.toString(16).padStart(2, '0')).join('');
    return `${hex.slice(0, 8)}-${hex.slice(8, 12)}-${hex.slice(12, 16)}-${hex.slice(16, 20)}-${hex.slice(20)}`;
}

function recordKey(chatKey) {
    return `${deviceId()}:${chatKey}`;
}

function scrollAnchor(chat) {
    if (!chat) return null;
    const chatTop = chat.getBoundingClientRect().top;
    const firstVisible = Array.from(chat.querySelectorAll(':scope > .mes[mesid]'))
        .find((message) => message.getBoundingClientRect().bottom > chatTop);
    return firstVisible ? {
        messageId: firstVisible.getAttribute('mesid'),
        offset: firstVisible.getBoundingClientRect().top - chatTop,
    } : null;
}

async function saveCurrentChat(chatKey = activeChatKey, snapshot = null) {
    if (!chatKey) return;
    const textarea = document.querySelector('#send_textarea');
    const chat = document.querySelector('#chat');
    const previous = await readRecord(recordKey(chatKey));
    await writeRecord(recordKey(chatKey), {
        ...(previous || {}),
        draft: snapshot?.draft ?? textarea?.value ?? '',
        scrollTop: snapshot?.scrollTop ?? chat?.scrollTop ?? 0,
        anchor: snapshot?.anchor ?? scrollAnchor(chat),
        buildId: build.buildId,
        updatedAt: Date.now(),
    });
}

function scheduleSave() {
    const chatKey = activeChatKey;
    const snapshot = {
        draft: document.querySelector('#send_textarea')?.value || '',
        scrollTop: document.querySelector('#chat')?.scrollTop || 0,
        anchor: scrollAnchor(document.querySelector('#chat')),
    };
    clearTimeout(saveTimer);
    saveTimer = setTimeout(() => void saveCurrentChat(chatKey, snapshot).catch(console.warn), 250);
}

async function switchChat() {
    const next = currentChatKey();
    if (!next || next === activeChatKey) return;
    activeChatKey = next;
    let previous = await readRecord(recordKey(next));
    try {
        const emergency = JSON.parse(sessionStorage.getItem(`tt-web-draft:${recordKey(next)}`) || 'null');
        if (emergency?.updatedAt > (previous?.updatedAt || 0)) previous = { ...previous, ...emergency };
    } catch { /* IndexedDB remains the primary recovery record. */ }
    const textarea = document.querySelector('#send_textarea');
    if (textarea && !textarea.value && previous?.draft) {
        textarea.value = previous.draft;
        textarea.dispatchEvent(new Event('input', { bubbles: true }));
    }
    const chat = document.querySelector('#chat');
    if (chat && Number.isFinite(previous?.scrollTop)) {
        requestAnimationFrame(() => {
            const anchor = previous?.anchor;
            const message = anchor?.messageId && /^\d+$/.test(anchor.messageId)
                ? chat.querySelector(`:scope > .mes[mesid="${anchor.messageId}"]`) : null;
            if (message) {
                chat.scrollTop += message.getBoundingClientRect().top - chat.getBoundingClientRect().top - anchor.offset;
            } else {
                chat.scrollTop = previous.scrollTop;
            }
        });
    }
}

function showStatus(next, message = '') {
    status = next;
    let banner = document.querySelector('#tt-web-connection-banner');
    if (next === 'online') {
        banner?.remove();
        return;
    }
    if (!banner) {
        banner = document.createElement('div');
        banner.id = 'tt-web-connection-banner';
        banner.setAttribute('role', 'status');
        const button = document.createElement('button');
        button.type = 'button';
        button.textContent = next === 'login-required' ? '重新登录' : '重试';
        button.addEventListener('click', () => {
            if (status === 'login-required') {
                void saveCurrentChat().finally(() => { location.href = '/login'; });
            } else {
                void recover(true);
            }
        });
        banner.append(document.createElement('span'), button);
        document.body.append(banner);
    }
    banner.querySelector('button').textContent = next === 'login-required' ? '重新登录' : '重试';
    banner.querySelector('span').textContent = message || '正在恢复连接…';
}

function showBuildChange(nextBuild) {
    if (!nextBuild || nextBuild === build.buildId || document.querySelector('#tt-web-update-banner')) return;
    const banner = document.createElement('div');
    banner.id = 'tt-web-update-banner';
    banner.textContent = '酒馆已更新。保存当前操作后刷新页面以切换版本。';
    const button = document.createElement('button');
    button.type = 'button';
    button.textContent = '刷新';
    button.addEventListener('click', () => {
        if (document.body.dataset.generating === 'true') {
            banner.firstChild.textContent = '生成中，请等生成完成后刷新。';
            return;
        }
        void saveCurrentChat().finally(() => location.reload());
    });
    banner.append(button);
    document.body.append(banner);
}

async function checkStatus() {
    const response = await fetch('/auth/status', { cache: 'no-store', credentials: 'same-origin', signal: AbortSignal.timeout(3000) });
    if (response.status === 401) {
        showStatus('login-required', '登录已过期，草稿已保留。');
        return false;
    }
    if (!response.ok) throw new Error(`运行状态检查失败：${response.status}`);
    const result = await response.json();
    showBuildChange(result.build?.buildId);
    if (!result.appReady) throw new Error('应用服务正在启动');
    return true;
}

async function recover(force = false) {
    if (pendingRecovery) return pendingRecovery;
    clearTimeout(recoveryTimer);
    pendingRecovery = (async () => {
        showStatus('recovering');
        for (let attempt = 0; attempt < 4; attempt += 1) {
            try {
                if (!await checkStatus()) return;
                window.__TT_WEB_EVENTS__?.reconnect?.();
                await switchChat();
                window.dispatchEvent(new CustomEvent('tt-web-recovered'));
                showStatus('online');
                return;
            } catch (error) {
                if (attempt === 3) {
                    showStatus(navigator.onLine ? 'offline' : 'offline', String(error.message || error));
                    return;
                }
                await new Promise((resolve) => setTimeout(resolve, Math.min(4000, 400 * 2 ** attempt)));
            }
        }
    })().finally(() => { pendingRecovery = null; });
    return pendingRecovery;
}

function installViewportFix() {
    const update = () => {
        const viewport = window.visualViewport;
        if (!viewport) return;
        document.documentElement.style.setProperty('--tt-web-viewport-height', `${viewport.height}px`);
        document.documentElement.style.setProperty('--tt-web-keyboard-height', `${Math.max(0, innerHeight - viewport.height - viewport.offsetTop)}px`);
    };
    window.visualViewport?.addEventListener('resize', update);
    window.visualViewport?.addEventListener('scroll', update);
    window.addEventListener('orientationchange', update);
    update();
}

async function installStaticWorker() {
    if (!window.isSecureContext || !('serviceWorker' in navigator) || build.buildId === 'development') return;
    await navigator.serviceWorker.register(`/tt-ext-sw.js?web-build=${encodeURIComponent(build.buildId)}`, { scope: '/' });
}

export async function startWebAdapter() {
    document.documentElement.classList.add('tt-web-host');
    installViewportFix();
    document.addEventListener('input', (event) => {
        if (event.target?.id === 'send_textarea') scheduleSave();
    });
    document.addEventListener('scroll', (event) => {
        if (event.target?.id === 'chat') scheduleSave();
    }, true);
    document.addEventListener('visibilitychange', () => {
        if (document.visibilityState === 'visible') void recover();
        else void saveCurrentChat().catch(console.warn);
    });
    window.addEventListener('pageshow', (event) => { if (event.persisted) void recover(); });
    window.addEventListener('online', () => void recover());
    window.addEventListener('offline', () => showStatus('offline', '网络已断开，恢复后会自动重连。'));
    window.addEventListener('pagehide', () => {
        window.__TT_WEB_UNLOADING__ = true;
        if (activeChatKey) {
            try {
                sessionStorage.setItem(`tt-web-draft:${recordKey(activeChatKey)}`, JSON.stringify({
                    draft: document.querySelector('#send_textarea')?.value || '',
                    updatedAt: Date.now(),
                }));
            } catch { /* Storage can be unavailable in private browsing. */ }
        }
        void saveCurrentChat().catch(console.warn);
    });
    await recover();
    void installStaticWorker().catch((error) => console.warn('Static cache unavailable:', error));
}

export function webAdapterAppReady() {
    let metricAttempts = 0;
    const captureMetrics = () => {
        const marks = Object.fromEntries(performance.getEntriesByType('mark')
            .filter((entry) => entry.name.startsWith('tt:init:') || entry.name.startsWith('tt:startup:'))
            .map((entry) => [entry.name, Math.round(entry.startTime)]));
        if (!marks['tt:startup:chat-restored'] && metricAttempts++ < 20) {
            setTimeout(captureMetrics, 500);
            return;
        }
        const resources = performance.getEntriesByType('resource');
        const metric = {
            buildId: build.buildId,
            at: new Date().toISOString(),
            navigation: performance.getEntriesByType('navigation')[0]?.type || 'navigate',
            transferBytes: resources.reduce((total, entry) => total + (entry.transferSize || 0), 0),
            resources: resources.length,
            marks,
        };
        try {
            const history = JSON.parse(localStorage.getItem('tt-web-startup-metrics') || '[]');
            history.push(metric);
            localStorage.setItem('tt-web-startup-metrics', JSON.stringify(history.slice(-40)));
        } catch (error) {
            console.warn('Startup measurements could not be saved:', error);
        }
        window.__TT_WEB_METRICS__ = metric;
        console.info('TauriTavern web startup', metric);
    };
    setTimeout(captureMetrics, 1500);
    let attempts = 0;
    const attach = () => {
        const context = window.SillyTavern?.getContext?.();
        if (context?.eventSource?.on && currentChatKey()) {
            context.eventSource.on(context.eventTypes?.CHAT_CHANGED || 'chat_id_changed', () => void switchChat().catch(console.warn));
            void switchChat().catch(console.warn);
            void import('./web-generation-recovery.js')
                .then((module) => module.startWebGenerationRecovery())
                .catch((error) => console.error('Generation recovery unavailable:', error));
            return;
        }
        if (attempts++ < 60) setTimeout(attach, 500);
    };
    attach();
}

export function getWebConnectionState() {
    return status;
}
