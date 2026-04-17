import { Time } from "@moq/lite";
import { Effect, type Getter, Signal } from "@moq/signals";
import type { Data, InitPost, InitShared, Latency, State } from "./render";
import { allocSharedRingBuffer, SharedRingBuffer } from "./shared-ring-buffer";

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
): AudioBuffer {
	if (supportsSharedArrayBuffer()) {
		console.log("[audio] using SharedArrayBuffer audio buffer");
		return new SharedAudioBuffer(worklet, channels, rate, latencySamples);
	}
	console.log("[audio] using postMessage audio buffer (SharedArrayBuffer unavailable)");
	return new PostAudioBuffer(worklet, channels, rate, latencySamples);
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

	#signals = new Effect();

	constructor(worklet: AudioWorkletNode, channels: number, rate: number, latencySamples: number) {
		this.#worklet = worklet;
		this.channels = channels;
		this.rate = rate;

		// Capacity needs headroom above LATENCY for overflow protection.
		const capacity = Math.max(rate, latencySamples * 2);
		const init = allocSharedRingBuffer(channels, capacity, rate);
		this.#ring = new SharedRingBuffer(init);
		this.#ring.setLatency(latencySamples);

		const msg: InitShared = { type: "init-shared", ...init };
		worklet.port.postMessage(msg);

		// Poll the shared control array and reflect it into signals.
		this.#signals.interval(() => {
			this.#timestamp.set(this.#ring.timestamp);
			this.#stalled.set(this.#ring.stalled);
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

	close(): void {
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

	#signals = new Effect();

	constructor(worklet: AudioWorkletNode, channels: number, rate: number, latencySamples: number) {
		this.#worklet = worklet;
		this.channels = channels;
		this.rate = rate;

		const latency = Time.Milli.fromSecond((latencySamples / rate) as Time.Second);
		const msg: InitPost = { type: "init-post", channels, rate, latency };
		worklet.port.postMessage(msg);

		// Listen for state updates from the worklet.
		this.#signals.event(worklet.port, "message", (ev: Event) => {
			const data = (ev as MessageEvent<State>).data;
			if (data?.type === "state") {
				this.#timestamp.set(data.timestamp);
				this.#stalled.set(data.stalled);
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

	close(): void {
		this.#signals.close();
	}
}
