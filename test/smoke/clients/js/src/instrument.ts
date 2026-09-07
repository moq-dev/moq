/**
 * Counts the platform resources the player holds, so a lifecycle test can prove they go away.
 *
 * "The session closed" is not observable from the player's own API: an element that leaks a
 * transport or an audio graph looks identical from the outside to one that tore them down. These
 * counters wrap the constructors the page itself calls, before any element is built, and report how
 * many of each are still alive. A detach that returns every count to zero is the baseline; anything
 * left over is a leak.
 *
 * Importing this module installs the wrappers, so it must be the first import on the page.
 *
 * @module
 */

/** Live instances of each wrapped resource. */
export type Resources = {
	/** Open {@link WebTransport} sessions, i.e. connections to the relay. */
	transports: number;
	/** Open {@link WebSocket}s: the transport a connection falls back to when QUIC loses the race. */
	sockets: number;
	/** Unclosed {@link AudioContext}s, i.e. audio graphs and their render threads. */
	audioContexts: number;
	/** Unterminated {@link Worker}s. */
	workers: number;
};

const live: Resources = { transports: 0, sockets: 0, audioContexts: 0, workers: 0 };

/** How many of each wrapped resource the page currently holds. */
export function resources(): Resources {
	return { ...live };
}

// Decrement at most once per instance: close() is idempotent, and a context also reports "closed"
// through statechange, so both paths can fire for the same object.
function once(release: () => void): () => void {
	let done = false;
	return () => {
		if (done) return;
		done = true;
		release();
	};
}

const RealTransport = globalThis.WebTransport;
if (RealTransport) {
	globalThis.WebTransport = class extends RealTransport {
		constructor(...args: ConstructorParameters<typeof RealTransport>) {
			super(...args);
			live.transports++;
			const release = once(() => {
				live.transports--;
			});
			this.closed.then(release, release);
		}
	};
}

const RealWebSocket = globalThis.WebSocket;
if (RealWebSocket) {
	globalThis.WebSocket = class extends RealWebSocket {
		constructor(...args: ConstructorParameters<typeof RealWebSocket>) {
			super(...args);
			live.sockets++;
			const release = once(() => {
				live.sockets--;
			});
			this.addEventListener("close", release);
			this.addEventListener("error", release);
		}
	};
}

const RealAudioContext = globalThis.AudioContext;
if (RealAudioContext) {
	globalThis.AudioContext = class extends RealAudioContext {
		readonly #release: () => void;

		constructor(...args: ConstructorParameters<typeof RealAudioContext>) {
			super(...args);
			live.audioContexts++;
			this.#release = once(() => {
				live.audioContexts--;
			});
			// Chrome flips state synchronously in close(), but the event is the only signal for a
			// context the browser tears down on its own.
			this.addEventListener("statechange", () => {
				if (this.state === "closed") this.#release();
			});
		}

		override async close(): Promise<void> {
			try {
				await super.close();
			} finally {
				this.#release();
			}
		}
	};
}

const RealWorker = globalThis.Worker;
if (RealWorker) {
	globalThis.Worker = class extends RealWorker {
		readonly #release: () => void;

		constructor(...args: ConstructorParameters<typeof RealWorker>) {
			super(...args);
			live.workers++;
			this.#release = once(() => {
				live.workers--;
			});
		}

		override terminate(): void {
			super.terminate();
			this.#release();
		}
	};
}
