import { renderExtensionTemplateAsync } from '../../extensions.js';
import { t, translate } from '../../i18n.js';
import { Popup, POPUP_TYPE, POPUP_RESULT } from '../../popup.js';
import { isAndroidRuntime, isIosRuntime } from '../../util/mobile-runtime.js';
import { getActiveIosPolicyActivationReport } from '../../tauritavern/ios-policy.js';
import { isWebRuntime, openDialog } from '../../../tauri-bridge.js';
import { flushLifecycleState } from '../../../tauri/main/services/lifecycle/lifecycle-flush-service.js';

const MODULE_NAME = 'data-migration';
const JOB_POLL_INTERVAL_MS = 1200;
const TERMINAL_JOB_STATES = new Set(['completed', 'failed', 'cancelled']);

const SILLYTAVERN_MIGRATION_COPY_KEY = 'Import a SillyTavern data archive (zip, tar, tar.gz, or tgz) and migrate it to TauriTavern.';
const TAURITAVERN_MIGRATION_COPY_KEY = 'Import a TauriTavern data zip archive from another device and migrate it to this TauriTavern.';

const jobState = {
    jobId: '',
    starting: false,
    cancelRequested: false,
    lastExportSavedPath: '',
    uploadController: null,
};

function extractErrorMessage(text) {
    if (!text) {
        return t`Unknown error`;
    }

    try {
        const json = JSON.parse(text);
        if (typeof json?.error === 'string' && json.error.trim()) {
            return json.error.trim();
        }
        if (typeof json?.message === 'string' && json.message.trim()) {
            return json.message.trim();
        }
    } catch {
        // Ignore JSON parse failure and fallback to plain text.
    }

    return String(text).trim() || t`Unknown error`;
}

async function readFailureMessage(response) {
    const responseText = await response.text();
    return extractErrorMessage(responseText);
}

function normalizeCaughtError(error) {
    if (error instanceof Error && typeof error.message === 'string') {
        return extractErrorMessage(error.message);
    }

    return extractErrorMessage(String(error || ''));
}

function requireJobId(payload, errorMessage) {
    if (typeof payload?.job_id !== 'string' || !payload.job_id.trim()) {
        throw new Error(errorMessage);
    }

    return payload.job_id.trim();
}

async function requestImportJob(url, init) {
    const response = await fetch(url, init);
    if (!response.ok) {
        throw new Error(await readFailureMessage(response));
    }

    const payload = await response.json();
    return requireJobId(payload, t`Import job id is missing`);
}

async function startImportJobFromDesktopPicker() {
    const picked = await openDialog({
        title: t`Select Data Archive`,
        multiple: false,
        directory: false,
        filters: [
            {
                name: t`Data Archive`,
                extensions: ['zip', 'tar', 'gz', 'tgz'],
            },
        ],
    });
    const archivePath = Array.isArray(picked) ? picked[0] : picked;
    if (typeof archivePath !== 'string' || archivePath.trim() === '') {
        return null;
    }

    return requestImportJob('/api/extensions/data-migration/import', {
        method: 'POST',
        headers: {
            'Content-Type': 'application/json',
        },
        body: JSON.stringify({ archive_path: archivePath }),
    });
}

async function pickNasImportArchive() {
    const response = await fetch('/api/extensions/data-migration/nas-archives');
    if (!response.ok) {
        throw new Error(await readFailureMessage(response));
    }

    const archives = await response.json();
    if (!Array.isArray(archives) || archives.length === 0) {
        toastr.warning(t`No supported archive files found in the server import folder`);
        return null;
    }

    const select = document.createElement('select');
    select.className = 'text_pole';
    for (const archive of archives) {
        const option = document.createElement('option');
        option.value = String(archive?.fileName || '');
        const size = Number(archive?.sizeBytes || 0);
        option.textContent = `${option.value} (${formatArchiveSize(size)})`;
        select.append(option);
    }
    const content = document.createElement('div');
    const label = document.createElement('label');
    label.textContent = t`Select an archive already stored on the server`;
    label.append(select);
    content.append(label);

    let selectedFileName = '';
    const popup = new Popup(content, POPUP_TYPE.TEXT, '', {
        okButton: t`Continue`,
        cancelButton: t`Cancel`,
        onClosing: () => {
            selectedFileName = select.value;
            return true;
        },
    });
    const result = await popup.show();
    return result === POPUP_RESULT.AFFIRMATIVE ? selectedFileName : null;
}

