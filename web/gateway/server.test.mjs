import assert from 'node:assert/strict';
import { createHash } from 'node:crypto';
import { createServer as createHttpServer } from 'node:http';
import { spawn } from 'node:child_process';
import { connect } from 'node:net';
import { once } from 'node:events';
import { setTimeout as delay } from 'node:timers/promises';
import { after, before, test } from 'node:test';
import { fileURLToPath } from 'node:url';

const serverPath = fileURLToPath(new URL('./server.mjs', import.meta.url));
let child;
let origin;
let backend;
let backendUrl;
let backendUpgradeCount = 0;
let output = '';

before(async () => {
    backend = createHttpServer(async (request, response) => {
        const chunks = [];
        for await (const chunk of request) chunks.push(chunk);
        response.writeHead(200, { 'Content-Type': 'application/json' });
        response.end(JSON.stringify({ result: { method: request.method, url: request.url, body: Buffer.concat(chunks).toString('utf8') } }));
    });
    backend.listen(0, '127.0.0.1');
    await once(backend, 'listening');
    backend.on('upgrade', (request, socket) => {
        backendUpgradeCount += 1;
        const key = request.headers['sec-websocket-key'];
        const accept = createHash('sha1')
            .update(`${key}258EAFA5-E914-47DA-95CA-C5AB0DC85B11`)
            .digest('base64');
        socket.write([
            'HTTP/1.1 101 Switching Protocols',
            'Upgrade: websocket',
            'Connection: Upgrade',
            `Sec-WebSocket-Accept: ${accept}`,
            '',
            '',
        ].join('\r\n'));
    });
    backendUrl = `http://127.0.0.1:${backend.address().port}`;

    child = spawn('node', [serverPath], {
        env: { ...process.env, PORT: '0', TT_BACKEND_URL: backendUrl, TT_WEB_PASSWORD: 'test-password-that-is-long-enough' },
        stdio: ['ignore', 'pipe', 'pipe'],
    });
    child.stdout.setEncoding('utf8');
    child.stderr.setEncoding('utf8');
    child.stdout.on('data', (chunk) => { output += chunk; });
    child.stderr.on('data', (chunk) => { output += chunk; });

    for (let attempt = 0; attempt < 100; attempt += 1) {
        const match = output.match(/listening on (\d+)/);
        if (match) {
            origin = `http://127.0.0.1:${match[1]}`;
            return;
        }
        if (child.exitCode !== null) throw new Error(`Gateway exited early: ${output}`);
        await delay(20);
    }
    throw new Error(`Gateway did not start: ${output}`);
});

after(async () => {
    if (child && child.exitCode === null) {
        child.kill();
        await once(child, 'exit');
    }
    backend?.close();
});

async function websocketHandshake(cookie) {
    const socket = connect(Number(new URL(origin).port), '127.0.0.1');
    const response = new Promise((resolve, reject) => {
        let received = '';
        socket.setEncoding('utf8');
        socket.once('error', reject);
        socket.on('data', (chunk) => {
            received += chunk;
            if (received.includes('\r\n\r\n')) resolve(received.split('\r\n', 1)[0]);
        });
    });
    await once(socket, 'connect');
    socket.write([
        'GET /__tt/events?event=app-ready HTTP/1.1',
        `Host: 127.0.0.1:${new URL(origin).port}`,
        'Upgrade: websocket',
        'Connection: Upgrade',
        'Sec-WebSocket-Version: 13',
        'Sec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==',
        ...(cookie ? [`Cookie: ${cookie}`] : []),
        '',
        '',
    ].join('\r\n'));
    const status = await response;
    socket.destroy();
    return status;
}

test('serves a public health check and redirects unauthenticated browser requests', async () => {
    const health = await fetch(`${origin}/healthz`);
    assert.equal(health.status, 200);
    assert.deepEqual(await health.json(), { status: 'ok' });

    const page = await fetch(origin, { redirect: 'manual' });
    assert.equal(page.status, 302);
    assert.equal(page.headers.get('location'), '/login');

    const loginPage = await fetch(`${origin}/login`);
    assert.equal(loginPage.status, 200);
    assert.match(loginPage.headers.get('content-security-policy') || '', /connect-src 'self'/);

    const malformedCookie = await fetch(origin, {
        headers: { Cookie: 'tt_web_session=%E0%A4%A' },
        redirect: 'manual',
    });
    assert.equal(malformedCookie.status, 302);
});

