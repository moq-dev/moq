import { Time } from "@moq/net";
import { Effect, type Getter, Signal } from "@moq/signals";
import type { Data, InitPost, InitShared, Latency, Reset, State } from "./render";
import { allocSharedRingBuffer, SharedRingBuffer } from "./shared-ring-buffer";

/**
 * Timestamp-based backpressure for buffered playback. The decoded PCM ring only holds the latency
 * floor; everything above it (the buffered lookahead, up to the ceiling) stays upstream as encoded
 * Opus. `wait(timestamp)` stays pending until the playhead is within `headroom` (the floor) of
 * `timestamp`, so the decode loop holds a frame as Opus instead of decoding it too far ahead of the
 * floor-sized ring. Both transports share this; they differ only in how they observe the playhead
 * (Atomics poll vs worklet state messages). A no-op when not buffered (the ring bounds itself).
 */
class Backpressure {
	readonly #enabled: boolean;
	#headroom: Time.Micro;
	#waiters: Array<{ timestamp: Time.Micro; resolve: () => void }> = [];

	constructor(enabled: boolean, headroom: Time.Micro) {
		this.#enabled = enabled;
		this.#headroom = headroom;
	}

	// Move the gate as the floor changes (e.g. "real-time" jitter tracking RTT).
	setHeadroom(headroom: Time.Micro): void {
		this.#headroom = headroom;
	}

	wait(timestamp: Time.Micro, playhead: Time.Micro): Promise<void> {
		if (!this.#enabled) return Promise.resolve();
		if (playhead >= ((timestamp - this.#headroom) | 0)) return Promise.resolve();
		return new Promise((resolve) => this.#waiters.push({ timestamp, resolve }));
	}

	// Resolve every waiter the playhead has reached. Thresholds are recomputed live so a changed
	// headroom takes effect on queued waiters too.
	advance(playhead: Time.Micro): void {
		if (this.#waiters.length === 0) return;
		this.#waiters = this.#waiters.filter(({ timestamp, resolve }) => {
			if (playhead < ((timestamp - this.#headroom) | 0)) return true;
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

/** Convert a sample count to a Time.Micro duration at the given sample rate. */
function samplesToMicro(samples: number, rate: number): Time.Micro {
	return Time.Micro.fromSecond((samples / rate) as Time.Second);
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
	 * Resolve once the playhead is near enough to decode a frame at `timestamp`. In buffered mode this
	 * applies backpressure: it stays pending while decoding `timestamp` would run more than the latency
	 * floor ahead of the playhead, so the caller holds the (encoded) frame instead of decoding it too
	 * far ahead of the floor-sized ring. Resolves immediately when not buffered (the ring bounds itself).
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
): AudioBuffer {
	if (supportsSharedArrayBuffer()) {
		console.log("[audio] using SharedArrayBuffer audio buffer");
		return new SharedAudioBuffer(worklet, channels, rate, latencySamples, buffered);
	}
	console.log("[audio] using postMessage audio buffer (SharedArrayBuffer unavailable)");
	return new PostAudioBuffer(worklet, channels, rate, latencySamples, buffered);
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

	constructor(worklet: AudioWorkletNode, channels: number, rate: number, latencySamples: number, buffered: boolean) {
		this.#worklet = worklet;
		this.channels = channels;
		this.rate = rate;

		// The ring holds the latency floor as decoded PCM (headroom above it for overflow). In
		// buffered mode the lookahead above the floor stays encoded upstream, held back by `wait()`.
		const capacity = Math.max(rate, latencySamples * 2);
		this.#backpressure = new Backpressure(buffered, samplesToMicro(latencySamples, rate));

		const init = allocSharedRingBuffer(channels, capacity, rate, buffered);
		this.#ring = new SharedRingBuffer(init);
		this.#ring.setLatency(latencySamples);

		const msg: InitShared = { type: "init-shared", ...init };
		worklet.port.postMessage(msg);

		// Poll the shared control array and reflect it into signals.
		this.#signals.interval(() => {
			const stalled = this.#ring.stalled;
			this.#timestamp.set(this.#ring.timestamp);
			this.#stalled.set(stalled);
			// While stalled the playhead is parked, so release the decode loop to refill the floor;
			// once playing, hold it to ~the floor ahead.
			if (stalled) this.#backpressure.flush();
			else this.#backpressure.advance(this.#ring.timestamp);
		}, 50);
	}

	insert(timestamp: Time.Micro, data: Float32Array[]): void {
		this.#ring.insert(timestamp, data);
	}

	setLatency(samples: number): void {
		this.#backpressure.setHeadroom(samplesToMicro(samples, this.rate));

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
		// Stalled = still filling the floor (bootstrap or underflow): let frames through to refill.
		if (this.#ring.stalled) return Promise.resolve();
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

	constructor(worklet: AudioWorkletNode, channels: number, rate: number, latencySamples: number, buffered: boolean) {
		this.#worklet = worklet;
		this.channels = channels;
		this.rate = rate;

		this.#backpressure = new Backpressure(buffered, samplesToMicro(latencySamples, rate));

		const latency = Time.Milli.fromSecond((latencySamples / rate) as Time.Second);
		const msg: InitPost = { type: "init-post", channels, rate, latency, buffered };
		worklet.port.postMessage(msg);

		// Listen for state updates from the worklet.
		this.#signals.event(worklet.port, "message", (ev: Event) => {
			const data = (ev as MessageEvent<State>).data;
			if (data?.type === "state") {
				this.#timestamp.set(data.timestamp);
				this.#stalled.set(data.stalled);
				// While stalled the playhead is parked, so release the decode loop to refill the floor;
				// once playing, hold it to ~the floor ahead.
				if (data.stalled) this.#backpressure.flush();
				else this.#backpressure.advance(data.timestamp);
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
		this.#backpressure.setHeadroom(samplesToMicro(samples, this.rate));

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
		// Stalled = still filling the floor (bootstrap or underflow): let frames through to refill.
		if (this.#stalled.peek()) return Promise.resolve();
		// Uses the worklet-reported playhead, which lags by a state-message interval; the floor's
		// headroom covers that. The worklet still drops the oldest if a frame slips through.
		return this.#backpressure.wait(timestamp, this.#timestamp.peek());
	}

	close(): void {
		this.#backpressure.flush(); // never leave a decode loop awaiting a closed buffer
		this.#signals.close();
	}
}