function formatArchiveSize(sizeBytes) {
    const size = Math.max(0, Number(sizeBytes) || 0);
    if (size < 1024 * 1024) {
        return `${Math.ceil(size / 1024)} KB`;
    }
    return `${(size / (1024 * 1024)).toFixed(1)} MB`;
}

async function startImportJobFromNasArchive(fileName) {
    return requestImportJob('/api/extensions/data-migration/import/nas', {
        method: 'POST',
        headers: {
            'Content-Type': 'application/json',
        },
        body: JSON.stringify({ file_name: fileName }),
    });
}

function pickBrowserArchive() {
    return new Promise((resolve) => {
        const input = document.getElementById('data_migration_import_file');
        if (!(input instanceof HTMLInputElement)) {
            resolve(null);
            return;
        }
        const finish = () => {
            const file = input.files?.[0] || null;
            input.removeEventListener('change', finish);
            input.removeEventListener('cancel', finish);
            resolve(file);
        };
        input.value = '';
        input.addEventListener('change', finish, { once: true });
        input.addEventListener('cancel', finish, { once: true });
        input.click();
    });
}

async function sendImportChunk(id, offset, chunk, fileSize, signal) {
    const end = offset + chunk.size;
    for (let attempt = 0; attempt < 4; attempt++) {
        if (signal.aborted) {
            throw new Error(t`Upload cancelled`);
        }
        let retryReason = '';
        try {
            const response = await fetch(`/__tt/import-uploads?id=${encodeURIComponent(id)}&offset=${offset}`, {
                method: 'PUT',
                body: chunk,
                credentials: 'same-origin',
                signal,
            });
            const result = await response.json().catch(() => ({}));
            if (response.ok && result.offset === end) {
                setProgress(end / fileSize * 100);
                setStatusText(`${t`Uploading archive...`} ${formatArchiveSize(end)} / ${formatArchiveSize(fileSize)}`);
                return;
            }
            if (response.status === 409 && result.offset === end) {
                setProgress(end / fileSize * 100);
                return;
            }
            if (![408, 429, 502, 503, 504].includes(response.status)) {
                throw new Error(result.error || `Upload failed (${response.status})`);
            }
            retryReason = result.error || `Upload failed (${response.status})`;
        } catch (error) {
            if (signal.aborted || !/Failed to fetch|NetworkError|Load failed/i.test(String(error))) {
                throw error;
            }
            retryReason = String(error);
        }
        if (attempt === 3) {
            throw new Error(`${retryReason}; ${t`Try the server import folder.`}`);
        }
        await new Promise((resolve) => setTimeout(resolve, 350 * (attempt + 1)));
    }
}

async function startImportJobFromBrowserFile(file) {
    const controller = new AbortController();
    jobState.uploadController = controller;
    refreshControls();
    let id = '';
    let submitted = false;
    try {
        const response = await fetch('/__tt/import-uploads', {
            method: 'POST',
            headers: { 'Content-Type': 'application/json' },
            body: JSON.stringify({ file_name: file.name, size: file.size }),
            signal: controller.signal,
        });
        if (!response.ok) {
            if ([408, 413, 502, 504].includes(response.status)) {
                throw new Error(t`Upload connection was rejected. Try the server import folder.`);
            }
            throw new Error(await readFailureMessage(response));
        }
        const session = await response.json();
        id = String(session?.id || '');
        const chunkSize = Number(session?.chunk_bytes);
        if (!id || !Number.isSafeInteger(chunkSize) || chunkSize <= 0) {
            throw new Error(t`Upload session is invalid`);
        }
        setProgress(0);
        setStatusText(t`Uploading archive...`);
        for (let offset = 0; offset < file.size; offset += chunkSize) {
            await sendImportChunk(id, offset, file.slice(offset, offset + chunkSize), file.size, controller.signal);
        }
        const finishResponse = await fetch(`/__tt/import-uploads/finish?id=${encodeURIComponent(id)}`, {
            method: 'POST',
            headers: { 'Content-Type': 'application/json' },
            body: JSON.stringify({ file_name: file.name, size: file.size }),
            signal: controller.signal,
        });
        if (!finishResponse.ok) {
            if ([408, 413, 502, 504].includes(finishResponse.status)) {
                throw new Error(t`Upload connection was rejected. Try the server import folder.`);
            }
            throw new Error(await readFailureMessage(finishResponse));
        }
        const finished = await finishResponse.json();
        setStatusText(t`Upload complete. Starting import...`);
        jobState.uploadController = null;
        refreshControls();
        const jobId = await requestImportJob('/api/extensions/data-migration/import', {
            method: 'POST',
            headers: { 'Content-Type': 'application/json' },
            body: JSON.stringify({ archive_path: finished.archive_path }),
        });
        submitted = true;
        return jobId;
    } finally {
        jobState.uploadController = null;
        refreshControls();
        if (id && !submitted) {
            await fetch(`/__tt/import-uploads?id=${encodeURIComponent(id)}`, { method: 'DELETE' }).catch(() => {});
        }
    }
}

