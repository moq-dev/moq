import { Micro, Milli, Nano, type Second } from "@moq/clock";
import { Effect, type Getter, Signal } from "@moq/signals";
import type { Data, InitPost, InitShared, Latency, Reset, State, Truncate } from "./render";
import { allocSharedRingBuffer, SharedRingBuffer } from "./shared-ring-buffer";

/**
 * Timestamp-based backpressure for buffered playback. The decoded PCM ring only holds the latency
 * floor; everything above it (the buffered lookahead, up to the ceiling) stays upstream as encoded
 * Opus. `wait(timestamp)` stays pending until decoding `timestamp` would land within `headroom` (the
 * floor) of the playhead, so the decode loop holds a frame as Opus instead of overflowing the
 * floor-sized ring, which drops the oldest unplayed audio. Both transports share this; they differ
 * only in how they observe the ring (Atomics poll vs worklet state messages). A no-op when not
 * buffered (the ring bounds itself).
 */
class Backpressure {
	#enabled: boolean;
	#headroom: Micro;
	#waiters: Array<{ timestamp: Micro; resolve: () => void }> = [];

	// The ring as last observed. The playhead is undefined until one is known on the current timeline.
	#playhead?: Micro;
	#stalled = true;

	// The first and latest frames released since the ring stalled. A stalled ring refills to the
	// floor from its parked playhead, but releasing it outright would let the whole lookahead through
	// before the decoder's output reaches the ring and un-stalls it.
	#first?: Micro;
	#last?: Micro;

	constructor(enabled: boolean, headroom: Micro) {
		this.#enabled = enabled;
		this.#headroom = headroom;
	}

	// Move the gate as the floor changes (e.g. "auto" tracking the measured arrival spread).
	setHeadroom(headroom: Micro): void {
		this.#headroom = headroom;
	}

	wait(timestamp: Micro, signal: AbortSignal): Promise<void> {
		if (!this.#enabled || signal.aborted || this.#admit(timestamp)) return Promise.resolve();
		return new Promise((resolve) => {
			// An aborted wait drops its waiter, so a retired decode loop leaves nothing queued here.
			const abort = () => {
				this.#waiters = this.#waiters.filter((w) => w !== waiter);
				resolve();
			};
			const waiter = {
				timestamp,
				resolve: () => {
					signal.removeEventListener("abort", abort);
					resolve();
				},
			};
			signal.addEventListener("abort", abort, { once: true });
			this.#waiters.push(waiter);
		});
	}

	// Record the ring's playhead and stall state, and resolve every waiter that now fits.
	observe(playhead: Micro | undefined, stalled: boolean): void {
		this.#playhead = playhead;
		this.#stalled = stalled;
		if (!stalled) {
			this.#first = undefined;
			this.#last = undefined;
		}
		this.#release();
	}

	// The ring was flushed for a new timeline: its playhead is unknown until the next frame anchors it.
	reset(): void {
		this.#playhead = undefined;
		this.#stalled = true;
		this.#first = undefined;
		this.#last = undefined;
		this.#release();
	}

	// Stop gating for good. Nothing advances a closed buffer, so a later wait would never settle.
	close(): void {
		this.#enabled = false;
		for (const { resolve } of this.#waiters) resolve();
		this.#waiters = [];
	}

	// Thresholds are recomputed live so a changed headroom takes effect on queued waiters too.
	#release(): void {
		this.#waiters = this.#waiters.filter(({ timestamp, resolve }) => {
			if (!this.#admit(timestamp)) return true;
			resolve();
			return false;
		});
	}