test('rejects unauthenticated API access and accepts a valid password', async () => {
    const api = await fetch(`${origin}/__tt/invoke`, { method: 'POST' });
    assert.equal(api.status, 401);

    const rejected = await fetch(`${origin}/auth/login`, {
        method: 'POST',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({ password: 'wrong-password' }),
    });
    assert.equal(rejected.status, 401);

    const accepted = await fetch(`${origin}/auth/login`, {
        method: 'POST',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({ password: 'test-password-that-is-long-enough' }),
    });
    assert.equal(accepted.status, 200);
    assert.match(accepted.headers.get('set-cookie') || '', /HttpOnly/);
    assert.match(accepted.headers.get('set-cookie') || '', /SameSite=Strict/);

    const cookie = accepted.headers.get('set-cookie').split(';', 1)[0];
    const invoked = await fetch(`${origin}/__tt/invoke`, {
        method: 'POST',
        headers: { Cookie: cookie, 'Content-Type': 'application/json' },
        body: JSON.stringify({ command: 'is_ready', args: {} }),
    });
    assert.equal(invoked.status, 200);
    const result = await invoked.json();
    assert.equal(result.result.url, '/__tt/invoke');
    assert.match(result.result.body, /"command":"is_ready"/);
});

test('serves the application shell only after login and marks browser host mode', async () => {
    const accepted = await fetch(`${origin}/auth/login`, {
        method: 'POST',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({ password: 'test-password-that-is-long-enough' }),
    });
    const cookie = accepted.headers.get('set-cookie').split(';', 1)[0];
    const page = await fetch(origin, { headers: { Cookie: cookie } });
    assert.equal(page.status, 200);
    assert.match(page.headers.get('content-type'), /text\/html/);
    assert.ok(page.headers.get('x-tt-web-build'));
    assert.match(await page.text(), /__TAURITAVERN_WEB__=true/);

    const status = await fetch(`${origin}/auth/status`, { headers: { Cookie: cookie } });
    assert.equal(status.status, 200);
    const statusBody = await status.json();
    assert.equal(statusBody.authenticated, true);
    assert.equal(statusBody.appReady, true);
    assert.equal(typeof statusBody.build.buildId, 'string');

    const staticAsset = await fetch(`${origin}/init.js`, {
        headers: { Cookie: cookie, 'Accept-Encoding': 'br' },
    });
    assert.equal(staticAsset.status, 200);
    assert.ok(staticAsset.headers.get('etag'));
    assert.equal(staticAsset.headers.get('content-encoding'), 'br');
    const unchanged = await fetch(`${origin}/init.js`, {
        headers: { Cookie: cookie, 'If-None-Match': staticAsset.headers.get('etag') },
    });
    assert.equal(unchanged.status, 304);

    const loggedOut = await fetch(`${origin}/logout`, {
        headers: { Cookie: cookie },
        redirect: 'manual',
    });
    assert.equal(loggedOut.status, 302);
    assert.match(loggedOut.headers.get('set-cookie') || '', /Max-Age=0/);
});

test('requires a valid session before proxying WebSocket event streams', async () => {
    backendUpgradeCount = 0;
    assert.equal(await websocketHandshake(), 'HTTP/1.1 401 Unauthorized');
    assert.equal(backendUpgradeCount, 0);

    const accepted = await fetch(`${origin}/auth/login`, {
        method: 'POST',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({ password: 'test-password-that-is-long-enough' }),
    });
    const cookie = accepted.headers.get('set-cookie').split(';', 1)[0];
    assert.equal(await websocketHandshake(cookie), 'HTTP/1.1 101 Switching Protocols');
    assert.equal(backendUpgradeCount, 1);
});

test('rejects weak or missing startup passwords', async () => {
    const invalid = spawn('node', [serverPath], {
        env: { ...process.env, PORT: '0', TT_WEB_PASSWORD: 'short' },
        stdio: ['ignore', 'ignore', 'pipe'],
    });
    const [code] = await once(invalid, 'exit');
    assert.notEqual(code, 0);
});
