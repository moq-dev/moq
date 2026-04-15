import { Time } from "@moq/lite";
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
	readonly timestamp: Time.Micro;

	/** Whether the buffer is stalled (waiting to fill). */
	readonly stalled: boolean;

	/** Release any resources (event listeners, etc.). */
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
	#buffer: SharedRingBuffer;

	constructor(worklet: AudioWorkletNode, channels: number, rate: number, latencySamples: number) {
		this.#worklet = worklet;
		this.channels = channels;
		this.rate = rate;

		// Capacity needs headroom above LATENCY for overflow protection.
		const capacity = Math.max(rate, latencySamples * 2);
		const init = allocSharedRingBuffer(channels, capacity, rate);
		this.#buffer = new SharedRingBuffer(init);
		this.#buffer.setLatency(latencySamples);

		const msg: InitShared = { type: "init-shared", ...init };
		worklet.port.postMessage(msg);
	}

	get timestamp(): Time.Micro {
		return this.#buffer.timestamp;
	}

	get stalled(): boolean {
		return this.#buffer.stalled;
	}

	insert(timestamp: Time.Micro, data: Float32Array[]): void {
		this.#buffer.insert(timestamp, data);
	}

	setLatency(samples: number): void {
		// Re-allocate if the current buffer is too small for the new target latency.
		if (this.#buffer.capacity < samples * 1.5) {
			const newCapacity = Math.max(this.rate, samples * 2);
			const init = allocSharedRingBuffer(this.channels, newCapacity, this.rate);
			this.#buffer = new SharedRingBuffer(init);
			this.#buffer.setLatency(samples);

			const msg: InitShared = { type: "init-shared", ...init };
			this.#worklet.port.postMessage(msg);
		} else {
			this.#buffer.setLatency(samples);
		}
	}

	close(): void {
		// Nothing to clean up — SABs are garbage collected.
	}
}

/** postMessage-backed fallback implementation. Samples are transferred, not shared. */
class PostAudioBuffer implements AudioBuffer {
	readonly rate: number;
	readonly channels: number;
	#worklet: AudioWorkletNode;
	#timestamp: Time.Micro = 0 as Time.Micro;
	#stalled = true;
	#onMessage: (ev: MessageEvent<State>) => void;

	constructor(worklet: AudioWorkletNode, channels: number, rate: number, latencySamples: number) {
		this.#worklet = worklet;
		this.channels = channels;
		this.rate = rate;

		const latency = Time.Milli.fromSecond((latencySamples / rate) as Time.Second);
		const msg: InitPost = { type: "init-post", channels, rate, latency };
		worklet.port.postMessage(msg);

		// Listen for state updates from the worklet.
		this.#onMessage = (ev) => {
			if (ev.data?.type === "state") {
				this.#timestamp = ev.data.timestamp;
				this.#stalled = ev.data.stalled;
			}
		};
		worklet.port.addEventListener("message", this.#onMessage as EventListener);
		worklet.port.start();
	}

	get timestamp(): Time.Micro {
		return this.#timestamp;
	}

	get stalled(): boolean {
		return this.#stalled;
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
		this.#worklet.port.removeEventListener("message", this.#onMessage as EventListener);
	}
}
