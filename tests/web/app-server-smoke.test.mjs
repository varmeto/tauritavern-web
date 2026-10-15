import assert from 'node:assert/strict';
import { spawn, spawnSync } from 'node:child_process';
import { createServer } from 'node:http';
import { mkdir, mkdtemp, rm, stat, writeFile } from 'node:fs/promises';
import net from 'node:net';
import os from 'node:os';
import path from 'node:path';
import { fileURLToPath } from 'node:url';
import test from 'node:test';

const repositoryRoot = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '../..');
const appManifest = path.join('web', 'app-server', 'Cargo.toml');
const appBinary = process.env.TT_WEB_SMOKE_APP_BINARY || path.join(
    repositoryRoot,
    'web',
    'app-server',
    'target',
    'debug',
    process.platform === 'win32' ? 'tt-web-host.exe' : 'tt-web-host',
);

async function getAvailablePort() {
    const server = net.createServer();
    await new Promise((resolve, reject) => {
        server.once('error', reject);
        server.listen(0, '127.0.0.1', resolve);
    });
    const { port } = server.address();
    await new Promise((resolve, reject) => server.close((error) => (error ? reject(error) : resolve())));
    return port;
}

function startApp(dataRoot, port, importRoot) {
    const child = spawn(appBinary, [], {
        cwd: repositoryRoot,
        env: {
            ...process.env,
            PORT: String(port),
            TT_DATA_DIR: dataRoot,
            TT_IMPORT_DIR: importRoot,
            TT_CONTENT_DIR: path.join(repositoryRoot, 'default', 'content'),
        },
        stdio: ['ignore', 'pipe', 'pipe'],
    });
    let output = '';
    child.stdout.setEncoding('utf8').on('data', (chunk) => { output += chunk; });
    child.stderr.setEncoding('utf8').on('data', (chunk) => { output += chunk; });
    return { child, get output() { return output; } };
}

async function waitForHealth(app, baseUrl) {
    const deadline = Date.now() + 20_000;
    while (Date.now() < deadline) {
        if (app.child.exitCode !== null) {
            throw new Error(`Web app exited before becoming healthy: ${app.output}`);
        }
        try {
            const response = await fetch(`${baseUrl}/healthz`);
            if (response.ok) return;
        } catch {
            // The server may still be binding its port.
        }
        await new Promise((resolve) => setTimeout(resolve, 100));
    }
    throw new Error(`Web app did not become healthy: ${app.output}`);
}

async function stopApp(app) {
    if (app.child.exitCode !== null) return;
    app.child.kill();
    await Promise.race([
        new Promise((resolve) => app.child.once('exit', resolve)),
        new Promise((_, reject) => setTimeout(() => reject(new Error('Web app did not stop')), 5_000)),
    ]);
}

async function invoke(baseUrl, command, args = {}) {
    const response = await fetch(`${baseUrl}/__tt/invoke`, {
        method: 'POST',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({ command, args }),
    });
    const body = await response.json();
    assert.equal(response.status, 200, `${command} failed: ${JSON.stringify(body)}`);
    return body.result;
}

