import assert from 'node:assert/strict';
import { test } from 'node:test';

class FakeWebSocket extends EventTarget {
    static instances = [];

    constructor(url) {
        super();
        this.url = String(url);
        FakeWebSocket.instances.push(this);
        queueMicrotask(() => this.dispatchEvent(new Event('open')));
    }

    close() {
        this.dispatchEvent(new Event('close'));
    }

    sendMessage(data) {
        const event = new Event('message');
        event.data = data;
        this.dispatchEvent(event);
    }
}

test('web invoke serializes channels and delivers streaming updates', async () => {
    const originalWindow = globalThis.window;
    const originalWebSocket = globalThis.WebSocket;
    const originalFetch = globalThis.fetch;
    const sentRequests = [];
    const receivedUpdates = [];

    globalThis.window = {
        __TAURITAVERN_WEB__: true,
        location: new URL('http://localhost:8080/'),
    };
    globalThis.WebSocket = FakeWebSocket;
    globalThis.fetch = async (_url, request) => {
        sentRequests.push(JSON.parse(request.body));
        return new Response(JSON.stringify({ result: { subscribed: true } }), {
            status: 200,
            headers: { 'Content-Type': 'application/json' },
        });
    };

    try {
        const bridge = await import('../../src/tauri-bridge.js?web-bridge-test');
        const channel = bridge.createChannel((update) => receivedUpdates.push(update));
        assert.deepEqual(await bridge.invoke('subscribe_agent_run_live_projection', {
            dto: { runId: 'run-1' },
            channel,
        }), { subscribed: true });

        assert.equal(sentRequests[0].command, 'subscribe_agent_run_live_projection');
        assert.equal(sentRequests[0].args.channel.__ttWebChannel, channel.id);
        assert.equal(new URL(FakeWebSocket.instances[0].url).pathname, '/__tt/channels');
        FakeWebSocket.instances[0].sendMessage(JSON.stringify({
            type: 'message',
            payload: { status: 'running', text: 'hello' },
        }));
        assert.deepEqual(receivedUpdates, [{ status: 'running', text: 'hello' }]);
    } finally {
        globalThis.window = originalWindow;
        globalThis.WebSocket = originalWebSocket;
        globalThis.fetch = originalFetch;
    }
});
