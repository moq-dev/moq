import { Micro, Milli, type Second } from "@moq/clock";
import { Effect, type Getter, Signal } from "@moq/signals";
import type { Data, Init, Latency, Reset, State, Truncate } from "./render";

/**
 * Timestamp-based backpressure for buffered playback. The decoded PCM ring only holds the latency
 * floor; everything above it (the buffered lookahead, up to the ceiling) stays upstream as encoded
 * Opus. `wait(timestamp)` stays pending until decoding `timestamp` would land within `headroom` (the
 * floor) of the playhead, so the decode loop holds a frame as Opus instead of overflowing the
 * floor-sized ring, which drops the oldest unplayed audio. The playhead comes from the worklet's
 * state messages. A no-op when not buffered (the ring bounds itself).
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
 * The decoder's side of the audio ring. Decoded PCM is transferred to the render worklet over its
 * port, and the worklet reports its playhead back in periodic state messages.
 */
export class AudioBuffer {
	readonly rate: number;
	readonly channels: number;
	#worklet: AudioWorkletNode;

	readonly #timestamp = new Signal<Micro>(0 as Micro);
	/** Current playback timestamp, as the worklet last reported it. */
	readonly timestamp: Getter<Micro> = this.#timestamp;

	readonly #stalled = new Signal<boolean>(true);
	/** Whether the ring is stalled (waiting to fill). */
	readonly stalled: Getter<boolean> = this.#stalled;

	readonly #underruns = new Signal<number>(0);
	/** How many times the ring has run dry mid-playback, cumulative. */
	readonly underruns: Getter<number> = this.#underruns;

	// Backpressure runs off the playhead the worklet reports in its state messages.
	#backpressure: Backpressure;
	// Bumped per reset. A state message posted before the worklet saw the reset reports the old
	// timeline's playhead, which would open the gate for the new one.
	#epoch = 0;

	#signals = new Effect();

	constructor(
		worklet: AudioWorkletNode,
		props: {
			channels: number;
			rate: number;
			/** The latency floor, in samples. */
			latency: number;
			/** Anchor to the first frame and play through, holding the lookahead upstream. */
			buffered: boolean;
		},
	) {
		const { channels, rate, buffered } = props;
		this.#worklet = worklet;
		this.channels = channels;
		this.rate = rate;

		this.#backpressure = new Backpressure(buffered, samplesToMicro(props.latency, rate));

		const latency = Milli.fromSecond((props.latency / rate) as Second);
		const msg: Init = { type: "init", channels, rate, latency, buffered };
		worklet.port.postMessage(msg);

		this.#signals.event(worklet.port, "message", (ev: Event) => {
			const data = (ev as MessageEvent<State>).data;
			if (data?.type === "state") {
				this.#timestamp.set(data.timestamp);
				this.#stalled.set(data.stalled);
				this.#underruns.set(data.underruns);
				if (data.epoch === this.#epoch) {
					this.#backpressure.observe(data.anchored ? data.timestamp : undefined, data.stalled);
				}
			}
		});
		// addEventListener on a MessagePort requires start() to begin delivery.
		worklet.port.start();
	}

	/** Insert audio samples at the given timestamp, transferring their buffers. Handles out-of-order writes. */
	insert(timestamp: Micro, data: Float32Array[]): void {
		const msg: Data = { type: "data", data, timestamp };
		// Transfer the ArrayBuffers to avoid a copy: the caller loses access to them.
		this.#worklet.port.postMessage(
			msg,
			data.map((d) => d.buffer),
		);
	}

	/** Update the latency floor, in samples. */
	setLatency(samples: number): void {
		this.#backpressure.setHeadroom(samplesToMicro(samples, this.rate));

		const latency = Milli.fromSecond((samples / this.rate) as Second);
		const msg: Latency = { type: "latency", latency };
		this.#worklet.port.postMessage(msg);
	}

	/**
	 * Drop buffered samples at or after `timestamp`, keeping what is already due.
	 *
	 * Used when a new track takes over the timeline: its samples overwrite the slots they land on,
	 * but the previous track's write-ahead tail would otherwise play once the new audio runs out.
	 * Unlike `reset()` this keeps playing, so there's no gap at the handover.
	 */
	truncate(timestamp: Micro): void {
		const msg: Truncate = { type: "truncate", timestamp };
		this.#worklet.port.postMessage(msg);
	}

	/** Flush buffered samples and re-stall, ready to anchor the next utterance (buffered mode). */
	reset(): void {
		this.#epoch++;
		const msg: Reset = { type: "reset", epoch: this.#epoch };
		this.#worklet.port.postMessage(msg);
		this.#backpressure.reset();
	}

	/**
	 * Resolve once the playhead is near enough to decode a frame at `timestamp`. In buffered mode this
	 * applies backpressure: it stays pending while decoding `timestamp` would run more than the latency
	 * floor ahead of the playhead, so the caller holds the (encoded) frame instead of decoding it too
	 * far ahead of the floor-sized ring. Resolves immediately when not buffered (the ring bounds itself).
	 * Resolves early, releasing the wait, once `signal` aborts.
	 */
	wait(timestamp: Micro, signal: AbortSignal): Promise<void> {
		// The worklet-reported playhead lags by a state-message interval, which only holds frames longer.
		return this.#backpressure.wait(timestamp, signal);
	}

	/** Stop observing the worklet and release every pending wait. */
	close(): void {
		this.#backpressure.close(); // never leave a decode loop awaiting a closed buffer
		this.#signals.close();
	}
}
