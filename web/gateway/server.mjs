import { createHmac, timingSafeEqual } from 'node:crypto';
import { createReadStream, readFileSync, realpathSync, statSync } from 'node:fs';
import { createServer, request as httpRequest } from 'node:http';
import path from 'node:path';
import { fileURLToPath } from 'node:url';
import { createBrotliCompress, createGzip } from 'node:zlib';

const gatewayDir = path.dirname(fileURLToPath(import.meta.url));
const frontendRoot = realpathSync(path.resolve(gatewayDir, '../../src'));
let buildManifest = null;
try {
    buildManifest = JSON.parse(readFileSync(path.join(frontendRoot, '__tt_web_build.json'), 'utf8'));
} catch {
    buildManifest = { upstreamVersion: 'unknown', upstreamCommit: 'unknown', webAdapterVersion: 'v1', webAdapterCommit: 'unknown', buildId: 'development', assets: {} };
}
const port = Number(process.env.PORT || 8080);
const password = process.env.TT_WEB_PASSWORD;
const backendUrl = new URL(process.env.TT_BACKEND_URL || 'http://app:3093');
const cookieName = 'tt_web_session';
const sessionLifetimeSeconds = 8 * 60 * 60;
const loginWindowMs = 15 * 60 * 1000;
const loginLimit = 8;
const attemptsByAddress = new Map();

if (!password || password.length < 12) {
    throw new Error('TT_WEB_PASSWORD is required and must contain at least 12 characters');
}

function sessionSignature(expiresAt) {
    return createHmac('sha256', password).update(`tt-web-session:${expiresAt}`).digest('base64url');
}

function makeSession() {
    const expiresAt = Math.floor(Date.now() / 1000) + sessionLifetimeSeconds;
    return `${expiresAt}.${sessionSignature(expiresAt)}`;
}

function hasSession(request) {
    const cookieHeader = request.headers.cookie || '';
    const cookie = cookieHeader.split(';').map((part) => part.trim())
        .find((part) => part.startsWith(`${cookieName}=`));
    if (!cookie) return false;

    try {
        const value = decodeURIComponent(cookie.slice(cookieName.length + 1));
        const separator = value.indexOf('.');
        if (separator < 1) return false;

        const expiresAt = Number(value.slice(0, separator));
        const supplied = Buffer.from(value.slice(separator + 1));
        const expected = Buffer.from(sessionSignature(expiresAt));
        return Number.isSafeInteger(expiresAt)
            && expiresAt > Math.floor(Date.now() / 1000)
            && supplied.length === expected.length
            && timingSafeEqual(supplied, expected);
    } catch {
        return false;
    }
}

function sessionExpiresAt(request) {
    const cookie = (request.headers.cookie || '').split(';').map((part) => part.trim())
        .find((part) => part.startsWith(`${cookieName}=`));
    if (!cookie) return null;
    const value = decodeURIComponent(cookie.slice(cookieName.length + 1));
    const expiresAt = Number(value.split('.', 1)[0]);
    return Number.isSafeInteger(expiresAt) ? expiresAt : null;
}

function setSessionCookie(request, response, value, maxAge) {
    const secure = request.headers['x-forwarded-proto'] === 'https' ? '; Secure' : '';
    response.setHeader('Set-Cookie', `${cookieName}=${encodeURIComponent(value)}; Path=/; HttpOnly; SameSite=Strict; Max-Age=${maxAge}${secure}`);
}

function respondJson(response, status, body) {
    response.writeHead(status, {
        'Content-Type': 'application/json; charset=utf-8',
        'Cache-Control': 'no-store',
        'X-Content-Type-Options': 'nosniff',
    });
    response.end(JSON.stringify(body));
}

async function readJson(request, limit = 4096) {
    const chunks = [];
    let size = 0;
    for await (const chunk of request) {
        size += chunk.length;
        if (size > limit) throw new Error('Request body is too large');
        chunks.push(chunk);
    }
    return JSON.parse(Buffer.concat(chunks).toString('utf8'));
}

function loginAllowed(address) {
    const now = Date.now();
    const current = attemptsByAddress.get(address);
    if (!current || now - current.startedAt >= loginWindowMs) {
        attemptsByAddress.set(address, { startedAt: now, count: 0 });
        return true;
    }
    return current.count < loginLimit;
}

