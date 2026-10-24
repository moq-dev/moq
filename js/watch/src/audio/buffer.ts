import { Micro, Milli, type Second } from "@moq/clock";
import { Effect, type Getter, Signal } from "@moq/signals";
import { ringHold, ringSamples } from "./latency";
import type { Data, Init, Latency, Reset, State, Truncate } from "./render";

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
 *     stops once the PCM in the ring plus the frames still decoding reach the floor, since the ring
 *     plays through and cannot shed what overflows it;
 *   - while playing, once the playhead is within the floor of where its output lands, keeping the
 *     ring on its floor;
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

	// The newest media timestamp queued ahead of the decoder, unset while none is.
	#queued?: Micro;

	// The ring as last observed. The playhead is undefined until one is known on the current timeline.
	#playhead?: Micro;
	#stalled = true;
	// The first frame released on the current timeline. Until PCM lands and a report carries a
	// playhead, it stands in for where the ring anchors, since the ring may already be playing.
	#anchor?: Micro;
	// Whether the ring is parked for a hold the queued media does not cover yet. The worklet keeps a
	// park until the next floor update, so only this decides when it ends: a write of output decoded
	// before the park cannot.
	#parked = false;

	// A buffered refill counts what reaches the ring, not what it released: output the decoder
	// discards (warmup, Opus pre-skip, a terminal trim) never lands, so counting released frames would
	// stop the refill short with the ring stalled and nothing left to decode. Releasing outright would
	// instead let the whole lookahead through before the output reaches the ring and un-stalls it.
	//
	// The frames released and not yet decoded, oldest first, and where the first PCM inserted on this
	// timeline starts (the ring anchors there) and the newest ends.
	#decoding: Micro[] = [];
	#inserted?: { start: Micro; end: Micro };
	// How far output lands in the ring behind the frame it was decoded from (Opus pre-skip). A playing
	// ring compares a frame to its playhead where the output lands, or an offset past the floor would
	// drain it.
	#offset = 0;

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
	 * Split `delay` between the floor and the hold. Returns true when the ring must park with the new
	 * floor: a rise in the hold that the queued media does not cover yet holds playback until it does,
	 * the way a deeper floor parks the ring until it refills, since video holds the extra delay on its
	 * own. A floor update ends a park, so a ring already parked stays parked while the remaining hold
	 * is uncovered.
	 */
	setLatency(delay: Milli): boolean {
		this.#floor = ringSamples(this.#rate, delay);
		this.#headroom = Micro.fromSecond((this.#floor / this.#rate) as Second);
		const hold = ringHold(delay);
		const rise = hold > this.#hold;
		this.#hold = hold;

		// Parks whatever the ring was last reported doing: a report of a stall may be stale, and
		// parking a ring that is still stalled changes nothing.
		const playhead = this.#start();
		this.#parked = (rise || this.#parked) && playhead !== undefined && !this.#covered(playhead + this.#headroom);
		if (this.#parked) this.#stalled = true;
		this.#release();
		return this.#parked;
	}

	/** Note the ring's playhead and whether it is stalled, and release every frame now due. */
	observe(playhead: Micro | undefined, stalled: boolean): void {
		this.#playhead = playhead;
		this.#stalled = stalled;
		this.#release();
	}

	/**
	 * Note the newest media timestamp queued ahead of the decoder, or undefined once none is. Returns
	 * true when that covers the hold of a parked ring, which a floor update then resumes.
	 */
	queued(newest: Micro | undefined): boolean {
		this.#queued = newest;
		const playhead = this.#start();
		const resume = this.#parked && playhead !== undefined && this.#covered(playhead + this.#headroom);
		if (resume) this.#parked = false;
		this.#release();
		return resume;
	}

	/**
	 * Note that the decoder produced output ending at `end`, which lands in the ring up to `landed`
	 * unless it was discarded. It answers the oldest frame still decoding and every one it reaches
	 * past: a decoder may emit nothing for a frame.
	 */
	decoded(end: Micro, landed?: Micro): void {
		if (landed !== undefined) this.#offset = end - landed;
		let answered = 0;
		while (answered < this.#decoding.length && (answered === 0 || this.#decoding[answered] < end)) answered++;
		this.#decoding.splice(0, answered);
		this.#release();
	}

	/** Note that the decoder closed, dropping every frame still decoding. */
	dropped(): void {
		this.#decoding = [];
		this.#release();
	}

	/** Note that PCM from `start` to `end` reached the ring. */
	inserted(start: Micro, end: Micro): void {
		const inserted = this.#inserted;
		this.#inserted = { start: inserted?.start ?? start, end: inserted && inserted.end > end ? inserted.end : end };
		this.#release();
	}

	/** Note that the ring dropped what it held at or after `timestamp`, keeping what is already due. */
	truncated(timestamp: Micro): void {
		const inserted = this.#inserted;
		const start = this.#playhead ?? inserted?.start;
		if (!inserted || start === undefined || timestamp >= inserted.end) return;
		this.#inserted = { start: inserted.start, end: (timestamp > start ? timestamp : start) as Micro };
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
		this.#anchor = undefined;
		this.#stalled = true;
		this.#parked = false;
		this.#decoding = [];
		this.#inserted = undefined;
		this.#offset = 0;
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

	// The frame the ring plays from: its reported playhead, else where it anchored or will anchor,
	// mapped back to the frame its output came from, which is what the queued media covers.
	#start(): Micro | undefined {
		const start = this.#playhead ?? this.#inserted?.start;
		if (start !== undefined) return (start + this.#offset) as Micro;
		return this.#anchor;
	}

	#admit(timestamp: Micro): boolean {
		const due = this.#due(timestamp);
		if (due) {
			this.#anchor ??= timestamp;
			if (this.#buffered) this.#decoding.push(timestamp);
		}
		return due;
	}

	#due(timestamp: Micro): boolean {
		const playhead = this.#playhead;
		if (!this.#stalled && playhead !== undefined) {
			if (timestamp - this.#offset - this.#headroom <= playhead) return true;
			return !this.#buffered && this.#covered(timestamp);
		}

		if (!this.#covered(timestamp)) return false;
		// A live ring sheds what overflows its floor, so it refills from everything that is due.
		if (!this.#buffered) return true;

		// Refill while the PCM in the ring past its playhead, plus the span of frames still decoding, is
		// short of the floor. The ring un-stalls once it holds the floor, and output the decoder
		// discards never counts, so the refill cannot stop with the ring short of it.
		const inserted = this.#inserted;
		const start = playhead ?? inserted?.start;
		const buffered = inserted && start !== undefined ? Math.max(0, inserted.end - start) : 0;
		const decoding = this.#decoding;
		const pending = decoding.length > 0 ? (decoding[decoding.length - 1] as number) - (decoding[0] as number) : 0;
		return buffered + pending < this.#headroom;
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

	/**
	 * Note that the decoder produced output ending at `end` on its own timeline, which lands in the ring
	 * up to `landed` unless it was discarded.
	 */
	decoded(end: Micro, landed?: Micro): void {
		this.#backpressure.decoded(end, landed);
	}

	/** Note that the decoder closed, dropping every frame still decoding. */
	dropped(): void {
		this.#backpressure.dropped();
	}

	/** Insert audio samples at the given timestamp, transferring their buffers. Handles out-of-order writes. */
	insert(timestamp: Micro, data: Float32Array[]): void {
		const end = (timestamp + Micro.fromSecond(((data[0]?.length ?? 0) / this.rate) as Second)) as Micro;
		this.#backpressure.inserted(timestamp, end);
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
		// One message, so the worklet never plays a quantum between the new floor and the park.
		if (park) msg.park = ++this.#epoch;
		this.#worklet.port.postMessage(msg);
	}

	/**
	 * Note the newest media timestamp queued ahead of the decoder, or undefined once none is. The
	 * queue is what covers the part of the target held encoded, so it is what resumes a parked ring.
	 */
	queued(newest: Micro | undefined): void {
		if (!this.#backpressure.queued(newest)) return;
		const msg: Latency = { type: "latency", latency: this.#latency() };
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
		this.#backpressure.truncated(timestamp);
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
