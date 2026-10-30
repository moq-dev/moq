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

import type { Resources } from "./contract";

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

// ── the speakers ────────────────────────────────────────────────────────────

// One analyser per context, fed by everything connected to that context's destination: what the
// speakers would play. The player's volume and mute live in a private GainNode between its graph
// root and the destination, so only a tap at the destination itself can see them.
const speakers = new WeakMap<BaseAudioContext, AnalyserNode>();

/** Samples per speaker reading: the same ~43ms window the probe's tone analyser reads. */
const SPEAKER_WINDOW = 2048;

/** An analyser carrying the sum of everything connected to `context.destination`, or undefined before anything was. */
export function speaker(context: BaseAudioContext): AnalyserNode | undefined {
	return speakers.get(context);
}

const RealNode = globalThis.AudioNode;
if (RealNode) {
	// Untyped, since both are overloaded on AudioNode and AudioParam targets.
	type Untyped = (this: AudioNode, ...args: unknown[]) => unknown;
	const connect = RealNode.prototype.connect as Untyped;
	const disconnect = RealNode.prototype.disconnect as Untyped;

	const tap = (context: BaseAudioContext) => {
		let analyser = speakers.get(context);
		if (!analyser) {
			analyser = new AnalyserNode(context, { fftSize: SPEAKER_WINDOW, smoothingTimeConstant: 0 });
			speakers.set(context, analyser);
		}
		return analyser;
	};

	// Mirror each connection to a destination onto the tap. A full disconnect() drops the mirror
	// with every other output; only a disconnect aimed at the destination needs mirroring back.
	RealNode.prototype.connect = function (this: AudioNode, ...args: unknown[]) {
		const result = connect.apply(this, args);
		if (args[0] instanceof AudioDestinationNode) connect.call(this, tap(this.context), args[1] ?? 0);
		return result;
	} as typeof RealNode.prototype.connect;

	RealNode.prototype.disconnect = function (this: AudioNode, ...args: unknown[]) {
		disconnect.apply(this, args);
		if (args[0] instanceof AudioDestinationNode) {
			const analyser = speakers.get(this.context);
			if (analyser) {
				try {
					disconnect.call(this, analyser);
				} catch {
					// Never mirrored, so there is nothing to drop.
				}
			}
		}
	} as typeof RealNode.prototype.disconnect;
}
