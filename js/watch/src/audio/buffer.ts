import { Time } from "@moq/net";
import { Effect, type Getter, Signal } from "@moq/signals";
import type { Data, InitPost, InitShared, Latency, Reset, State } from "./render";
import { allocSharedRingBuffer, SharedRingBuffer } from "./shared-ring-buffer";

// Ring capacity (ms) when buffering is uncapped. The decoded PCM ring stays small; the
// real lookahead lives upstream as encoded frames. On the SharedArrayBuffer path `wait()`
// applies backpressure (holding encoded frames) once decoding would run this far ahead.
const UNCAPPED_RING_MS = 1500 as Time.Milli;

// Headroom below capacity so a frame mid-decode doesn't overflow the small ring.
const RING_MARGIN_MS = 250 as Time.Milli;

/**
 * Timestamp-based backpressure for the uncapped buffered ring. `wait(timestamp)` stays pending
 * until the playhead reaches `timestamp - (capacity - margin)`, so the decoder holds a frame (as
 * encoded Opus) instead of decoding it too far ahead of the small PCM ring. Both transports share
 * this; they differ only in how they observe the playhead (Atomics poll vs worklet state messages).
 */
class Backpressure {
	readonly #enabled: boolean;
	readonly #headroom: Time.Micro;
	#waiters: Array<{ threshold: Time.Micro; resolve: () => void }> = [];

	constructor(enabled: boolean, capacityMs: Time.Milli | undefined) {
		this.#enabled = enabled;
		const headroomMs = Math.max(0, (capacityMs ?? 0) - RING_MARGIN_MS) as Time.Milli;
		this.#headroom = Time.Micro.fromMilli(headroomMs);
	}

