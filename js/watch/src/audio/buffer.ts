import { Micro, Milli, Nano, type Second } from "@moq/clock";
import { Effect, type Getter, Signal } from "@moq/signals";
import { ringCapacity, ringHold, ringSamples } from "./latency";
import type { Data, InitPost, InitShared, Latency, Park, Reset, State, Truncate } from "./render";
import { allocSharedRingBuffer, SharedRingBuffer } from "./shared-ring-buffer";

/**
 * Decides when the decode loop may decode a frame, which is how the target delay is split between
 * the decoded ring and the encoded media upstream of it.
 *
 * The ring holds the floor as PCM, at most a receiver-chosen budget. The rest of the target (the
 * hold) and, in buffered mode, the lookahead stay encoded in the container consumer, so memory follows
 * the bytes actually received rather than whatever a catalog advertised. A frame at `timestamp` is due:
 *
 *   - while the ring is stalled, once the media queued behind it covers the hold, so playback starts
 *     or refills with the whole target ahead of the playhead, decoded or not;
 *   - while playing, once the playhead is within the floor of it, keeping the ring on its floor;
 *   - while playing live, also once the media queued behind it covers the hold, so media past the
 *     target reaches the ring, whose skip band catches up to the live edge. Buffered mode plays
 *     through instead.
 *
 * With no hold, live playback releases every frame at once and the ring bounds itself. Both transports
 * share this; they differ only in how they observe the playhead (Atomics poll vs worklet state messages).
 *
 * @internal Exported for the replay harness.
 */
export class Backpressure {
	readonly #rate: number;
	readonly #buffered: boolean;
	#closed = false;

	// The ring's floor in samples, and the part of the target above it, held encoded.
	#floor = 0;
	#hold = 0;

	// The newest media timestamp queued upstream of the decoder, unset while nothing is.
	#queued?: number;

	// The ring as last observed.
	#playhead = 0;
	#stalled = true;

	#waiters: Array<{ timestamp: Micro; resolve: () => void }> = [];

	constructor(rate: number, buffered: boolean, delay: Milli) {
		this.#rate = rate;
		this.#buffered = buffered;
		this.setLatency(delay);
	}

	/** The ring's floor in samples: the part of the target held as decoded PCM. */
	get floor(): number {
		return this.#floor;
	}

	/**
	 * Split `delay` between the floor and the hold. Returns true when the ring must park: a rise in the
	 * hold that the queued media does not cover yet holds playback until it does, the way a deeper
	 * floor parks the ring until it refills, since video holds the extra delay on its own.
	 */
	setLatency(delay: Milli): boolean {
		this.#floor = ringSamples(this.#rate, delay);
		const hold = ringHold(delay);
		const rise = hold > this.#hold;
		this.#hold = hold;
		const park = rise && !this.#stalled && !this.#covered(this.#playhead + this.#headroom());
		if (park) this.#stalled = true;
		this.#advance();
		return park;
	}

	/** Note the ring's playhead and whether it is stalled. */
	observe(playhead: Micro, stalled: boolean): void {
		this.#playhead = playhead;
		this.#stalled = stalled;
		this.#advance();
	}

	/** Note the newest media timestamp queued upstream of the decoder, or undefined once none is. */
	queued(newest: Micro | undefined): void {
		this.#queued = newest;
		this.#advance();
	}