async function onWebImportFromComputer() {
    if (hasActiveJob()) {
        toastr.warning(t`A migration job is already running`);
        return;
    }
    const file = await pickBrowserArchive();
    if (file) {
        await runConfirmedImport(() => startImportJobFromBrowserFile(file));
    }
}

async function onWebImportFromServer() {
    if (hasActiveJob()) {
        toastr.warning(t`A migration job is already running`);
        return;
    }
    try {
        const fileName = await pickNasImportArchive();
        if (fileName) {
            await runConfirmedImport(() => startImportJobFromNasArchive(fileName));
        }
    } catch (error) {
        const message = `${normalizeCaughtError(error)}. ${t`Check the server imports folder mount, or upload from this device.`}`;
        toastr.error(message, t`Data import failed`);
        setStatusText(message);
    }
}

async function startImportJobFromAndroidContentUri(contentUri) {
    return requestImportJob('/api/extensions/data-migration/import/android', {
        method: 'POST',
        headers: {
            'Content-Type': 'application/json',
        },
        body: JSON.stringify({
            content_uri: contentUri,
        }),
    });
}

async function pickAndroidImportArchive() {
    const response = await fetch('/api/extensions/data-migration/import/android/pick', {
        method: 'POST',
    });
    if (!response.ok) {
        throw new Error(await readFailureMessage(response));
    }

    const payload = await response.json();
    const contentUri = String(payload?.content_uri || '').trim();
    if (!contentUri) {
        throw new Error(t`Android import picker did not return a content URI`);
    }

    return contentUri;
}

async function startImportJobFromIosPicker() {
    const response = await fetch('/api/extensions/data-migration/import/ios', {
        method: 'POST',
        headers: {
            'Content-Type': 'application/json',
        },
        body: JSON.stringify({}),
    });

    if (!response.ok) {
        throw new Error(await readFailureMessage(response));
    }

    const payload = await response.json();
    if (payload?.cancelled) {
        return null;
    }

    return requireJobId(payload, t`Import job id is missing`);
}

async function saveExportArchive(jobId) {
    if (isAndroidRuntime()) {
        const response = await fetch('/api/extensions/data-migration/export/android/save', {
            method: 'POST',
            headers: {
                'Content-Type': 'application/json',
            },
            body: JSON.stringify({ job_id: jobId }),
        });
        if (!response.ok) {
            throw new Error(await readFailureMessage(response));
        }

        const payload = await response.json();
        return {
            mode: 'mobile-native',
            cancelled: Boolean(payload?.cancelled),
            savedPath: String(payload?.saved_target || ''),
            cleanupError: payload?.cleanup_error ? String(payload.cleanup_error) : null,
        };
    }

    if (isIosRuntime()) {
        const response = await fetch('/api/extensions/data-migration/export/ios/share', {
            method: 'POST',
            headers: {
                'Content-Type': 'application/json',
            },
            body: JSON.stringify({ job_id: jobId }),
        });
        if (!response.ok) {
            throw new Error(await readFailureMessage(response));
        }

        const payload = await response.json();
        return {
            mode: 'ios-native-share',
            completed: Boolean(payload?.completed),
            activity: payload?.activity ? String(payload.activity) : null,
            cleanupError: payload?.cleanup_error ? String(payload.cleanup_error) : null,
        };
    }

    const response = await fetch('/api/extensions/data-migration/export/save', {
        method: 'POST',
        headers: {
            'Content-Type': 'application/json',
        },
        body: JSON.stringify({ job_id: jobId }),
    });
    if (!response.ok) {
        throw new Error(await readFailureMessage(response));
    }

    const payload = await response.json();
    return {
        mode: 'desktop-native',
        savedPath: String(payload?.saved_target || ''),
    };
}

