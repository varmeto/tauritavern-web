import { createHash } from 'node:crypto';
import { readFile, readdir, writeFile } from 'node:fs/promises';
import { execFileSync } from 'node:child_process';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

const root = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..');
const src = path.join(root, 'src');
const packageJson = JSON.parse(await readFile(path.join(root, 'package.json'), 'utf8'));
const upstreamMetadata = JSON.parse(await readFile(path.join(root, 'web', 'upstream.json'), 'utf8'));
const extensions = new Set(['.js', '.mjs', '.css', '.svg', '.woff', '.woff2', '.ttf', '.wasm']);
const assets = {};

async function collect(directory) {
    for (const entry of await readdir(directory, { withFileTypes: true })) {
        const filename = path.join(directory, entry.name);
        if (entry.isDirectory()) {
            if (filename === path.join(src, 'scripts', 'extensions', 'third-party')) continue;
            await collect(filename);
        } else if (entry.isFile() && extensions.has(path.extname(entry.name).toLowerCase())) {
            const url = `/${path.relative(src, filename).replaceAll(path.sep, '/')}`;
            if (url === '/tt-ext-sw.js' || url === '/css/user.css') continue;
            const bytes = await readFile(filename);
            assets[url] = createHash('sha256').update(bytes).digest('hex');
        }
    }
}

await collect(src);
let upstreamCommit = process.env.TT_UPSTREAM_COMMIT || upstreamMetadata.commit;
let webAdapterCommit = process.env.TT_WEB_ADAPTER_COMMIT || 'unknown';
try {
    if (path.basename(upstreamMetadata.tag) !== `v${packageJson.version}`) {
        throw new Error('Upstream metadata tag does not match package version');
    }
    if (!process.env.TT_UPSTREAM_COMMIT) {
        upstreamCommit = execFileSync('git', ['rev-parse', upstreamMetadata.tag], { cwd: root, encoding: 'utf8' }).trim();
    }
} catch {
    if (upstreamMetadata.tag !== `v${packageJson.version}`) throw new Error('Upstream metadata tag does not match package version');
}
try {
    if (!process.env.TT_WEB_ADAPTER_COMMIT) {
        webAdapterCommit = execFileSync('git', ['rev-parse', 'HEAD'], { cwd: root, encoding: 'utf8' }).trim();
    }
} catch {
    webAdapterCommit = process.env.TT_WEB_ADAPTER_COMMIT || 'unknown';
}
const webAdapterVersion = 'v1';
const buildId = createHash('sha256')
    .update(JSON.stringify({ version: packageJson.version, upstreamCommit, webAdapterVersion, webAdapterCommit, assets }))
    .digest('hex').slice(0, 20);
const manifest = {
    upstreamVersion: packageJson.version,
    upstreamCommit,
    webAdapterVersion,
    webAdapterCommit,
    buildId,
    assets,
};
await writeFile(path.join(src, '__tt_web_build.json'), `${JSON.stringify(manifest)}\n`);
console.log(`Web build ${buildId}: ${Object.keys(assets).length} static assets`);