	wait(timestamp: Micro, signal: AbortSignal): Promise<void> {
		if (this.#closed || signal.aborted || this.#due(timestamp)) return Promise.resolve();
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

	// The floor as a duration.
	#headroom(): number {
		return samplesToMicro(this.#floor, this.#rate);
	}

	// Whether the queued media reaches the hold past `timestamp`.
	#covered(timestamp: number): boolean {
		if (this.#hold === 0) return true;
		return this.#queued !== undefined && this.#queued >= timestamp + this.#hold;
	}

	#due(timestamp: Micro): boolean {
		if (this.#stalled) return this.#covered(timestamp);
		if (this.#playhead >= timestamp - this.#headroom()) return true;
		return !this.#buffered && this.#covered(timestamp);
	}

	// Resolve every waiter that is now due. Thresholds are recomputed live, so a moved target takes
	// effect on queued waiters too.
	#advance(): void {
		if (this.#waiters.length === 0) return;
		this.#waiters = this.#waiters.filter(({ timestamp, resolve }) => {
			if (!this.#due(timestamp)) return true;
			resolve();
			return false;
		});
	}

	// Resolve everything unconditionally: never strand a decode loop.
	#flush(): void {
		for (const { resolve } of this.#waiters) resolve();
		this.#waiters = [];
	}

	/** The ring was flushed and re-stalled: the old timeline is gone, so let the decode loop re-anchor. */
	reset(): void {
		this.#stalled = true;
		this.#flush();
	}

	/** Stop gating for good. Nothing advances a closed buffer, so a later wait would never settle. */
	close(): void {
		this.#closed = true;
		this.#flush();
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

	/**
	 * Update the target delay. The ring holds up to a fixed floor of it decoded, and the rest stays
	 * encoded upstream until the playhead nears it.
	 */
	setLatency(delay: Milli): void;

	/**
	 * Note the newest media timestamp queued upstream of the decoder, or undefined once none is. The
	 * queue is what covers the part of the target held encoded.
	 */
	queued(newest: Micro | undefined): void;

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
	 * Resolve once a frame at `timestamp` may be decoded, so the caller holds it encoded until then
	 * instead of decoding it too far ahead of the floor-sized ring. Resolves at once for live playback
	 * whose whole target fits the floor. Resolves early, releasing the wait, once `signal` aborts.
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
	delay: Milli,
	buffered = false,
): AudioBuffer {
	if (supportsSharedArrayBuffer()) {
		console.log("[audio] using SharedArrayBuffer audio buffer");
		return new SharedAudioBuffer(worklet, channels, rate, delay, buffered);
	}
	console.warn(
		"[audio] SharedArrayBuffer unavailable, falling back to the higher latency postMessage audio buffer. " +
			"Serve the page cross-origin isolated (Cross-Origin-Opener-Policy: same-origin, Cross-Origin-Embedder-Policy: require-corp) to avoid this.",
	);
	return new PostAudioBuffer(worklet, channels, rate, delay, buffered);
}

/** SharedArrayBuffer-backed implementation. Writes go directly into shared memory. */
class SharedAudioBuffer implements AudioBuffer {
	readonly rate: number;
	readonly channels: number;
	readonly #ring: SharedRingBuffer;

	readonly #timestamp = new Signal<Micro>(0 as Micro);
	readonly timestamp: Getter<Micro> = this.#timestamp;

	readonly #stalled = new Signal<boolean>(true);
	readonly stalled: Getter<boolean> = this.#stalled;

	readonly #underruns = new Signal<number>(0);
	readonly underruns: Getter<number> = this.#underruns;

	#backpressure: Backpressure;

	#signals = new Effect();

	constructor(worklet: AudioWorkletNode, channels: number, rate: number, delay: Milli, buffered: boolean) {
		this.channels = channels;
		this.rate = rate;

		// The ring holds the floor as decoded PCM, with headroom above it for overflow, at a capacity
		// fixed by the receiver. The rest of the target, and in buffered mode the lookahead, stays
		// encoded upstream, held back by `wait()`.
		this.#backpressure = new Backpressure(rate, buffered, delay);

		const init = allocSharedRingBuffer(channels, ringCapacity(rate), rate, buffered);
		this.#ring = new SharedRingBuffer(init);
		this.#ring.setLatency(this.#backpressure.floor);

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

	insert(timestamp: Micro, data: Float32Array[]): void {
		this.#ring.insert(timestamp, data);
	}

	setLatency(delay: Milli): void {
		this.#observe();
		const park = this.#backpressure.setLatency(delay);
		this.#ring.setLatency(this.#backpressure.floor);
		if (park) this.#ring.park();
		// A deeper floor the buffer does not cover parked the ring too.
		this.#observe();
	}

	queued(newest: Micro | undefined): void {
		this.#backpressure.queued(newest);
	}

	// Feed the live playhead to the gate. Reading the cursor is cheap and idempotent.
	#observe(): void {
		this.#backpressure.observe(this.#ring.timestamp, this.#ring.stalled);
	}

	truncate(timestamp: Micro): void {
		this.#ring.truncate(timestamp);
	}

	reset(): void {
		this.#ring.reset();
		this.#backpressure.reset();
	}

	wait(timestamp: Micro, signal: AbortSignal): Promise<void> {
		// Stalled (bootstrap, an underrun the reader re-stalled on, a park, or an explicit reset)
		// releases frames once the queued media covers the hold, and the ring un-stalls itself on the
		// insert that reaches the floor, so the gate closes again within one frame.
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

	#signals = new Effect();

	constructor(worklet: AudioWorkletNode, channels: number, rate: number, delay: Milli, buffered: boolean) {
		this.#worklet = worklet;
		this.channels = channels;
		this.rate = rate;

		this.#backpressure = new Backpressure(rate, buffered, delay);

		const latency = Milli.fromSecond((this.#backpressure.floor / rate) as Second);
		const msg: InitPost = { type: "init-post", channels, rate, latency, buffered };
		worklet.port.postMessage(msg);

		// Listen for state updates from the worklet.
		this.#signals.event(worklet.port, "message", (ev: Event) => {
			const data = (ev as MessageEvent<State>).data;
			if (data?.type === "state") {
				this.#timestamp.set(data.timestamp);
				this.#stalled.set(data.stalled);
				this.#underruns.set(data.underruns);
				this.#backpressure.observe(data.timestamp, data.stalled);
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

	setLatency(delay: Milli): void {
		const park = this.#backpressure.setLatency(delay);

		const latency = Milli.fromSecond((this.#backpressure.floor / this.rate) as Second);
		const msg: Latency = { type: "latency", latency };
		this.#worklet.port.postMessage(msg);
		if (park) {
			const msg: Park = { type: "park" };
			this.#worklet.port.postMessage(msg);
		}
	}

	queued(newest: Micro | undefined): void {
		this.#backpressure.queued(newest);
	}

	truncate(timestamp: Micro): void {
		const msg: Truncate = { type: "truncate", timestamp };
		this.#worklet.port.postMessage(msg);
	}

	reset(): void {
		const msg: Reset = { type: "reset" };
		this.#worklet.port.postMessage(msg);
		this.#backpressure.reset();
	}

	wait(timestamp: Micro, signal: AbortSignal): Promise<void> {
		// Uses the worklet-reported playhead, which lags by a state-message interval; the floor's
		// headroom covers that. The worklet still drops the oldest if a frame slips through.
		return this.#backpressure.wait(timestamp, signal);
	}

	close(): void {
		this.#backpressure.close(); // never leave a decode loop awaiting a closed buffer
		this.#signals.close();
	}
}