	#admit(timestamp: Micro): boolean {
		const playhead = this.#playhead;
		if (!this.#stalled && playhead !== undefined) return timestamp - this.#headroom <= playhead;

		// Refill from the parked playhead, or from the first frame while the ring has not anchored. The
		// first frame always goes, so a ring that needs it to anchor, or to see past a gap, never waits
		// on a playhead that will not move. Frames keep going until one reaches the floor: the ring
		// only un-stalls once it holds that much, and decoded audio can land a little short of the
		// timestamp it was released at (Opus pre-skip), so stopping at the floor exactly can leave the
		// ring stalled with the gate waiting on it.
		const first = this.#first;
		const last = this.#last;
		if (first !== undefined && last !== undefined) {
			const from = playhead ?? first;
			if (timestamp - this.#headroom > from && last - this.#headroom >= from) return false;
		}
		this.#first ??= timestamp;
		this.#last = last === undefined ? timestamp : (Math.max(last, timestamp) as Micro);
		return true;
	}
}

/** Convert a sample count to a Micro duration at the given sample rate. */
function samplesToMicro(samples: number, rate: number): Micro {
	return Micro.fromSecond((samples / rate) as Second);
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
	insert(timestamp: Micro, data: Float32Array[]): void;

	/** Update the target latency in samples. */
	setLatency(samples: number): void;

	/** Flush buffered samples and re-stall, ready to anchor the next utterance (buffered mode). */
	reset(): void;

	/**
	 * Drop buffered samples at or after `timestamp`, keeping what is already due.
	 *
	 * Used when a new track takes over the timeline: its samples overwrite the slots they land on,
	 * but the previous track's write-ahead tail would otherwise play once the new audio runs out.
	 * Unlike `reset()` this keeps playing, so there's no gap at the handover.
	 */
	truncate(timestamp: Micro): void;

	/**
	 * Resolve once the playhead is near enough to decode a frame at `timestamp`. In buffered mode this
	 * applies backpressure: it stays pending while decoding `timestamp` would run more than the latency
	 * floor ahead of the playhead, so the caller holds the (encoded) frame instead of decoding it too
	 * far ahead of the floor-sized ring. Resolves immediately when not buffered (the ring bounds itself).
	 * Resolves early, releasing the wait, once `signal` aborts.
	 */
	wait(timestamp: Micro, signal: AbortSignal): Promise<void>;

	/** Current playback timestamp (derived from reader position). */
	readonly timestamp: Getter<Micro>;

	/** Whether the buffer is stalled (waiting to fill). */
	readonly stalled: Getter<boolean>;

	/** How many times the ring has run dry mid-playback, cumulative. */
	readonly underruns: Getter<number>;

	/** Release any resources (event listeners, intervals, etc.). */
	close(): void;
}

/** Returns true when SharedArrayBuffer is available and usable in the current context. */
function supportsSharedArrayBuffer(): boolean {
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
	console.warn(
		"[audio] SharedArrayBuffer unavailable, falling back to the higher latency postMessage audio buffer. " +
			"Serve the page cross-origin isolated (Cross-Origin-Opener-Policy: same-origin, Cross-Origin-Embedder-Policy: require-corp) to avoid this.",
	);
	return new PostAudioBuffer(worklet, channels, rate, latencySamples, buffered);
}

/** SharedArrayBuffer-backed implementation. Writes go directly into shared memory. */
class SharedAudioBuffer implements AudioBuffer {
	readonly rate: number;
	readonly channels: number;
	#worklet: AudioWorkletNode;
	#ring: SharedRingBuffer;

	readonly #timestamp = new Signal<Micro>(0 as Micro);
	readonly timestamp: Getter<Micro> = this.#timestamp;

	readonly #stalled = new Signal<boolean>(true);
	readonly stalled: Getter<boolean> = this.#stalled;

