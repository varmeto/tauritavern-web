// @ts-check

import { invoke } from '../../../tauri-bridge.js';

export async function waitForBackendReady() {
    await invoke('wait_for_backend_ready');
}
