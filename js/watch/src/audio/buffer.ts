import { Micro, Milli, type Second } from "@moq/clock";
import { Effect, type Getter, Signal } from "@moq/signals";
import { ringHold, ringSamples } from "./latency";
import type { Data, Init, Latency, Park, Reset, State, Truncate } from "./render";

/**
 * Decides when the decode loop may decode a frame, which is how the target delay is split between
 * the decoded ring and the encoded media upstream of it.
 *
 * The ring holds the floor as PCM, at most a receiver-chosen budget. The rest of the target (the
 * hold) and, in buffered mode, the lookahead stay encoded in the container consumer, so memory follows
 * the bytes actually received rather than whatever a catalog advertised. A frame at `timestamp` is due:
 *
 *   - while the ring is stalled, once the media queued behind it covers the hold, so playback starts
 *     or refills with the whole target ahead of the playhead, decoded or not. A buffered refill also
 *     stops at the first frame that reaches the floor, since the ring plays through and cannot shed
 *     what overflows it;
 *   - while playing, once the playhead is within the floor of it, keeping the ring on its floor;
 *   - while playing live, also once the media queued behind it covers the hold, so media past the
 *     target reaches the ring, whose skip band catches up to the live edge. Buffered mode plays
 *     through instead.
 *
 * With no hold, live playback releases every frame at once and the ring bounds itself. The playhead
 * comes from the worklet's state messages.
 *
 * @internal Exported for the replay harness.
 */
export class Backpressure {
	readonly #rate: number;
	readonly #buffered: boolean;
	#closed = false;

	// The ring's floor in samples and as a duration, and the part of the target above it, held encoded.
	#floor = 0;
	#headroom = 0 as Micro;
	#hold = 0 as Micro;

	// The newest media timestamp queued upstream of the decoder, unset while nothing is.
	#queued?: Micro;

	// The ring as last observed. The playhead is undefined until one is known on the current timeline.
	#playhead?: Micro;
	#stalled = true;

	// The first and latest frames released since the ring stalled. A stalled ring refills to the
	// floor from its parked playhead, but releasing it outright would let the whole lookahead through
	// before the decoder's output reaches the ring and un-stalls it.
	#first?: Micro;
	#last?: Micro;

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
		this.#headroom = Micro.fromSecond((this.#floor / this.#rate) as Second);
		const hold = ringHold(delay);
		const rise = hold > this.#hold;
		this.#hold = hold;

		// Parks whatever the ring was last reported doing: a report of a stall may be stale, and
		// parking a ring that is still stalled changes nothing.
		const playhead = this.#playhead;
		const park = rise && playhead !== undefined && !this.#covered(playhead + this.#headroom);
		if (park) this.#stalled = true;
		this.#release();
		return park;
	}

	/** Note the ring's playhead and whether it is stalled, and release every frame now due. */
	observe(playhead: Micro | undefined, stalled: boolean): void {
		this.#playhead = playhead;
		this.#stalled = stalled;
		if (!stalled) {
			this.#first = undefined;
			this.#last = undefined;
		}
		this.#release();
	}

	/** Note the newest media timestamp queued upstream of the decoder, or undefined once none is. */
	queued(newest: Micro | undefined): void {
		this.#queued = newest;
		this.#release();
	}

	wait(timestamp: Micro, signal: AbortSignal): Promise<void> {
		if (this.#closed || signal.aborted || this.#admit(timestamp)) return Promise.resolve();
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

	/** The ring was flushed for a new timeline: its playhead is unknown until the next frame anchors it. */
	reset(): void {
		this.#playhead = undefined;
		this.#stalled = true;
		this.#first = undefined;
		this.#last = undefined;
		this.#release();
	}

	/** Stop gating for good. Nothing advances a closed buffer, so a later wait would never settle. */
	close(): void {
		this.#closed = true;
		for (const { resolve } of this.#waiters) resolve();
		this.#waiters = [];
	}

	// Thresholds are recomputed live, so a moved target takes effect on queued waiters too.
	#release(): void {
		if (this.#waiters.length === 0) return;
		this.#waiters = this.#waiters.filter(({ timestamp, resolve }) => {
			if (!this.#admit(timestamp)) return true;
			resolve();
			return false;
		});
	}

	// Whether the queued media reaches the hold past `timestamp`.
	#covered(timestamp: number): boolean {
		if (this.#hold === 0) return true;
		return this.#queued !== undefined && this.#queued >= timestamp + this.#hold;
	}

	#admit(timestamp: Micro): boolean {
		const playhead = this.#playhead;
		if (!this.#stalled && playhead !== undefined) {
			if (timestamp - this.#headroom <= playhead) return true;
			return !this.#buffered && this.#covered(timestamp);
		}

		if (!this.#covered(timestamp)) return false;
		// A live ring sheds what overflows its floor, so it refills from everything that is due.
		if (!this.#buffered) return true;

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
	// Bumped per reset or park. A state message posted before the worklet saw it reports the ring
	// playing on the old terms, which would open the gate.
	#epoch = 0;

	#signals = new Effect();

	constructor(
		worklet: AudioWorkletNode,
		props: {
			channels: number;
			rate: number;
			/** The target delay. The ring holds up to a fixed floor of it decoded; the rest stays encoded. */
			delay: Milli;
			/** Anchor to the first frame and play through, holding the lookahead upstream. */
			buffered: boolean;
		},
	) {
		const { channels, rate, buffered } = props;
		this.#worklet = worklet;
		this.channels = channels;
		this.rate = rate;

		this.#backpressure = new Backpressure(rate, buffered, props.delay);

		const msg: Init = { type: "init", channels, rate, latency: this.#latency(), buffered };
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

	// The floor as the worklet takes it.
	#latency(): Milli {
		return Milli.fromSecond((this.#backpressure.floor / this.rate) as Second);
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

	/**
	 * Update the target delay. The ring holds up to a fixed floor of it decoded, and the rest stays
	 * encoded upstream until the media queued behind a frame covers it.
	 */
	setLatency(delay: Milli): void {
		const park = this.#backpressure.setLatency(delay);

		const msg: Latency = { type: "latency", latency: this.#latency() };
		this.#worklet.port.postMessage(msg);
		if (park) {
			this.#epoch++;
			const msg: Park = { type: "park", epoch: this.#epoch };
			this.#worklet.port.postMessage(msg);
		}
	}

	/**
	 * Note the newest media timestamp queued upstream of the decoder, or undefined once none is. The
	 * queue is what covers the part of the target held encoded.
	 */
	queued(newest: Micro | undefined): void {
		this.#backpressure.queued(newest);
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
	 * Resolve once a frame at `timestamp` may be decoded, so the caller holds it encoded until then
	 * instead of decoding it too far ahead of the floor-sized ring. Resolves at once for live playback
	 * whose whole target fits the floor. Resolves early, releasing the wait, once `signal` aborts.
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