	readonly #underruns = new Signal<number>(0);
	readonly underruns: Getter<number> = this.#underruns;

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
		this.#signals.interval(
			() => {
				this.#timestamp.set(this.#ring.timestamp);
				this.#stalled.set(this.#ring.stalled);
				this.#underruns.set(this.#ring.underruns);
				this.#observe();
			},
			Nano.fromMilli(Milli(50)),
		);
	}

	// Until the next insert anchors a reset ring, its playhead is still the old timeline's.
	#observe(): void {
		this.#backpressure.observe(this.#ring.anchored ? this.#ring.timestamp : undefined, this.#ring.stalled);
	}

	insert(timestamp: Micro, data: Float32Array[]): void {
		this.#ring.insert(timestamp, data);
	}

	setLatency(samples: number): void {
		this.#backpressure.setHeadroom(samplesToMicro(samples, this.rate));

		// Park the current ring before any resize: the worklet keeps reading it until `init-shared`
		// lands, and `resize` carries the floor and the park over to the copy.
		this.#ring.setLatency(samples);

		// Grow the ring (preserving the unread window) if it's too small for the new latency.
		if (this.#ring.capacity < samples * 1.5) {
			const newCapacity = Math.max(this.rate, samples * 2);
			this.#ring = this.#ring.resize(newCapacity);

			const msg: InitShared = { type: "init-shared", ...this.#ring.init };
			this.#worklet.port.postMessage(msg);
		}
	}

	truncate(timestamp: Micro): void {
		this.#ring.truncate(timestamp);
	}

	reset(): void {
		this.#ring.reset();
		this.#backpressure.reset();
	}

	wait(timestamp: Micro, signal: AbortSignal): Promise<void> {
		this.#observe();
		return this.#backpressure.wait(timestamp, signal);
	}

	close(): void {
		this.#backpressure.close(); // never leave a decode loop awaiting a closed buffer
		this.#signals.close();
	}
}

/** postMessage-backed fallback implementation. Samples are transferred, not shared. */
class PostAudioBuffer implements AudioBuffer {
	readonly rate: number;
	readonly channels: number;
	#worklet: AudioWorkletNode;

	readonly #timestamp = new Signal<Micro>(0 as Micro);
	readonly timestamp: Getter<Micro> = this.#timestamp;

	readonly #stalled = new Signal<boolean>(true);
	readonly stalled: Getter<boolean> = this.#stalled;

	readonly #underruns = new Signal<number>(0);
	readonly underruns: Getter<number> = this.#underruns;

	// Backpressure runs off the playhead the worklet reports in its state messages.
	#backpressure: Backpressure;
	// Bumped per reset. A state message posted before the worklet saw the reset reports the old
	// timeline's playhead, which would open the gate for the new one.
	#epoch = 0;

	#signals = new Effect();

	constructor(worklet: AudioWorkletNode, channels: number, rate: number, latencySamples: number, buffered: boolean) {
		this.#worklet = worklet;
		this.channels = channels;
		this.rate = rate;

		this.#backpressure = new Backpressure(buffered, samplesToMicro(latencySamples, rate));

		const latency = Milli.fromSecond((latencySamples / rate) as Second);
		const msg: InitPost = { type: "init-post", channels, rate, latency, buffered };
		worklet.port.postMessage(msg);

		// Listen for state updates from the worklet.
		this.#signals.event(worklet.port, "message", (ev: Event) => {
			const data = (ev as MessageEvent<State>).data;
			if (data?.type === "state") {
				this.#timestamp.set(data.timestamp);
				this.#stalled.set(data.stalled);
				this.#underruns.set(data.underruns);
				if (data.epoch === this.#epoch) this.#backpressure.observe(data.timestamp, data.stalled);
			}
		});
		// addEventListener on a MessagePort requires start() to begin delivery.
		worklet.port.start();
	}

	insert(timestamp: Micro, data: Float32Array[]): void {
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

		const latency = Milli.fromSecond((samples / this.rate) as Second);
		const msg: Latency = { type: "latency", latency };
		this.#worklet.port.postMessage(msg);
	}

	truncate(timestamp: Micro): void {
		const msg: Truncate = { type: "truncate", timestamp };
		this.#worklet.port.postMessage(msg);
	}

	reset(): void {
		this.#epoch++;
		const msg: Reset = { type: "reset", epoch: this.#epoch };
		this.#worklet.port.postMessage(msg);
		this.#backpressure.reset();
	}

	wait(timestamp: Micro, signal: AbortSignal): Promise<void> {
		// The worklet-reported playhead lags by a state-message interval, which only holds frames longer.
		return this.#backpressure.wait(timestamp, signal);
	}

	close(): void {
		this.#backpressure.close(); // never leave a decode loop awaiting a closed buffer
		this.#signals.close();
	}
}