test('web app bootstraps, persists data across restart, and imports staged world info', { timeout: 600_000 }, async (t) => {
    if (process.env.TT_WEB_SMOKE_SKIP_BUILD !== '1') {
        const build = spawnSync('cargo', ['build', '--locked', '--manifest-path', appManifest], {
            cwd: repositoryRoot,
            encoding: 'utf8',
            timeout: 480_000,
        });
        assert.equal(build.status, 0, `Rust host build failed:\n${build.stdout}\n${build.stderr}`);
    }

    const dataRoot = await mkdtemp(path.join(os.tmpdir(), 'tt-web-host-smoke-'));
    const importRoot = await mkdtemp(path.join(os.tmpdir(), 'tt-web-import-smoke-'));
    const port = await getAvailablePort();
    const baseUrl = `http://127.0.0.1:${port}`;
    let app;
    t.after(async () => {
        if (app) await stopApp(app).catch(() => {});
        await rm(dataRoot, { recursive: true, force: true });
        await rm(importRoot, { recursive: true, force: true });
    });

    app = startApp(dataRoot, port, importRoot);
    await waitForHealth(app, baseUrl);
    const health = await fetch(`${baseUrl}/healthz`).then((response) => response.json());
    assert.equal(health.status, 'ok');

    const snapshotResponse = await fetch(`${baseUrl}/api/bootstrap`, {
        method: 'POST',
        headers: { 'Content-Type': 'application/json' },
        body: '{}',
    });
    assert.equal(snapshotResponse.status, 200);
    const snapshot = await snapshotResponse.json();
    assert.ok(snapshot.settings);

    const settings = await invoke(baseUrl, 'get_sillytavern_settings');
    assert.equal(typeof settings.settings, 'string');
    const largeSettingsValue = 'x'.repeat(8 * 1024 * 1024);
    await invoke(baseUrl, 'save_user_settings', {
        settings: { web_smoke_large_setting: largeSettingsValue },
    });
    const persistedLargeSettings = await invoke(baseUrl, 'get_sillytavern_settings');
    assert.equal(JSON.parse(persistedLargeSettings.settings).web_smoke_large_setting.length, largeSettingsValue.length);

    assert.deepEqual(await invoke(baseUrl, 'backend_error_bridge_ready'), []);
    const errorSocket = new WebSocket(`${baseUrl.replace(/^http/, 'ws')}/__tt/events?event=tauritavern-backend-error`);
    await new Promise((resolve, reject) => {
        errorSocket.addEventListener('open', resolve, { once: true });
        errorSocket.addEventListener('error', reject, { once: true });
    });
    const backendErrorEvent = new Promise((resolve, reject) => {
        const timeout = setTimeout(() => reject(new Error('Backend error event was not delivered')), 5_000);
        errorSocket.addEventListener('message', (event) => {
            clearTimeout(timeout);
            resolve(JSON.parse(event.data));
        }, { once: true });
    });
    const failedInvoke = await fetch(`${baseUrl}/__tt/invoke`, {
        method: 'POST',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({ command: 'get_preset', args: { api_id: 'unknown', name: 'missing' } }),
    });
    assert.equal(failedInvoke.status, 500);
    const backendError = await backendErrorEvent;
    assert.match(backendError.payload.message, /Unknown API ID/);
    errorSocket.close();

    assert.ok(await invoke(baseUrl, 'get_tauritavern_settings'));
    assert.ok(await invoke(baseUrl, 'get_personas'));
    assert.ok(Array.isArray(await invoke(baseUrl, 'get_avatars')));
    assert.ok(Array.isArray(await invoke(baseUrl, 'get_all_backgrounds')));
    assert.ok(Array.isArray(await invoke(baseUrl, 'list_sprites', { dto: { name: 'web-smoke' } })));
    assert.ok(await invoke(baseUrl, 'get_assets_library'));
    const userImage = await invoke(baseUrl, 'upload_user_image', {
        image_base64: 'iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mP8/x8AAwMCAO+/F0cAAAAASUVORK5CYII=',
        format: 'png',
        filename: 'web-smoke-image.png',
        ch_name: 'web-smoke',
    });
    assert.match(userImage.path, /^user\/images\//);
    assert.deepEqual(await invoke(baseUrl, 'list_user_images', { folder: 'web-smoke' }), ['web-smoke-image.png']);
    assert.ok((await invoke(baseUrl, 'list_user_image_folders')).includes('web-smoke'));
    await invoke(baseUrl, 'delete_user_image', { path: userImage.path });
    const userFile = await invoke(baseUrl, 'upload_user_file', {
        name: 'web-smoke-data.json',
        data_base64: Buffer.from('{"web":true}').toString('base64'),
    });
    assert.equal(userFile.path, '/user/files/web-smoke-data.json');
    assert.deepEqual(await invoke(baseUrl, 'verify_user_files', { urls: [userFile.path] }), {
        [userFile.path]: true,
    });
    await invoke(baseUrl, 'delete_user_file', { path: userFile.path });

    const folder = await invoke(baseUrl, 'create_image_metadata_folder', { dto: { name: 'web-smoke-folder' } });
    assert.ok((await invoke(baseUrl, 'get_background_folders')).folders.some((item) => item.id === folder.id));
    await invoke(baseUrl, 'delete_image_metadata_folder', { dto: { id: folder.id } });

    const quickReplyPayload = { name: 'web-smoke-replies', qrList: [] };
    await invoke(baseUrl, 'save_quick_reply_set', { payload: quickReplyPayload });
    await invoke(baseUrl, 'delete_quick_reply_set', { payload: quickReplyPayload });
    await invoke(baseUrl, 'save_theme', { dto: { name: 'web-smoke-theme', data: { color: 'blue' } } });
    await invoke(baseUrl, 'delete_theme', { dto: { name: 'web-smoke-theme' } });

    const backgroundBytes = Buffer.from('iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mP8/x8AAwMCAO+/F0cAAAAASUVORK5CYII=', 'base64');
    const stagedBackground = await fetch(`${baseUrl}/__tt/uploads?extension=png&kind=background`, {
        method: 'POST',
        body: backgroundBytes,
    }).then((response) => response.json());
    await invoke(baseUrl, 'upload_background_from_path', {
        filename: 'web-smoke-background.png',
        file_path: stagedBackground.file_path,
    });
    assert.ok((await invoke(baseUrl, 'get_all_backgrounds')).some((item) => item.filename === 'web-smoke-background.png'));
    await invoke(baseUrl, 'delete_background', { dto: { bg: 'web-smoke-background.png' } });
    await fetch(`${baseUrl}/__tt/uploads?path=${encodeURIComponent(stagedBackground.file_path)}`, { method: 'DELETE' });

    const mcpServer = await invoke(baseUrl, 'create_mcp_server', {
        dto: {
            displayName: 'Web smoke MCP',
            endpoint: 'https://example.invalid/mcp',
            headers: {},
            protocolVersion: 'auto',
        },
    });
    assert.equal(mcpServer.state, 'paused');
    assert.ok((await invoke(baseUrl, 'list_mcp_servers')).servers.some((server) => server.id === mcpServer.id));
    await invoke(baseUrl, 'remove_mcp_server', { dto: { registrationId: mcpServer.id } });

    const skillName = 'web-smoke-skill';
    await invoke(baseUrl, 'install_skill_import', {
        request: {
            input: {
                kind: 'inlineFiles',
                files: [{
                    path: 'SKILL.md',
                    content: `---\nname: ${skillName}\ndescription: Web host integration fixture\n---\n\nSmoke test skill.\n`,
                }],
            },
            targetScope: { kind: 'global' },
        },
    });
    assert.ok((await invoke(baseUrl, 'list_skills')).some((skill) => skill.name === skillName));
    const skillText = await invoke(baseUrl, 'read_skill_file', { name: skillName, path: 'SKILL.md' });
    assert.match(skillText.content, /Smoke test skill/);
    await invoke(baseUrl, 'delete_skill', { name: skillName });

    await invoke(baseUrl, 'create_settings_snapshot');
    assert.ok((await invoke(baseUrl, 'get_settings_snapshots')).length > 0);

    const extensionDirectory = path.join(dataRoot, 'default-user', 'extensions', 'web-smoke-extension');
    await mkdir(extensionDirectory, { recursive: true });
    await writeFile(path.join(extensionDirectory, 'manifest.json'), JSON.stringify({ display_name: 'Web Smoke Extension', js: 'index.js' }));
    await writeFile(path.join(extensionDirectory, 'index.js'), 'export const ready = true;');
    const extensions = await invoke(baseUrl, 'get_extensions');
    assert.ok(extensions.some((extension) => extension.name === 'third-party/web-smoke-extension' && extension.extension_type === 'Local'));
    const extensionManifest = await fetch(`${baseUrl}/scripts/extensions/third-party/web-smoke-extension/manifest.json`);
    assert.equal(extensionManifest.status, 200);
    assert.equal((await extensionManifest.json()).display_name, 'Web Smoke Extension');
    const extensionScript = await fetch(`${baseUrl}/scripts/extensions/third-party/web-smoke-extension/index.js`);
    assert.equal(extensionScript.status, 200);
    assert.match(extensionScript.headers.get('content-type') || '', /javascript/);
    const extensionSource = await extensionScript.text();
    assert.match(extensionSource, /export const ready = true/);
    const extensionModule = await import(`data:text/javascript,${encodeURIComponent(extensionSource)}`);
    assert.equal(extensionModule.ready, true);
    const extensionStore = { namespace: 'web-smoke-extension', table: 'contract', key: 'profile' };
    const extensionStoreValue = { enabled: true, nested: ['persisted', 2] };
    await invoke(baseUrl, 'set_extension_store_json', { ...extensionStore, value: extensionStoreValue });
    assert.deepEqual(await invoke(baseUrl, 'get_extension_store_json', extensionStore), extensionStoreValue);
    assert.deepEqual(await invoke(baseUrl, 'try_get_extension_store_json', extensionStore), {
        found: true,
        value: extensionStoreValue,
    });
    assert.deepEqual(await invoke(baseUrl, 'list_extension_store_keys', {
        namespace: extensionStore.namespace,
        table: extensionStore.table,
    }), [extensionStore.key]);
    await invoke(baseUrl, 'update_extension_store_json', {
        ...extensionStore,
        value: { enabled: false, added: 'merged' },
    });
    const updatedExtensionStoreValue = { enabled: false, nested: ['persisted', 2], added: 'merged' };
    assert.deepEqual(await invoke(baseUrl, 'get_extension_store_json', extensionStore), updatedExtensionStoreValue);
    await invoke(baseUrl, 'rename_extension_store_key', {
        namespace: extensionStore.namespace,
        table: extensionStore.table,
        key: extensionStore.key,
        newKey: 'renamed-profile',
    });
    assert.deepEqual(await invoke(baseUrl, 'try_get_extension_store_json', extensionStore), { found: false });
    assert.deepEqual(await invoke(baseUrl, 'get_extension_store_json', {
        ...extensionStore,
        key: 'renamed-profile',
    }), updatedExtensionStoreValue);
    assert.ok((await invoke(baseUrl, 'list_extension_store_tables', {
        namespace: extensionStore.namespace,
    })).includes(extensionStore.table));
    const extensionBlob = Buffer.from([0, 1, 127, 255]);
    await invoke(baseUrl, 'set_extension_store_blob', {
        namespace: extensionStore.namespace,
        table: extensionStore.table,
        key: 'fixture.bin',
        dataBase64: extensionBlob.toString('base64'),
    });
    const storedBlob = await invoke(baseUrl, 'get_extension_store_blob', {
        namespace: extensionStore.namespace,
        table: extensionStore.table,
        key: 'fixture.bin',
    });
    assert.equal(storedBlob.content_base64, extensionBlob.toString('base64'));
    assert.equal(storedBlob.mime_type, 'application/octet-stream');
    assert.deepEqual(await invoke(baseUrl, 'list_extension_store_blob_keys', {
        namespace: extensionStore.namespace,
        table: extensionStore.table,
    }), ['fixture.bin']);
    await invoke(baseUrl, 'delete_extension_store_blob', {
        namespace: extensionStore.namespace,
        table: extensionStore.table,
        key: 'fixture.bin',
    });
    await invoke(baseUrl, 'delete_extension_store_table', {
        namespace: extensionStore.namespace,
        table: extensionStore.table,
    });
    assert.ok(!(await invoke(baseUrl, 'list_extension_store_tables', {
        namespace: extensionStore.namespace,
    })).includes(extensionStore.table));
    const agentProfiles = await invoke(baseUrl, 'list_agent_profiles');
    assert.ok(agentProfiles.profiles.some((profile) => profile.id === 'default-writer'));
    const agentTools = await invoke(baseUrl, 'list_agent_tools');
    assert.ok(agentTools.tools.some((tool) => tool.nativeName === 'workspace.read_file'));
    const character = await invoke(baseUrl, 'create_character', {
        dto: {
            file_name: null,
            json_data: null,
            primary_lorebook: null,
            name: 'Web Smoke Character',
            description: 'A browser host integration fixture.',
            personality: '',
            scenario: '',
            first_mes: 'Hello.',
            mes_example: '',
            creator: null,
            creator_notes: null,
            character_version: null,
            tags: [],
            talkativeness: 0.5,
            fav: false,
            alternate_greetings: [],
            system_prompt: '',
            post_history_instructions: '',
            extensions: {},
        },
    });
    assert.equal(character.name, 'Web Smoke Character');
    const duplicateCharacter = await invoke(baseUrl, 'duplicate_character', {
        dto: { name: character.name },
    });
    assert.notEqual(duplicateCharacter.avatar, character.avatar);
    const duplicateStem = duplicateCharacter.avatar.replace(/\.png$/i, '');
    const renamedCharacter = await invoke(baseUrl, 'rename_character', {
        dto: { old_name: duplicateStem, new_name: 'Web Smoke Renamed Character' },
    });
    assert.equal(renamedCharacter.avatar, 'Web Smoke Renamed Character.png');
    const exportedCharacter = await invoke(baseUrl, 'export_character_content', {
        dto: { name: character.name, format: 'json' },
    });
    assert.equal(exportedCharacter.mime_type, 'application/json');
    assert.match(Buffer.from(exportedCharacter.data).toString('utf8'), /Web Smoke Character/);
    const exportedPng = await invoke(baseUrl, 'export_character_content', {
        dto: { name: character.name, format: 'png' },
    });
    assert.equal(exportedPng.mime_type, 'image/png');
    assert.deepEqual(Buffer.from(exportedPng.data).subarray(0, 8), Buffer.from([137, 80, 78, 71, 13, 10, 26, 10]));
    await invoke(baseUrl, 'save_preset', {
        dto: { name: 'web-smoke-preset', apiId: 'openai', preset: { temperature: 0.7 } },
    });
    assert.ok((await invoke(baseUrl, 'list_presets', { apiId: 'openai' })).includes('web-smoke-preset'));
    assert.deepEqual(await invoke(baseUrl, 'get_preset', {
        name: 'web-smoke-preset',
        apiId: 'openai',
    }), { temperature: 0.7, name: 'web-smoke-preset' });
    assert.equal(await invoke(baseUrl, 'preset_exists', {
        name: 'web-smoke-preset',
        apiId: 'openai',
    }), true);
    const restoredPreset = await invoke(baseUrl, 'restore_preset', {
        dto: { name: 'Default', apiId: 'openai' },
    });
    assert.equal(restoredPreset.isDefault, true);
    assert.ok(restoredPreset.preset);
    const group = await invoke(baseUrl, 'create_group', {
        dto: { name: 'web-smoke-group', members: [] },
    });
    assert.equal(group.name, 'web-smoke-group');
    assert.ok((await invoke(baseUrl, 'get_all_groups')).some((item) => item.id === group.id));
    assert.equal((await invoke(baseUrl, 'get_group', { id: group.id })).name, group.name);
    assert.ok(Array.isArray(await invoke(baseUrl, 'list_group_chat_summaries')));
    assert.ok(Array.isArray(await invoke(baseUrl, 'list_recent_group_chat_summaries')));
    assert.ok(Array.isArray(await invoke(baseUrl, 'search_group_chats', { query: 'web-smoke' })));
    assert.equal((await invoke(baseUrl, 'get_character', { name: character.name })).name, character.name);

    let providerRequests = 0;
    let providerDelayMs = 15;
    const provider = createServer((request, response) => {
        providerRequests += 1;
        request.resume();
        response.writeHead(200, { 'Content-Type': 'text/event-stream' });
        response.write('data: {"choices":[{"index":0,"delta":{"content":"Hello from the mock."},"finish_reason":null}]}\n\n');
        setTimeout(() => {
            response.end('data: [DONE]\n\n');
        }, providerDelayMs);
    });
    await new Promise((resolve, reject) => {
        provider.once('error', reject);
        provider.listen(0, '127.0.0.1', resolve);
    });
    const providerPort = provider.address().port;
    t.after(() => new Promise((resolve, reject) => provider.close((error) => (error ? reject(error) : resolve()))));

    const streamId = 'web-smoke-stream';
    const streamPayload = {
        chat_completion_source: 'custom',
        custom_api_format: 'openai_compat',
        custom_url: `http://127.0.0.1:${providerPort}/v1`,
        model: 'smoke-model',
        messages: [{ role: 'user', content: 'hello' }],
        stream: true,
    };
    const startRequest = (url) => fetch(`${baseUrl}/__tt/invoke`, {
        method: 'POST',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({
            command: 'start_chat_completion_stream',
            args: { streamId, dto: streamPayload },
        }),
    });
    const confirmationResponse = await startRequest();
    assert.equal(confirmationResponse.status, 403);
    const confirmation = await confirmationResponse.json();
    assert.equal(confirmation.requires_confirmation, true);
    await invoke(baseUrl, 'grant_user_endpoint_access', { endpoint: confirmation.endpoint });
    const startResponse = await startRequest();
    assert.equal(startResponse.status, 200, await startResponse.text());

    let streamResult;
    const streamDeadline = Date.now() + 10_000;
    do {
        streamResult = await invoke(baseUrl, 'read_chat_completion_stream', { streamId, afterSeq: 0 });
        if (streamResult.status === 'done' || streamResult.status === 'error') break;
        await new Promise((resolve) => setTimeout(resolve, 50));
    } while (Date.now() < streamDeadline);
    assert.equal(streamResult.status, 'done', JSON.stringify(streamResult));
    assert.ok(streamResult.events.some((event) => event.type === 'chunk'));
    assert.equal(providerRequests, 1);
    const webTaskId = crypto.randomUUID();
    const webTaskArgs = {
        taskId: webTaskId,
        chatKey: 'character:smoke:chat',
        revision: 'smoke-revision',
        dto: streamPayload,
    };
    const startedWebTask = await invoke(baseUrl, 'start_web_generation', webTaskArgs);
    assert.equal(startedWebTask.taskId, webTaskId);
    assert.equal((await invoke(baseUrl, 'start_web_generation', webTaskArgs)).taskId, webTaskId);
    let webTaskResult;
    const webTaskDeadline = Date.now() + 10_000;
    do {
        webTaskResult = await invoke(baseUrl, 'read_web_generation', { taskId: webTaskId, afterSeq: 0 });
        if (webTaskResult.status !== 'running') break;
        await new Promise((resolve) => setTimeout(resolve, 50));
    } while (Date.now() < webTaskDeadline);
    assert.equal(webTaskResult.status, 'done', JSON.stringify(webTaskResult));
    assert.ok(webTaskResult.events.some((event) => event.type === 'chunk'));
    assert.deepEqual((await invoke(baseUrl, 'read_web_generation', { taskId: webTaskId, afterSeq: 0 })).events, webTaskResult.events);
    assert.equal(providerRequests, 2);
    const taskList = await invoke(baseUrl, 'list_web_generations', { chatKey: webTaskArgs.chatKey });
    assert.equal(taskList.length, 1);
    assert.equal(taskList[0].taskId, webTaskId);
    assert.equal(taskList[0].requestDigest, undefined);
    const claimToken = crypto.randomUUID();
    assert.equal((await invoke(baseUrl, 'claim_web_generation', { taskId: webTaskId, token: claimToken })).acquired, true);
    assert.equal((await invoke(baseUrl, 'confirm_web_generation', { taskId: webTaskId, token: claimToken })).confirmedAt > 0, true);
    const llmLogs = await invoke(baseUrl, 'devlog_get_llm_api_log_index', { limit: 10 });
    assert.equal(llmLogs.length, 2);
    assert.equal(llmLogs[0].source, 'custom');
    assert.equal(llmLogs[0].model, 'smoke-model');
    assert.equal(llmLogs[0].stream, true);
    const llmPreview = await invoke(baseUrl, 'devlog_get_llm_api_log_preview', { id: llmLogs[0].id });
    assert.match(llmPreview.requestReadable, /hello/);
    assert.match(llmPreview.responseReadable, /Hello from the mock/);
    const llmRaw = await invoke(baseUrl, 'devlog_get_llm_api_log_raw', { id: llmLogs[0].id });
    assert.match(llmRaw.responseRaw, /Hello from the mock/);

    const bundlePath = await invoke(baseUrl, 'devlog_export_bundle', {
        frontend_entries: [{ id: 1, timestampMs: Date.now(), level: 'info', message: 'web smoke', target: 'test' }],
    });
    const bundleUrl = new URL('/__tt/file', baseUrl);
    bundleUrl.searchParams.set('path', bundlePath);
    const bundleResponse = await fetch(bundleUrl);
    assert.equal(bundleResponse.status, 200);
    assert.deepEqual(Buffer.from(await bundleResponse.arrayBuffer()).subarray(0, 2), Buffer.from('PK'));

    const archiveJobId = await invoke(baseUrl, 'start_export_data_archive');
    let archiveStatus;
    const archiveDeadline = Date.now() + 30_000;
    do {
        archiveStatus = await invoke(baseUrl, 'get_data_archive_job_status', { job_id: archiveJobId });
        if (archiveStatus.state === 'completed' || archiveStatus.state === 'failed') break;
        await new Promise((resolve) => setTimeout(resolve, 100));
    } while (Date.now() < archiveDeadline);
    assert.equal(archiveStatus.state, 'completed', JSON.stringify(archiveStatus));
    const archiveDownloadPath = await invoke(baseUrl, 'save_export_data_archive', { job_id: archiveJobId });
    const archiveDownloadUrl = new URL('/__tt/file', baseUrl);
    archiveDownloadUrl.searchParams.set('path', archiveDownloadPath);
    const archiveResponse = await fetch(archiveDownloadUrl);
    assert.equal(archiveResponse.status, 200);
    const archiveBytes = Buffer.from(await archiveResponse.arrayBuffer());
    assert.deepEqual(archiveBytes.subarray(0, 2), Buffer.from('PK'));
    await writeFile(path.join(importRoot, 'nas-archive.zip'), archiveBytes);
    const nasArchives = await invoke(baseUrl, 'list_nas_data_archives');
    assert.ok(nasArchives.some((archive) => archive.fileName === 'nas-archive.zip'));
    await writeFile(path.join(importRoot, 'not-an-archive.txt'), 'ignored');
    assert.ok(!(await invoke(baseUrl, 'list_nas_data_archives')).some((archive) => archive.fileName === 'not-an-archive.txt'));
    const invalidNasResponse = await fetch(`${baseUrl}/__tt/invoke`, {
        method: 'POST',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({ command: 'start_import_nas_data_archive', args: { file_name: '../nas-archive.zip' } }),
    });
    assert.match(JSON.stringify(await invalidNasResponse.json()), /Invalid NAS archive file name/);
    const nasJobId = await invoke(baseUrl, 'start_import_nas_data_archive', { file_name: 'nas-archive.zip' });
    let nasStatus;
    const nasDeadline = Date.now() + 30_000;
    do {
        nasStatus = await invoke(baseUrl, 'get_data_archive_job_status', { job_id: nasJobId });
        if (['completed', 'failed', 'cancelled'].includes(nasStatus.state)) break;
        await new Promise((resolve) => setTimeout(resolve, 100));
    } while (Date.now() < nasDeadline);
    assert.equal(nasStatus.state, 'completed', JSON.stringify(nasStatus));
    assert.equal((await stat(path.join(importRoot, 'nas-archive.zip'))).size, archiveBytes.length);
    const invalidUploadName = await fetch(`${baseUrl}/__tt/import-uploads`, {
        method: 'POST',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({ file_name: '../invalid.zip', size: archiveBytes.length }),
    });
    assert.equal(invalidUploadName.status, 400);
    const beginResponse = await fetch(`${baseUrl}/__tt/import-uploads`, {
        method: 'POST',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({ file_name: 'browser-archive.zip', size: archiveBytes.length }),
    });
    assert.equal(beginResponse.status, 200);
    const browserUpload = await beginResponse.json();
    const incompletePath = path.join(dataRoot, '_tauritavern', 'web-imports', `${browserUpload.id}.part`);
    const incompleteImport = await fetch(`${baseUrl}/__tt/invoke`, {
        method: 'POST',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({ command: 'start_import_data_archive', args: { archive_path: incompletePath } }),
    });
    assert.match(JSON.stringify(await incompleteImport.json()), /completed web upload/);
    const overLimitResponse = await fetch(`${baseUrl}/__tt/import-uploads`, {
        method: 'POST',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({ file_name: 'too-large.zip', size: 4 * 1024 ** 3 + 1 }),
    });
    assert.equal(overLimitResponse.status, 413);
    const chunkLimit = await fetch(`${baseUrl}/__tt/import-uploads`, {
        method: 'POST',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({ file_name: 'chunk-limit.zip', size: 8 * 1024 * 1024 }),
    });
    assert.equal(chunkLimit.status, 200);
    const { id: chunkLimitId } = await chunkLimit.json();
    const maximumChunk = await fetch(`${baseUrl}/__tt/import-uploads?id=${chunkLimitId}&offset=0`, {
        method: 'PUT',
        body: Buffer.alloc(8 * 1024 * 1024),
    });
    assert.equal(maximumChunk.status, 200);
    assert.equal((await maximumChunk.json()).offset, 8 * 1024 * 1024);
    const discardChunk = await fetch(`${baseUrl}/__tt/import-uploads?id=${chunkLimitId}`, { method: 'DELETE' });
    assert.equal(discardChunk.status, 204);
    const prematureFinish = await fetch(`${baseUrl}/__tt/import-uploads/finish?id=${browserUpload.id}`, {
        method: 'POST',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({ file_name: 'browser-archive.zip', size: archiveBytes.length }),
    });
    assert.equal(prematureFinish.status, 400);
    const wrongOffsetResponse = await fetch(`${baseUrl}/__tt/import-uploads?id=${browserUpload.id}&offset=1`, {
        method: 'PUT',
        body: archiveBytes.subarray(0, 1),
    });
    assert.equal(wrongOffsetResponse.status, 409);
    const middle = Math.floor(archiveBytes.length / 2);
    for (const [offset, chunk] of [[0, archiveBytes.subarray(0, middle)], [middle, archiveBytes.subarray(middle)]]) {
        const chunkResponse = await fetch(`${baseUrl}/__tt/import-uploads?id=${browserUpload.id}&offset=${offset}`, {
            method: 'PUT',
            body: chunk,
        });
        assert.equal(chunkResponse.status, 200);
    }
    const finishResponse = await fetch(`${baseUrl}/__tt/import-uploads/finish?id=${browserUpload.id}`, {
        method: 'POST',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({ file_name: 'browser-archive.zip', size: archiveBytes.length }),
    });
    assert.equal(finishResponse.status, 200);
    const { archive_path: browserArchivePath } = await finishResponse.json();
    const browserJobId = await invoke(baseUrl, 'start_import_data_archive', {
        archive_path: browserArchivePath,
        archive_is_temporary: true,
    });
    let browserStatus;
    const browserDeadline = Date.now() + 30_000;
    do {
        browserStatus = await invoke(baseUrl, 'get_data_archive_job_status', { job_id: browserJobId });
        if (['completed', 'failed', 'cancelled'].includes(browserStatus.state)) break;
        await new Promise((resolve) => setTimeout(resolve, 100));
    } while (Date.now() < browserDeadline);
    assert.equal(browserStatus.state, 'completed', JSON.stringify(browserStatus));
    await assert.rejects(stat(browserArchivePath), { code: 'ENOENT' });
    const stagedArchiveResponse = await fetch(`${baseUrl}/__tt/uploads?extension=zip&kind=data-archive`, {
        method: 'POST',
        headers: { 'Content-Type': 'application/octet-stream' },
        body: archiveBytes,
    });
    assert.equal(stagedArchiveResponse.status, 200);
    const stagedArchive = await stagedArchiveResponse.json();
    const importJobId = await invoke(baseUrl, 'start_import_data_archive', {
        archive_path: stagedArchive.file_path,
        archive_is_temporary: true,
    });
    let importStatus;
    const importDeadline = Date.now() + 30_000;
    do {
        importStatus = await invoke(baseUrl, 'get_data_archive_job_status', { job_id: importJobId });
        if (importStatus.state === 'completed' || importStatus.state === 'failed') break;
        await new Promise((resolve) => setTimeout(resolve, 100));
    } while (Date.now() < importDeadline);
    assert.equal(importStatus.state, 'completed', JSON.stringify(importStatus));
    await fetch(`${baseUrl}/__tt/uploads?path=${encodeURIComponent(stagedArchive.file_path)}`, { method: 'DELETE' });
    await invoke(baseUrl, 'cleanup_web_archive_download', { path: archiveDownloadPath });

    await invoke(baseUrl, 'save_world_info', {
        dto: { name: 'web-smoke', data: { entries: {} } },
    });

    providerDelayMs = 5_000;
    const interruptedTaskId = crypto.randomUUID();
    await invoke(baseUrl, 'start_web_generation', {
        taskId: interruptedTaskId,
        chatKey: 'character:smoke:interrupted',
        revision: 'before-restart',
        dto: streamPayload,
    });
    let partial;
    const partialDeadline = Date.now() + 5_000;
    do {
        partial = await invoke(baseUrl, 'read_web_generation', { taskId: interruptedTaskId, afterSeq: 0 });
        if (partial.events.some((event) => event.type === 'chunk')) break;
        await new Promise((resolve) => setTimeout(resolve, 50));
    } while (Date.now() < partialDeadline);
    assert.ok(partial.events.some((event) => event.type === 'chunk'));
    assert.equal(partial.status, 'running');

    await stopApp(app);
    app = startApp(dataRoot, port, importRoot);
    await waitForHealth(app, baseUrl);
    const recoveredWebTask = await invoke(baseUrl, 'read_web_generation', { taskId: webTaskId, afterSeq: 0 });
    assert.equal(recoveredWebTask.status, 'done');
    assert.deepEqual(recoveredWebTask.events, webTaskResult.events);
    const interrupted = await invoke(baseUrl, 'read_web_generation', { taskId: interruptedTaskId, afterSeq: 0 });
    assert.equal(interrupted.status, 'interrupted');
    assert.ok(interrupted.events.some((event) => event.type === 'chunk'));
    const persistedWorld = await invoke(baseUrl, 'get_world_info', { dto: { name: 'web-smoke' } });
    assert.deepEqual(persistedWorld, { entries: {} });
    assert.ok((await invoke(baseUrl, 'devlog_get_llm_api_log_index', { limit: 10 })).length >= 2);
    assert.ok((await invoke(baseUrl, 'list_presets', { apiId: 'openai' })).includes('web-smoke-preset'));
    assert.equal(await invoke(baseUrl, 'preset_exists', {
        name: 'web-smoke-preset',
        apiId: 'openai',
    }), true);
    assert.equal((await invoke(baseUrl, 'get_group', { id: group.id })).name, group.name);

    const uploadResponse = await fetch(`${baseUrl}/__tt/uploads?extension=json&kind=worldinfo-import`, {
        method: 'POST',
        headers: { 'Content-Type': 'application/octet-stream' },
        body: Buffer.from('{"entries":{}}'),
    });
    assert.equal(uploadResponse.status, 200);
    const stagedUpload = await uploadResponse.json();
    const importResult = await invoke(baseUrl, 'import_world_info', {
        dto: {
            file_path: stagedUpload.file_path,
            original_filename: 'web-import-smoke.json',
            converted_data: null,
        },
    });
    assert.equal(typeof importResult.name, 'string');

    const cleanupResponse = await fetch(
        `${baseUrl}/__tt/uploads?path=${encodeURIComponent(stagedUpload.file_path)}`,
        { method: 'DELETE' },
    );
    assert.equal(cleanupResponse.status, 204);
    await invoke(baseUrl, 'delete_world_info', { dto: { name: importResult.name } });
    await invoke(baseUrl, 'delete_world_info', { dto: { name: 'web-smoke' } });
    await invoke(baseUrl, 'delete_preset', {
        dto: { name: 'web-smoke-preset', apiId: 'openai' },
    });
    await invoke(baseUrl, 'delete_group', { dto: { id: group.id } });
    await invoke(baseUrl, 'delete_character', {
        dto: { name: character.name, delete_chats: true },
    });
    await invoke(baseUrl, 'delete_character', {
        dto: { name: renamedCharacter.avatar.replace(/\.png$/i, ''), delete_chats: true },
    });
    await stopApp(app);
});
