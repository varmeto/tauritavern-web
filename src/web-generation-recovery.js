import { invoke } from './tauri-bridge.js';
import { getWebChatKey, getWebGenerationContext } from './tauri/main/services/ai/web-generation-context.js';
import { getStreamingReply } from './scripts/openai.js';
import { saveReply, saveChatConditional } from './script.js';

let activeTask = null;
let checking = false;
let timer = null;
let lastChatKey = null;
const processingTasks = new Set();

function banner(message, action = null) {
    let element = document.querySelector('#tt-web-generation-banner');
    if (!message) { element?.remove(); return; }
    if (!element) {
        element = document.createElement('div');
        element.id = 'tt-web-generation-banner';
        element.setAttribute('role', 'status');
        document.body.append(element);
    }
    const actionKey = action?.key || action?.label || '';
    if (element.dataset.message === message && element.dataset.action === actionKey) return;
    element.dataset.message = message;
    element.dataset.action = actionKey;
    element.replaceChildren(document.createTextNode(message));
    if (action) {
        const button = document.createElement('button');
        button.type = 'button';
        button.textContent = action.label;
        button.addEventListener('click', action.run);
        element.append(button);
    }
}

async function readAll(taskId) {
    const events = [];
    let afterSeq = 0;
    for (;;) {
        const result = await invoke('read_web_generation', { taskId, afterSeq });
        const batch = result.events || [];
        events.push(...batch);
        if (batch.length < 64) return { events, task: result.task };
        afterSeq = batch.at(-1).seq;
    }
}

function parseResult(events, task) {
    const state = { reasoning: '', images: [], signature: '', toolSignatures: {}, native: null, usage: {} };
    const options = {
        chatCompletionSource: task.source,
        model: task.model,
        customApiFormat: task.customApiFormat,
        opencodeApiFormat: task.opencodeApiFormat,
    };
    let text = '';
    for (const event of events) {
        if (event.type === 'error') throw new Error(event.message || '生成失败');
        if (event.type !== 'chunk' || !event.data || event.data === '[DONE]') continue;
        const parsed = JSON.parse(event.data);
        if (parsed?.choices?.some((choice) => choice?.delta?.tool_calls || choice?.message?.tool_calls)
            || parsed?.tool_calls || parsed?.tool_use) {
            throw new Error('回复包含工具调用，需在当前会话中手动处理');
        }
        text += getStreamingReply(parsed, state, options);
    }
    return { text, reasoning: state.reasoning, images: state.images, signature: state.signature };
}

function showResult(task, result, reason) {
    banner(reason, { label: '查看结果', key: task.taskId, run: () => {
        const dialog = document.createElement('dialog');
        dialog.id = 'tt-web-generation-result';
        const title = document.createElement('p');
        title.textContent = reason;
        const textarea = document.createElement('textarea');
        textarea.readOnly = true;
        textarea.value = result?.text || JSON.stringify(result?.events || [], null, 2);
        const close = document.createElement('button');
        close.type = 'button';
        close.textContent = '关闭';
        close.addEventListener('click', () => dialog.close());
        dialog.append(title, textarea, close);
        dialog.addEventListener('close', () => dialog.remove());
        document.body.append(dialog);
        dialog.showModal();
    } });
}

function claimToken(taskId) {
    const key = `tt-web-claim:${taskId}`;
    let token = sessionStorage.getItem(key);
    if (!token) {
        token = crypto?.randomUUID?.() || 'xxxxxxxx-xxxx-4xxx-yxxx-xxxxxxxxxxxx'.replace(/[xy]/g, (char) => {
            const byte = crypto.getRandomValues(new Uint8Array(1))[0] & 15;
            return (char === 'x' ? byte : (byte & 3) | 8).toString(16);
        });
        sessionStorage.setItem(key, token);
    }
    return token;
}