function hasActiveJob() {
    return jobState.starting || Boolean(jobState.jobId);
}

function setStatusText(message) {
    $('#data_migration_status').text(String(message || ''));
}

function setProgress(value) {
    const progress = $('#data_migration_progress');
    if (!progress.length) {
        return;
    }

    progress.prop('hidden', false);
    if (Number.isFinite(value)) {
        const clamped = Math.max(0, Math.min(100, value));
        progress.attr('value', clamped);
        progress.attr('aria-valuenow', clamped);
    } else {
        progress.removeAttr('value aria-valuenow');
    }
}

function hideProgress() {
    const progress = $('#data_migration_progress');
    progress.prop('hidden', true);
    progress.removeAttr('value aria-valuenow');
}

function refreshExportActions() {
    const actions = $('#data_migration_export_actions');
    if (!actions.length) {
        return;
    }

    const savedPath = String(jobState.lastExportSavedPath || '').trim();
    const visible = Boolean(savedPath) && !isAndroidRuntime() && !isIosRuntime();
    actions.toggle(visible);

    $('#data_migration_reveal_export_button').prop('disabled', !visible);
}

function refreshControls() {
    const busy = hasActiveJob();
    $('#data_migration_import_button').prop('disabled', busy);
    $('#data_migration_import_local_button, #data_migration_import_server_button').prop('disabled', busy);
    $('#data_migration_export_button').prop('disabled', busy);
    if (busy) {
        $('#data_migration_reveal_export_button').prop('disabled', true);
    } else {
        refreshExportActions();
    }

    const cancelButton = $('#data_migration_cancel_button');
    if (jobState.jobId || jobState.uploadController) {
        cancelButton.show();
        cancelButton.prop('disabled', jobState.cancelRequested);
        return;
    }

    cancelButton.hide();
    cancelButton.prop('disabled', false);
}

function markJobStarting() {
    jobState.jobId = '';
    jobState.starting = true;
    jobState.cancelRequested = false;
    refreshControls();
}

function startJobTracking(jobId) {
    jobState.jobId = jobId;
    jobState.starting = false;
    jobState.cancelRequested = false;
    refreshControls();
}

function stopJobTracking() {
    jobState.jobId = '';
    jobState.starting = false;
    jobState.cancelRequested = false;
    jobState.uploadController = null;
    hideProgress();
    refreshControls();
}

async function onRevealExportClick() {
    const savedPath = String(jobState.lastExportSavedPath || '').trim();
    if (!savedPath) {
        return;
    }

    try {
        const invoke = window.__TAURI__?.core?.invoke;
        if (typeof invoke !== 'function') {
            throw new Error('Tauri opener is unavailable');
        }

        await invoke('plugin:opener|reveal_item_in_dir', {
            paths: [savedPath],
        });
    } catch (error) {
        toastr.error(normalizeCaughtError(error), t`Unable to open folder`);
    }
}

async function onImportButtonClick() {
    try {
        if (hasActiveJob()) {
            toastr.warning(t`A migration job is already running`);
            return;
        }

        if (isAndroidRuntime()) {
            await onAndroidImportButtonClick();
            return;
        }

        if (isIosRuntime()) {
            await onIosImportButtonClick();
            return;
        }

        await runConfirmedImport(startImportJobFromDesktopPicker);
    } catch (error) {
        const failureMessage = normalizeCaughtError(error);
        toastr.error(failureMessage, t`Data import failed`);
        setStatusText(failureMessage);
    }
}

async function startExportJob() {
    const response = await fetch('/api/extensions/data-migration/export', {
        method: 'POST',
    });
    if (!response.ok) {
        throw new Error(await readFailureMessage(response));
    }

    const payload = await response.json();
    return requireJobId(payload, t`Export job id is missing`);
}

async function fetchJobStatus(jobId) {
    const response = await fetch(`/api/extensions/data-migration/job?id=${encodeURIComponent(jobId)}`, {
        method: 'GET',
        cache: 'no-store',
    });
    if (!response.ok) {
        throw new Error(await readFailureMessage(response));
    }

    return response.json();
}