function recordFailedLogin(address) {
    const current = attemptsByAddress.get(address);
    if (current) current.count += 1;
}

function getClientAddress(request) {
    return request.socket.remoteAddress || 'unknown';
}

function getContentType(filePath) {
    const ext = path.extname(filePath).toLowerCase();
    return ({
        '.avif': 'image/avif',
        '.css': 'text/css; charset=utf-8',
        '.gif': 'image/gif',
        '.html': 'text/html; charset=utf-8',
        '.ico': 'image/x-icon',
        '.jpeg': 'image/jpeg',
        '.jpg': 'image/jpeg',
        '.js': 'text/javascript; charset=utf-8',
        '.json': 'application/json; charset=utf-8',
        '.mjs': 'text/javascript; charset=utf-8',
        '.mp3': 'audio/mpeg',
        '.mp4': 'video/mp4',
        '.ogg': 'audio/ogg',
        '.png': 'image/png',
        '.svg': 'image/svg+xml',
        '.ttf': 'font/ttf',
        '.txt': 'text/plain; charset=utf-8',
        '.wasm': 'application/wasm',
        '.webmanifest': 'application/manifest+json',
        '.webm': 'video/webm',
        '.webp': 'image/webp',
        '.woff': 'font/woff',
        '.woff2': 'font/woff2',
    })[ext] || 'application/octet-stream';
}

function proxyRequest(request, response) {
    const target = new URL(request.url || '/', backendUrl);
    const proxy = httpRequest(target, {
        method: request.method,
        headers: {
            ...request.headers,
            host: target.host,
            'x-forwarded-host': request.headers.host || '',
            'x-forwarded-proto': request.headers['x-forwarded-proto'] || 'http',
        },
    }, (upstream) => {
        response.writeHead(upstream.statusCode || 502, upstream.headers);
        upstream.pipe(response);
    });
    proxy.on('error', () => {
        if (!response.headersSent) respondJson(response, 502, { error: 'Application service is unavailable' });
        else response.destroy();
    });
    request.pipe(proxy);
}

function proxyUpgrade(request, clientSocket, head) {
    const target = new URL(request.url || '/', backendUrl);
    const proxy = httpRequest(target, {
        method: 'GET',
        headers: {
            ...request.headers,
            host: target.host,
            'x-forwarded-host': request.headers.host || '',
            'x-forwarded-proto': request.headers['x-forwarded-proto'] || 'http',
        },
    });
    proxy.on('upgrade', (response, upstreamSocket, upstreamHead) => {
        const headers = Object.entries(response.headers)
            .filter(([, value]) => value !== undefined)
            .map(([name, value]) => `${name}: ${Array.isArray(value) ? value.join(', ') : value}`);
        clientSocket.write(`HTTP/1.1 ${response.statusCode} ${response.statusMessage}\r\n${headers.join('\r\n')}\r\n\r\n`);
        if (upstreamHead.length) clientSocket.write(upstreamHead);
        if (head.length) upstreamSocket.write(head);
        upstreamSocket.pipe(clientSocket);
        clientSocket.pipe(upstreamSocket);
        clientSocket.on('error', () => upstreamSocket.destroy());
        upstreamSocket.on('error', () => clientSocket.destroy());
    });
    proxy.on('error', () => clientSocket.destroy());
    proxy.end();
}

