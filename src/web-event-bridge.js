const subscriptions = new Set();

if (typeof window !== 'undefined' && window.__TAURITAVERN_WEB__ === true) {
    window.__TT_WEB_EVENTS__ = {
        reconnect() {
            for (const reconnect of subscriptions) reconnect();
        },
    };
}

export function listenWebEvent(eventName, handler) {
    if (typeof eventName !== 'string' || typeof handler !== 'function') {
        throw new Error('Invalid web event subscription');
    }

    return new Promise((resolve, reject) => {
        let socket = null;
        let stopped = false;
        let ready = false;
        let retryTimer = null;
        let retryCount = 0;
        let generation = 0;
        const unlisten = () => {
            stopped = true;
            generation += 1;
            clearTimeout(retryTimer);
            subscriptions.delete(reconnect);
            socket?.close(1000, 'unlisten');
        };
        const connect = () => {
            if (stopped) return;
            const current = ++generation;
            const url = new URL('/__tt/events', window.location.href);
            url.protocol = url.protocol === 'https:' ? 'wss:' : 'ws:';
            url.searchParams.set('event', eventName);
            socket = new WebSocket(url);
            socket.addEventListener('open', () => {
                if (current !== generation) return;
                retryCount = 0;
                if (!ready) {
                    ready = true;
                    resolve(unlisten);
                }
            });
            socket.addEventListener('message', (message) => {
                if (current !== generation) return;
                try { handler(JSON.parse(message.data)); }
                catch (error) { console.error(`Invalid event payload for ${eventName}`, error); }
            });
            socket.addEventListener('close', () => {
                if (stopped || current !== generation) return;
                const delay = Math.min(5000, 400 * 2 ** Math.min(retryCount++, 4));
                retryTimer = setTimeout(connect, delay);
            });
        };
        const reconnect = () => {
            if (stopped) return;
            clearTimeout(retryTimer);
            socket?.close(1000, 'reconnect');
            connect();
        };
        subscriptions.add(reconnect);
        connect();
        setTimeout(() => {
            if (!ready) {
                unlisten();
                reject(new Error(`Unable to subscribe to ${eventName}`));
            }
        }, 10000);
    });
}