function updateStatusFromJob(status) {
    const stage = String(status?.stage || '').trim();
    const message = String(status?.message || '').trim();
    const progress = Number(status?.progress_percent);

    setProgress(Number.isFinite(progress) ? progress : null);

    const parts = [];
    if (stage) {
        parts.push(stage);
    }
    if (Number.isFinite(progress)) {
        parts.push(`${progress.toFixed(1)}%`);
    }
    if (message) {
        parts.push(message);
    }

    if (parts.length === 0) {
        return;
    }

    setStatusText(parts.join(' | '));
}

async function pollUntilTerminal(jobId) {
    while (true) {
        const status = await fetchJobStatus(jobId);
        updateStatusFromJob(status);

        const state = status.state;
        if (TERMINAL_JOB_STATES.has(state)) {
            return status;
        }

        await sleep(JOB_POLL_INTERVAL_MS);
    }
}

function importStatusRequiresReload(status) {
    return Boolean(status?.local_applied) || Boolean(status?.reconcile_error);
}

function importTerminalMessage(status, fallback) {
    const message = String(status?.error || fallback || '').trim();
    const reconcileError = String(status?.reconcile_error || '').trim();
    return [message, reconcileError].filter(Boolean).join(' | ');
}

function reloadSoon() {
    setTimeout(() => {
        location.reload();
    }, 800);
}

async function requestCancelActiveJob() {
    if (jobState.uploadController) {
        jobState.uploadController.abort();
        setStatusText(t`Upload cancelled`);
        return;
    }
    if (!hasRunningJob() || jobState.cancelRequested) {
        return;
    }

    jobState.cancelRequested = true;
    refreshControls();

    try {
        const response = await fetch('/api/extensions/data-migration/job/cancel', {
            method: 'POST',
            headers: {
                'Content-Type': 'application/json',
            },
            body: JSON.stringify({ job_id: jobState.jobId }),
        });

        if (!response.ok) {
            const reason = await readFailureMessage(response);
            jobState.cancelRequested = false;
            refreshControls();
            toastr.error(reason, t`Failed to cancel job`);
            return;
        }

        setStatusText(t`Cancellation requested...`);
        toastr.info(t`Cancellation requested`);
    } catch (error) {
        jobState.cancelRequested = false;
        refreshControls();
        toastr.error(normalizeCaughtError(error), t`Failed to cancel job`);
    }
}

async function runMigrationJob(kind, startJob) {
    const failureTitle = kind === 'import' ? t`Data import failed` : t`Data export failed`;

    try {
        markJobStarting();
        setProgress(null);
        setStatusText(kind === 'import' ? t`Preparing import...` : t`Preparing export...`);
        await flushLifecycleState(`data-archive:${kind}`);
        const jobId = await startJob();
        if (!jobId) {
            if (kind === 'import') {
                toastr.info(t`Import cancelled`);
                setStatusText(t`Import cancelled`);
                return;
            }

            throw new Error(t`Migration job did not return a job id`);
        }
        startJobTracking(jobId);

        const finalStatus = await pollUntilTerminal(jobId);
        const finalState = finalStatus.state;

        if (finalState === 'completed') {
            if (kind === 'import') {
                const sourceUsers = finalStatus.result.source_users;
                const targetUser = finalStatus.result.target_user;
                const userSummary = sourceUsers.join(', ');

                toastr.success(
                    t`Imported users: ${userSummary}. Migrated target: ${targetUser}. Reloading...`,
                    t`Data import completed`,
                    { timeOut: 6000 },
                );
                setStatusText(t`Import completed`);

                reloadSoon();
            } else {
                const saveResult = await saveExportArchive(jobId);
                if (saveResult.mode === 'ios-native-share') {
                    if (saveResult.cleanupError) {
                        toastr.warning(saveResult.cleanupError, t`Export cleanup failed`);
                    }

                    if (saveResult.completed) {
                        toastr.success(t`Data archive is ready to share/save`, t`Export completed`, { timeOut: 8000 });
                        setStatusText(t`Export completed`);
                    } else {
                        toastr.info(t`Sharing cancelled`, t`Export cancelled`);
                        setStatusText(t`Export cancelled`);
                    }

                    return;
                }

                if (saveResult.mode === 'mobile-native') {
                    if (saveResult.cleanupError) {
                        toastr.warning(saveResult.cleanupError, t`Export cleanup failed`);
                    }
                    if (saveResult.cancelled) {
                        toastr.info(t`Export cancelled`);
                        setStatusText(t`Export cancelled`);
                        return;
                    }
                }

                const savedPath = saveResult.savedPath;
                if (savedPath) {
                    jobState.lastExportSavedPath = savedPath;
                    refreshExportActions();
                    toastr.success(t`Data archive saved: ${savedPath}`, t`Export completed`, { timeOut: 8000 });
                    setStatusText(t`Export completed | ${savedPath}`);
                } else {
                    toastr.success(t`Data archive saved`, t`Export completed`);
                    setStatusText(t`Export completed`);
                }
            }
            return;
        }

        if (finalState === 'cancelled') {
            if (kind === 'import' && importStatusRequiresReload(finalStatus)) {
                const message = importTerminalMessage(finalStatus, t`Import cancelled after updating local data`);
                toastr.warning(`${message}. ${t`Reloading...`}`, t`Data import cancelled`, { timeOut: 6000 });
                setStatusText(message);
                reloadSoon();
                return;
            }

            toastr.info(t`Migration job cancelled`);
            setStatusText(t`Job cancelled`);
            return;
        }

        if (kind === 'import' && importStatusRequiresReload(finalStatus)) {
            const message = importTerminalMessage(finalStatus, t`Data import failed after updating local data`);
            toastr.error(`${message}. ${t`Reloading...`}`, failureTitle, { timeOut: 8000 });
            setStatusText(message);
            reloadSoon();
            return;
        }

        throw new Error(finalStatus.error || t`Unknown error`);
    } catch (error) {
        const failureMessage = normalizeCaughtError(error);
        toastr.error(failureMessage, failureTitle);
        setStatusText(failureMessage);
    } finally {
        stopJobTracking();
    }
}