const server = createServer(async (request, response) => {
    const pathname = new URL(request.url || '/', 'http://gateway.local').pathname;
    response.setHeader('X-Content-Type-Options', 'nosniff');
    response.setHeader('X-Frame-Options', 'DENY');
    response.setHeader('Referrer-Policy', 'same-origin');

    if (pathname === '/healthz') {
        return respondJson(response, 200, { status: 'ok' });
    }

    if (pathname === '/login' && request.method === 'GET') {
        response.writeHead(200, {
            'Content-Type': 'text/html; charset=utf-8',
            'Cache-Control': 'no-store',
            'X-Content-Type-Options': 'nosniff',
            'Content-Security-Policy': "default-src 'none'; style-src 'unsafe-inline'; script-src 'unsafe-inline'; connect-src 'self'; form-action 'self'; base-uri 'none'; frame-ancestors 'none'",
        });
        response.end(`<!doctype html><html lang="zh-CN"><meta charset="utf-8"><meta name="viewport" content="width=device-width,initial-scale=1"><title>TauriTavern 登录</title><style>body{margin:0;min-height:100vh;display:grid;place-items:center;background:#242425;color:#eee;font:16px system-ui}main{width:min(360px,calc(100vw - 40px));padding:28px;border:1px solid #555;border-radius:12px;background:#303033;box-sizing:border-box}h1{font-size:22px;margin:0 0 20px}label{display:block;margin-bottom:8px}input,button{box-sizing:border-box;width:100%;padding:12px;border-radius:7px;font:inherit}input{border:1px solid #666;background:#202022;color:#fff}button{margin-top:14px;border:0;background:#5f7fff;color:white;cursor:pointer}p{min-height:1.4em;color:#ff8d8d}</style><main><h1>TauriTavern</h1><form id="login"><label for="password">访问密码</label><input id="password" name="password" type="password" autocomplete="current-password" required autofocus><button>登录</button><p id="error" role="alert"></p></form></main><script>document.querySelector('#login').addEventListener('submit',async event=>{event.preventDefault();const error=document.querySelector('#error');error.textContent='';try{const response=await fetch('/auth/login',{method:'POST',headers:{'Content-Type':'application/json'},body:JSON.stringify({password:document.querySelector('#password').value})});if(!response.ok)throw new Error(response.status===429?'尝试次数过多，请稍后再试':'密码不正确');location.replace('/')}catch(reason){error.textContent=reason.message}})</script></html>`);
        return;
    }

    if (pathname === '/auth/login' && request.method === 'POST') {
        const address = getClientAddress(request);
        if (!loginAllowed(address)) return respondJson(response, 429, { error: 'Too many login attempts' });
        try {
            const body = await readJson(request);
            const candidate = Buffer.from(String(body.password || ''));
            const expected = Buffer.from(password);
            const matches = candidate.length === expected.length && timingSafeEqual(candidate, expected);
            if (!matches) {
                recordFailedLogin(address);
                return respondJson(response, 401, { error: 'Invalid password' });
            }
            attemptsByAddress.delete(address);
            setSessionCookie(request, response, makeSession(), sessionLifetimeSeconds);
            return respondJson(response, 200, { ok: true });
        } catch (error) {
            return respondJson(response, 400, { error: error.message || 'Invalid request' });
        }
    }

    if (pathname === '/auth/logout' && request.method === 'POST') {
        setSessionCookie(request, response, '', 0);
        return respondJson(response, 200, { ok: true });
    }

    if (pathname === '/logout' && request.method === 'GET') {
        setSessionCookie(request, response, '', 0);
        response.writeHead(302, { Location: '/login', 'Cache-Control': 'no-store' });
        return response.end();
    }

    if (!hasSession(request)) {
        if (pathname.startsWith('/api/') || pathname.startsWith('/__tt/') || pathname.startsWith('/auth/')) {
            return respondJson(response, 401, { error: 'Authentication required' });
        }
        response.writeHead(302, { Location: '/login', 'Cache-Control': 'no-store' });
        return response.end();
    }

    if (pathname === '/auth/status' && request.method === 'GET') {
        let appReady = false;
        try {
            const backendResponse = await fetch(new URL('/healthz', backendUrl), { signal: AbortSignal.timeout(2000) });
            appReady = backendResponse.ok;
        } catch {
            // The authenticated browser still needs a status response while the app restarts.
        }
        return respondJson(response, 200, {
            authenticated: true,
            appReady,
            sessionExpiresAt: sessionExpiresAt(request),
            build: {
                upstreamVersion: buildManifest.upstreamVersion,
                upstreamCommit: buildManifest.upstreamCommit,
                webAdapterVersion: buildManifest.webAdapterVersion,
                webAdapterCommit: buildManifest.webAdapterCommit,
                buildId: buildManifest.buildId,
            },
        });
    }

    if (request.method !== 'GET' && request.method !== 'HEAD') return proxyRequest(request, response);

    let candidate;
    try {
        const relativePath = decodeURIComponent(pathname === '/' ? '/index.html' : pathname);
        candidate = path.resolve(frontendRoot, `.${relativePath}`);
    } catch {
        return respondJson(response, 400, { error: 'Invalid path' });
    }
    if (candidate !== frontendRoot && !candidate.startsWith(`${frontendRoot}${path.sep}`)) {
        return respondJson(response, 400, { error: 'Invalid path' });
    }

    try {
        const filePath = realpathSync(candidate);
        if (filePath !== frontendRoot && !filePath.startsWith(`${frontendRoot}${path.sep}`)) {
            return respondJson(response, 404, { error: 'Not found' });
        }
        if (!statSync(filePath).isFile()) throw new Error('Not a file');
        const isHtml = filePath.endsWith('.html');
        const isManifest = pathname === '/__tt_web_build.json';
        if (isHtml || isManifest) {
            response.writeHead(200, {
                'Content-Type': getContentType(filePath),
                'Cache-Control': 'no-store',
                'X-Content-Type-Options': 'nosniff',
                ...(isHtml ? { 'X-TT-Web-Build': buildManifest.buildId } : {}),
            });
            if (request.method === 'HEAD') return response.end();
        }
        if (isHtml) {
            let html = await (await import('node:fs/promises')).readFile(filePath, 'utf8');
            const build = JSON.stringify({
                upstreamVersion: buildManifest.upstreamVersion,
                upstreamCommit: buildManifest.upstreamCommit,
                webAdapterVersion: buildManifest.webAdapterVersion,
                webAdapterCommit: buildManifest.webAdapterCommit,
                buildId: buildManifest.buildId,
            }).replaceAll('<', '\\u003c');
            html = html.replace('</head>', `<link rel="stylesheet" href="/css/web-mobile.css"><script>window.__TAURITAVERN_WEB__=true;window.__TT_WEB_BUILD__=${build};</script></head>`);
            return response.end(html);
        }
        if (isManifest) return createReadStream(filePath).pipe(response);

        const relative = `/${path.relative(frontendRoot, filePath).replaceAll(path.sep, '/')}`;
        const hash = buildManifest.assets?.[relative];
        const metadata = statSync(filePath);
        const etag = hash ? `"${hash}"` : `W/"${metadata.size}-${Math.floor(metadata.mtimeMs)}"`;
        const contentType = getContentType(filePath);
        const compressible = /^(text\/|application\/(?:json|javascript|wasm))/.test(contentType)
            && metadata.size >= 1024 && !request.headers.range;
        const acceptedEncoding = String(request.headers['accept-encoding'] || '');
        const encoding = compressible && /(?:^|,)\s*br(?:\s*;|\s*,|$)/.test(acceptedEncoding)
            ? 'br'
            : compressible && /(?:^|,)\s*gzip(?:\s*;|\s*,|$)/.test(acceptedEncoding) ? 'gzip' : null;
        const headers = {
            'Content-Type': contentType,
            'Cache-Control': 'private, max-age=0, must-revalidate',
            'X-Content-Type-Options': 'nosniff',
            ETag: etag,
            'Last-Modified': metadata.mtime.toUTCString(),
            Vary: 'Accept-Encoding',
            ...(encoding ? { 'Content-Encoding': encoding } : {}),
        };
        if (request.headers['if-none-match'] === etag) {
            response.writeHead(304, headers);
            return response.end();
        }
        response.writeHead(200, headers);
        if (request.method === 'HEAD') return response.end();
        const body = createReadStream(filePath);
        if (encoding === 'br') return body.pipe(createBrotliCompress()).pipe(response);
        if (encoding === 'gzip') return body.pipe(createGzip()).pipe(response);
        return body.pipe(response);
    } catch {
        return proxyRequest(request, response);
    }
});

server.on('upgrade', (request, clientSocket, head) => {
    if (!hasSession(request)) {
        clientSocket.write('HTTP/1.1 401 Unauthorized\r\nConnection: close\r\n\r\n');
        clientSocket.destroy();
        return;
    }
    proxyUpgrade(request, clientSocket, head);
});

server.listen(port, '0.0.0.0', () => {
    console.log(`TauriTavern web gateway listening on ${server.address().port}`);
});