	wait(timestamp: Time.Micro, playhead: Time.Micro): Promise<void> {
		if (!this.#enabled) return Promise.resolve();
		const threshold = (timestamp - this.#headroom) as Time.Micro;
		if (playhead >= threshold) return Promise.resolve();
		return new Promise((resolve) => this.#waiters.push({ threshold, resolve }));
	}

	// Resolve every waiter the playhead has reached.
	advance(playhead: Time.Micro): void {
		if (this.#waiters.length === 0) return;
		this.#waiters = this.#waiters.filter(({ threshold, resolve }) => {
			if (playhead < threshold) return true;
			resolve();
			return false;
		});
	}

	// Resolve everything unconditionally (reset/close): never strand a decode loop.
	flush(): void {
		for (const { resolve } of this.#waiters) resolve();
		this.#waiters = [];
	}
}

/**
 * Unified interface for the audio buffer between the main thread and the AudioWorklet.
 *
 * Two implementations exist:
 *   - `SharedAudioBuffer`: backed by SharedArrayBuffer, lock-free writes via Atomics.
 *   - `PostAudioBuffer`: backed by postMessage transfer (the fallback when SAB is unavailable).
 *
 * Use `createAudioBuffer()` to pick the right implementation automatically.
 */
export interface AudioBuffer {
	readonly rate: number;
	readonly channels: number;

	/** Insert audio samples at the given timestamp. Handles out-of-order writes. */
	insert(timestamp: Time.Micro, data: Float32Array[]): void;

	/** Update the target latency in samples. */
	setLatency(samples: number): void;

	/** Flush buffered samples and re-stall, ready to anchor the next utterance (buffered mode). */
	reset(): void;

	/**
	 * Resolve once there's room to insert a frame at `timestamp`. In uncapped buffered mode this
	 * applies backpressure: it stays pending while decoding `timestamp` would run too far ahead of
	 * the playhead for the small ring, so the caller holds the (encoded) frame instead of decoding
	 * it. Resolves immediately when capped or live (the ring bounds itself).
	 */
	wait(timestamp: Time.Micro): Promise<void>;

	/** Current playback timestamp (derived from reader position). */
	readonly timestamp: Getter<Time.Micro>;

	/** Whether the buffer is stalled (waiting to fill). */
	readonly stalled: Getter<boolean>;

	/** Release any resources (event listeners, intervals, etc.). */
	close(): void;
}

/** Returns true when SharedArrayBuffer is available and usable in the current context. */
export function supportsSharedArrayBuffer(): boolean {
	if (typeof SharedArrayBuffer === "undefined") return false;
	// In browsers, SharedArrayBuffer requires cross-origin isolation (COOP/COEP).
	// crossOriginIsolated is a browser global; in Node/Bun it's undefined.
	if (typeof crossOriginIsolated !== "undefined" && !crossOriginIsolated) return false;
	return true;
}

/**
 * Create the best audio buffer implementation for the current environment.
 * Picks `SharedAudioBuffer` when possible, falling back to `PostAudioBuffer`.
 */
export function createAudioBuffer(
	worklet: AudioWorkletNode,
	channels: number,
	rate: number,
	latencySamples: number,
	buffered = false,
	// undefined = uncapped (only meaningful when buffered).
	maxBuffer?: Time.Milli,
): AudioBuffer {
	if (supportsSharedArrayBuffer()) {
		console.log("[audio] using SharedArrayBuffer audio buffer");
		return new SharedAudioBuffer(worklet, channels, rate, latencySamples, buffered, maxBuffer);
	}
	console.log("[audio] using postMessage audio buffer (SharedArrayBuffer unavailable)");
	return new PostAudioBuffer(worklet, channels, rate, latencySamples, buffered, maxBuffer);
}

/** SharedArrayBuffer-backed implementation. Writes go directly into shared memory. */
class SharedAudioBuffer implements AudioBuffer {
	readonly rate: number;
	readonly channels: number;
	#worklet: AudioWorkletNode;
	#ring: SharedRingBuffer;

	readonly #timestamp = new Signal<Time.Micro>(0 as Time.Micro);
	readonly timestamp: Getter<Time.Micro> = this.#timestamp;

	readonly #stalled = new Signal<boolean>(true);
	readonly stalled: Getter<boolean> = this.#stalled;

	#backpressure: Backpressure;

	#signals = new Effect();

	constructor(
		worklet: AudioWorkletNode,
		channels: number,
		rate: number,
		latencySamples: number,
		buffered: boolean,
		maxBuffer?: Time.Milli,
	) {
		this.#worklet = worklet;
		this.channels = channels;
		this.rate = rate;

		// Buffered + capped: capacity is the cap (drops the oldest beyond it; no backpressure).
		// Buffered + uncapped: a small ring with backpressure via `wait()`; lookahead stays encoded.
		// Not buffered: just headroom above LATENCY.
		const capacityMs = buffered ? (maxBuffer ?? UNCAPPED_RING_MS) : undefined;
		const capacity =
			capacityMs !== undefined
				? Math.ceil(rate * Time.Second.fromMilli(capacityMs))
				: Math.max(rate, latencySamples * 2);
		this.#backpressure = new Backpressure(buffered && maxBuffer === undefined, capacityMs);

		const init = allocSharedRingBuffer(channels, capacity, rate, buffered);
		this.#ring = new SharedRingBuffer(init);
		this.#ring.setLatency(latencySamples);

		const msg: InitShared = { type: "init-shared", ...init };
		worklet.port.postMessage(msg);

		// Poll the shared control array and reflect it into signals.
		this.#signals.interval(() => {
			this.#timestamp.set(this.#ring.timestamp);
			this.#stalled.set(this.#ring.stalled);
			this.#backpressure.advance(this.#ring.timestamp);
		}, 50);
	}

	insert(timestamp: Time.Micro, data: Float32Array[]): void {
		this.#ring.insert(timestamp, data);
	}

	setLatency(samples: number): void {
		// Grow the ring (preserving the unread window) if it's too small for the new latency.
		if (this.#ring.capacity < samples * 1.5) {
			const newCapacity = Math.max(this.rate, samples * 2);
			this.#ring = this.#ring.resize(newCapacity);
			this.#ring.setLatency(samples);

			const msg: InitShared = { type: "init-shared", ...this.#ring.init };
			this.#worklet.port.postMessage(msg);
		} else {
			this.#ring.setLatency(samples);
		}
	}

	reset(): void {
		this.#ring.reset();
		this.#backpressure.flush(); // the old timeline is gone; let the decode loop re-anchor
	}

	wait(timestamp: Time.Micro): Promise<void> {
		return this.#backpressure.wait(timestamp, this.#ring.timestamp);
	}

	close(): void {
		this.#backpressure.flush(); // never leave a decode loop awaiting a closed buffer
		this.#signals.close();
	}
}

/** postMessage-backed fallback implementation. Samples are transferred, not shared. */
class PostAudioBuffer implements AudioBuffer {
	readonly rate: number;
	readonly channels: number;
	#worklet: AudioWorkletNode;

	readonly #timestamp = new Signal<Time.Micro>(0 as Time.Micro);
	readonly timestamp: Getter<Time.Micro> = this.#timestamp;

	readonly #stalled = new Signal<boolean>(true);
	readonly stalled: Getter<boolean> = this.#stalled;

	// Backpressure runs off the playhead the worklet reports in its state messages.
	#backpressure: Backpressure;

	#signals = new Effect();

	constructor(
		worklet: AudioWorkletNode,
		channels: number,
		rate: number,
		latencySamples: number,
		buffered: boolean,
		maxBuffer?: Time.Milli,
	) {
		this.#worklet = worklet;
		this.channels = channels;
		this.rate = rate;

		const capacityMs = buffered ? (maxBuffer ?? UNCAPPED_RING_MS) : undefined;
		this.#backpressure = new Backpressure(buffered && maxBuffer === undefined, capacityMs);

		const latency = Time.Milli.fromSecond((latencySamples / rate) as Time.Second);
		const msg: InitPost = { type: "init-post", channels, rate, latency, buffered, maxBuffer };
		worklet.port.postMessage(msg);

		// Listen for state updates from the worklet.
		this.#signals.event(worklet.port, "message", (ev: Event) => {
			const data = (ev as MessageEvent<State>).data;
			if (data?.type === "state") {
				this.#timestamp.set(data.timestamp);
				this.#stalled.set(data.stalled);
				this.#backpressure.advance(data.timestamp);
			}
		});
		// addEventListener on a MessagePort requires start() to begin delivery.
		worklet.port.start();
	}

	insert(timestamp: Time.Micro, data: Float32Array[]): void {
		const msg: Data = { type: "data", data, timestamp };
		// Transfer the ArrayBuffers to avoid a copy. This is why samples can be dropped
		// under load: the main thread loses access until the worklet drains the message queue.
		this.#worklet.port.postMessage(
			msg,
			data.map((d) => d.buffer),
		);
	}

	setLatency(samples: number): void {
		const latency = Time.Milli.fromSecond((samples / this.rate) as Time.Second);
		const msg: Latency = { type: "latency", latency };
		this.#worklet.port.postMessage(msg);
	}

	reset(): void {
		const msg: Reset = { type: "reset" };
		this.#worklet.port.postMessage(msg);
		this.#backpressure.flush(); // the old timeline is gone; let the decode loop re-anchor
	}

	wait(timestamp: Time.Micro): Promise<void> {
		// Uses the worklet-reported playhead, which lags by a state-message interval; the ring
		// margin covers that. The worklet still drops the oldest if a frame slips through.
		return this.#backpressure.wait(timestamp, this.#timestamp.peek());
	}

	close(): void {
		this.#backpressure.flush(); // never leave a decode loop awaiting a closed buffer
		this.#signals.close();
	}
}