async function onAndroidImportButtonClick() {
    await runConfirmedImport(async () => {
        const contentUri = await pickAndroidImportArchive();
        return startImportJobFromAndroidContentUri(contentUri);
    });
}

async function onIosImportButtonClick() {
    await runConfirmedImport(() => startImportJobFromIosPicker());
}

async function runConfirmedImport(startJob) {
    const prompt = t`Importing will merge into the TauriTavern data directory (same-path files will be overwritten). Continue?`;
    const confirmed = isIosRuntime() || isWebRuntime()
        ? await Popup.show.confirm(t`Confirm data import`, prompt) === POPUP_RESULT.AFFIRMATIVE
        : window.confirm(prompt);
    if (!confirmed) {
        return;
    }

    toastr.info(t`Importing data archive...`);
    await runMigrationJob('import', startJob);
}

async function onExportClick() {
    if (hasActiveJob()) {
        toastr.warning(t`A migration job is already running`);
        return;
    }

    toastr.info(t`Exporting data archive...`);
    setStatusText(t`Preparing export...`);
    await runMigrationJob('export', startExportJob);
}

function sleep(ms) {
    return new Promise((resolve) => setTimeout(resolve, ms));
}

jQuery(async () => {
    const html = await renderExtensionTemplateAsync(MODULE_NAME, 'settings');
    $('#data_migration_container').append(html);
    refreshControls();

    const iosPolicy = getActiveIosPolicyActivationReport();
    if (iosPolicy?.profile === 'ios_external_beta') {
        const description = document.querySelector(`#data_migration_settings .extensions_info[data-i18n="${CSS.escape(SILLYTAVERN_MIGRATION_COPY_KEY)}"]`);
        if (!(description instanceof HTMLElement)) {
            throw new Error('[TauriTavern][iOSPolicy] Data migration description element not found');
        }

        description.dataset.i18n = TAURITAVERN_MIGRATION_COPY_KEY;
        description.textContent = translate(TAURITAVERN_MIGRATION_COPY_KEY);
    }

    $('#data_migration_import_button').on('click', onImportButtonClick);
    if (isWebRuntime()) {
        $('#data_migration_import_button').hide();
        $('#data_migration_import_local_button, #data_migration_import_server_button, #data_migration_web_import_tip').show();
        $('#data_migration_import_local_button').on('click', onWebImportFromComputer);
        $('#data_migration_import_server_button').on('click', onWebImportFromServer);
    }
    $('#data_migration_export_button').on('click', onExportClick);
    $('#data_migration_cancel_button').on('click', requestCancelActiveJob);
    $('#data_migration_reveal_export_button').on('click', onRevealExportClick);
});