async function commitTask(task, { fromOriginalPage = false } = {}) {
    const context = window.SillyTavern?.getContext?.();
    if (!context?.chat || task.status !== 'done' || task.confirmedAt) return;
    const token = claimToken(task.taskId);
    const claim = await invoke('claim_web_generation', { taskId: task.taskId, token });
    if (!claim.acquired) {
        banner('另一页面正在接回这次生成。');
        return;
    }
    const alreadySaved = context.chat.some((message) => message?.extra?.tt_web_task_id === task.taskId);
    if (!alreadySaved) {
        const current = await getWebGenerationContext();
        if (!current || current.chatKey !== task.chatKey || (!fromOriginalPage && current.revision !== task.revision)) {
            const result = await readAll(task.taskId);
            let parsed = null;
            try { parsed = parseResult(result.events, task); } catch { parsed = { events: result.events }; }
            showResult(task, parsed, '聊天已更改，生成结果已保留，未覆盖现有内容。');
            return;
        }
        if (!fromOriginalPage) {
            const result = await readAll(task.taskId);
            let parsed;
            try {
                parsed = parseResult(result.events, task);
            } catch (error) {
                showResult(task, { events: result.events }, `无法自动还原此回复：${error.message}`);
                return;
            }
            if (!parsed.text) {
                showResult(task, parsed, '生成结果为空，未写入聊天。');
                return;
            }
            await saveReply({
                type: 'normal',
                getMessage: parsed.text,
                reasoning: parsed.reasoning,
                imageUrls: parsed.images,
                reasoningSignature: parsed.signature || null,
                providerReplay: { chatCompletionSource: task.source, model: task.model },
            });
        }
        const message = context.chat.at(-1);
        if (!message || message.is_user) throw new Error('生成回复尚未写入聊天');
        if (fromOriginalPage) {
            const finishedAt = new Date(message.gen_finished).getTime();
            if (!Number.isFinite(finishedAt) || finishedAt < task.createdAt * 1000 - 1000) {
                throw new Error('无法确认最新回复属于这次生成');
            }
        }
        message.extra ||= {};
        message.extra.tt_web_task_id = task.taskId;
        await saveChatConditional();
    }
    await invoke('confirm_web_generation', { taskId: task.taskId, token });
    banner(null);
}

async function inspect() {
    if (checking || document.body.dataset.generating === 'true') return;
    const chatKey = getWebChatKey();
    if (!chatKey) return;
    checking = true;
    try {
        if (lastChatKey !== chatKey) { lastChatKey = chatKey; banner(null); }
        const tasks = await invoke('list_web_generations', { chatKey });
        const pending = tasks.filter((task) => !task.confirmedAt && task.taskId !== activeTask?.taskId && !processingTasks.has(task.taskId))
            .sort((left, right) => right.createdAt - left.createdAt);
        const task = pending[0];
        if (!task) { banner(null); return; }
        if (task.status === 'running') {
            banner('模型仍在 NAS 上生成，返回后会自动接回。', {
                label: '停止生成',
                key: task.taskId,
                run: () => void invoke('cancel_web_generation', { taskId: task.taskId }).then(() => inspect()).catch(console.error),
            });
        } else if (task.status === 'done') {
            await commitTask(task);
        } else {
            const result = await readAll(task.taskId);
            showResult(task, { events: result.events }, `生成${task.status === 'interrupted' ? '因服务重启中断' : '已停止或失败'}，已收到的内容仍可查看。`);
        }
    } catch (error) {
        banner(`生成恢复失败：${error.message || error}`, { label: '重试', run: () => void inspect() });
    } finally {
        checking = false;
    }
}

export function startWebGenerationRecovery() {
    window.addEventListener('tt-web-generation-started', (event) => { activeTask = event.detail; });
    window.addEventListener('tt-web-recovered', () => void inspect());
    const context = window.SillyTavern?.getContext?.();
    context?.eventSource?.on?.(context.eventTypes?.CHAT_CHANGED || 'chat_id_changed', () => void inspect());
    context?.eventSource?.on?.(context.eventTypes?.GENERATION_ENDED || 'generation_ended', () => {
        if (!activeTask) return;
        const taskId = activeTask.taskId;
        processingTasks.add(taskId);
        void invoke('read_web_generation', { taskId, afterSeq: 0 })
            .then((result) => commitTask(result.task, { fromOriginalPage: true }))
            .catch(console.error)
            .finally(() => {
                activeTask = null;
                processingTasks.delete(taskId);
            });
    });
    clearInterval(timer);
    timer = setInterval(() => void inspect(), 1000);
    void inspect();
}
