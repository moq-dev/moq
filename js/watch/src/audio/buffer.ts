import { Duration, Instant, now } from "@moq/clock";
import { Effect, type Getter, Signal } from "@moq/signals";
import { Peak, ringHold, ringSamples } from "./latency";
import type { Clock, Data, End, Init, Latency, Playing, Received, Reset, State, Truncate } from "./render";

// The stretch's 30 ms processing window, in microseconds.
const WINDOW = 30_000;

// How far the clock's offset from the context's time may fall per state report: far past any drift
// between the two clocks, and well short of the render callback a reading can land within.
const FALL = 100;

// An offset that moves further than this moved for real, as a delay or a reference change moves it,
// rather than by where a reading landed within a render callback.
const JUMP = 10_000;

/**
 * Decides when the decode loop may decode a frame, which is how the target delay is split between
 * the decoded ring and the encoded media upstream of it.
 *
 * The ring holds the floor as PCM, at most a receiver-chosen budget. The rest of the target (the
 * hold) and, in buffered mode, the lookahead stay encoded in the container consumer, so memory follows
 * the bytes actually received rather than whatever a catalog advertised. A frame at `timestamp` is due:
 *
 *   - before playback starts on a timeline, once the media queued behind it covers the hold, so
 *     playback starts with the whole target ahead of the playhead, decoded or not. A buffered start
 *     also stops once the PCM in the ring plus the frames still decoding reach the floor, since
 *     nothing drains the ring until it starts;
 *   - once it has started, while what the ring was given, landed or still decoding, reaches less
 *     than the floor and one stretch window past the playhead, so the ring holds its floor and a
 *     full window to stretch. The frame's own timestamp does not matter: across a hole the next
 *     frame lies far ahead, and the ring plays the hole as silence. A stall keeps releasing by the
 *     playhead: the ring resumes on a cushion and slows down to rebuild, and media past the floor
 *     stays encoded, counted toward the level the ring decides on.
 *
 * The playhead comes from the worklet's state messages.
 *
 * @internal Exported for the replay harness.
 */
export class Backpressure {
	readonly #rate: number;
	readonly #buffered: boolean;
	#closed = false;

	// The ring's floor in samples and as a duration, and the part of the target above it, held encoded.
	#floor = 0;
	#headroom = 0;
	#hold = 0;

	// The newest media timestamp queued ahead of the decoder, unset while none is.
	#queued?: number;

	// The ring as last observed. The playhead is undefined until one is known on the current timeline.
	#playhead?: number;
	// Whether the ring has played on the current timeline.
	#started = false;

	// A buffered refill counts what reaches the ring, not what it released: output the decoder
	// discards (warmup, Opus pre-skip, a terminal trim) never lands, so counting released frames would
	// stop the refill short with the ring stalled and nothing left to decode. Releasing outright would
	// instead let the whole lookahead through before the output reaches the ring and un-stalls it.
	//
	// The frames released and not yet decoded, oldest first, and where the first PCM inserted on this
	// timeline starts (the ring anchors there) and the newest ends.
	#decoding: number[] = [];
	#inserted?: { start: number; end: number };
	// How far output lands in the ring behind the frame it was decoded from (Opus pre-skip). A playing
	// ring counts a frame still decoding where its output lands, or an offset past the floor would
	// drain it.
	#offset = 0;
	// The duration of the latest output, which a frame still decoding is taken to match.
	#frame = 0;

	#waiters: Array<{ timestamp: number; resolve: () => void }> = [];

	constructor(rate: number, buffered: boolean, delay: Duration) {
		this.#rate = rate;
		this.#buffered = buffered;
		this.setLatency(delay);
	}

	/** The ring's floor in samples: the part of the target held as decoded PCM. */
	get floor(): number {
		return this.#floor;
	}

	/** How far output lands in the ring behind the frame it was decoded from, in microseconds. */
	get offset(): number {
		return this.#offset;
	}

	/** Split `delay` between the floor and the hold. */
	setLatency(delay: Duration): void {
		this.#floor = ringSamples(this.#rate, delay);
		this.#headroom = (this.#floor / this.#rate) * 1_000_000;
		this.#hold = ringHold(delay).asMicros();
		this.#release();
	}

	/** Note the ring's playhead and whether it is stalled, and release every frame now due. */
	observe(playhead: number | undefined, stalled: boolean): void {
		this.#playhead = playhead;
		if (playhead !== undefined && !stalled) this.#started = true;
		this.#release();
	}

	/** Note the newest media timestamp queued ahead of the decoder, or undefined once none is. */
	queued(newest: number | undefined): void {
		this.#queued = newest;
		this.#release();
	}

	/**
	 * Note that the decoder produced output ending at `end`, which lands in the ring up to `landed`
	 * unless it was discarded. It answers the oldest frame still decoding and every one it reaches
	 * past: a decoder may emit nothing for a frame.
	 */
	decoded(end: number, landed?: number): void {
		if (landed !== undefined) this.#offset = end - landed;
		let answered = 0;
		while (answered < this.#decoding.length && (answered === 0 || this.#decoding[answered] < end)) answered++;
		this.#decoding.splice(0, answered);
		this.#release();
	}

	/** Note that the decoder closed or reset, dropping every frame still decoding. */
	dropped(): void {
		this.#decoding = [];
		this.#release();
	}

	/** Note that PCM from `start` to `end` reached the ring. */
	inserted(start: number, end: number): void {
		this.#frame = end - start;
		const inserted = this.#inserted;
		this.#inserted = { start: inserted?.start ?? start, end: inserted && inserted.end > end ? inserted.end : end };
		this.#release();
	}

	/** Note that the ring dropped what it held at or after `timestamp`, keeping what is already due. */
	truncated(timestamp: number): void {
		const inserted = this.#inserted;
		const start = this.#playhead ?? inserted?.start;
		if (!inserted || start === undefined || timestamp >= inserted.end) return;
		this.#inserted = { start: inserted.start, end: Math.max(timestamp, start) };
	}

	wait(timestamp: number, signal: AbortSignal): Promise<void> {
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
		this.#started = false;
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

	#admit(timestamp: number): boolean {
		const due = this.#due(timestamp);
		if (due) this.#decoding.push(timestamp);
		return due;
	}

	#due(timestamp: number): boolean {
		const playhead = this.#playhead;
		if (this.#started && playhead !== undefined) return this.#reach(playhead) - playhead < this.#headroom + WINDOW;

		if (!this.#covered(timestamp)) return false;
		// A live ring is bounded only by its capacity until it starts, so it fills from everything due.
		if (!this.#buffered) return true;

		// Fill while the PCM in the ring past its playhead, plus the span of frames still decoding, is
		// short of the floor. The ring starts once it holds the floor, and output the decoder discards
		// never counts, so the fill cannot stop with the ring short of it.
		const inserted = this.#inserted;
		const start = playhead ?? inserted?.start;
		const buffered = inserted && start !== undefined ? Math.max(0, inserted.end - start) : 0;
		const decoding = this.#decoding;
		const pending = decoding.length > 0 ? decoding[decoding.length - 1] - decoding[0] : 0;
		return buffered + pending < this.#headroom;
	}

	// Where what the ring was given ends, on its timeline: the newest frame still decoding, where its
	// output lands, or else the PCM it holds.
	#reach(playhead: number): number {
		const landed = this.#inserted?.end ?? playhead;
		const decoding = this.#decoding.at(-1);
		return decoding === undefined ? landed : Math.max(landed, decoding + this.#frame - this.#offset);
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
	// The realtime context the worklet renders in, which reports where its output stands.
	#context: AudioContext;

	readonly #timestamp = new Signal<number>(0);
	/** The playback timestamp sounding now, as of the worklet's last report. */
	readonly timestamp: Getter<number> = this.#timestamp;

	readonly #output = new Signal<Duration>(Duration.ZERO);
	/**
	 * How long the output takes to sound what the ring renders: the context's base latency plus the
	 * highest output latency it reported over the last five seconds.
	 */
	readonly output: Getter<Duration> = this.#output;

	readonly #stalled = new Signal<boolean>(true);
	/** Whether the ring is stalled (waiting to fill). */
	readonly stalled: Getter<boolean> = this.#stalled;

	readonly #underruns = new Signal<number>(0);
	/** How many times the ring has run dry mid-playback, cumulative. */
	readonly underruns: Getter<number> = this.#underruns;

	readonly #skips = new Signal<number>(0);
	/** How many times the ring skipped ahead, dropping queued audio, cumulative. */
	readonly skips: Getter<number> = this.#skips;

	readonly #trimmed = new Signal<Duration>(Duration.ZERO);
	/** Media the ring's skips dropped unplayed, less declared silence, cumulative. */
	readonly trimmed: Getter<Duration> = this.#trimmed;

	readonly #accelerated = new Signal<Duration>(Duration.ZERO);
	/** Media the stretch removed by playing faster, cumulative. */
	readonly accelerated: Getter<Duration> = this.#accelerated;

	readonly #silent = new Signal<boolean>(false);
	/** Whether playback is at a declared end, reached or announced before any audio, and nothing has played since. */
	readonly silent: Getter<boolean> = this.#silent;

	// Backpressure runs off the playhead the worklet reports in its state messages.
	#backpressure: Backpressure;
	// Bumped per reset. A state message posted before the worklet saw it reports the ring on the old
	// timeline, which would open the gate.
	#epoch = 0;
	#playing?: Getter<boolean>;
	// How far audio may trail the shared clock before the ring skips ahead.
	#bound: Duration;
	// The peak output latency, between the render and the speaker, which the ring does not hold.
	#latency = new Peak();
	#heard = Duration.ZERO;
	// What the worklet reported rendering, oldest first, back to what is sounding now.
	#rendered: { time: number; timestamp: number }[] = [];
	// The shared clock less the context time sounding at the same instant, in microseconds, as last
	// sent. A browser that reads its output position only once per render callback reports it up to a
	// callback early, so the greatest offset is the reading taken nearest a callback's start. It falls
	// only slowly, as drift needs, unless it moves past any callback, as a delay or reference change
	// moves it.
	#offset?: number;
	// The shared clock's reference as last sent, in milliseconds. One that moved earlier jumped the
	// clock ahead, which the ring skips rather than speeds through, as video skips the frames it passed.
	#reference?: number;

	#signals = new Effect();

	constructor(
		worklet: AudioWorkletNode,
		props: {
			channels: number;
			rate: number;
			/** The target delay. The ring holds up to a fixed floor of it decoded; the rest stays encoded. */
			delay: Duration;
			/** How far audio may trail the shared clock before the ring skips ahead. */
			bound?: Duration;
			/** Anchor to the first frame and hold the lookahead upstream until playback starts. */
			buffered: boolean;
			/** The shared clock the ring plays onto, and the reference it plays from. */
			clock?: { position(): number | undefined; out: { reference: Getter<number | undefined> } };
			/** Hold consumption until the shared startup floor is ready. */
			playing?: Getter<boolean>;
		},
	) {
		const { channels, rate, buffered, playing, clock } = props;
		this.#playing = props.playing;
		this.#worklet = worklet;
		this.#context = worklet.context as AudioContext;
		this.channels = channels;
		this.rate = rate;
		this.#bound = props.bound ?? Duration.ZERO;

		const held = this.held(props.delay);
		this.#backpressure = new Backpressure(rate, buffered, held);

		const msg: Init = {
			type: "init",
			channels,
			rate,
			...this.#terms(held),
			...(playing && { playing: playing.peek() }),
		};
		worklet.port.postMessage(msg);
		if (playing) {
			this.#signals.run((effect) => {
				const msg: Playing = { type: "playing", playing: effect.get(playing), epoch: this.#epoch };
				worklet.port.postMessage(msg);
			});
		}

		this.#signals.event(worklet.port, "message", (ev: Event) => {
			const data = (ev as MessageEvent<State>).data;
			if (data?.type === "state") {
				const heard = this.#sounding();
				this.#observeOutput();
				this.#timestamp.set(this.#heardAt(data, heard));
				this.#stalled.set(data.stalled);
				this.#underruns.set(data.underruns);
				this.#skips.set(data.skips);
				this.#trimmed.set(Duration.millis(data.trimmed));
				this.#accelerated.set(Duration.millis(data.accelerated));
				this.#silent.set(data.silent);
				if (data.epoch === this.#epoch) {
					this.#backpressure.observe(data.anchored ? data.timestamp : undefined, data.stalled);
					if (clock) this.#clock(clock.position(), clock.out.reference.peek(), heard);
				}
			}
		});
		// addEventListener on a MessagePort requires start() to begin delivery.
		worklet.port.start();
	}

	/**
	 * What the ring holds of a delay: the delay less the output latency it renders ahead by, though
	 * never less than one callback, the least it can render from. Its floor, its startup, and what
	 * stays encoded upstream all follow this, so a start never queues media the clock then skips.
	 * @internal
	 */
	held(delay: Duration): Duration {
		const base = Duration.secs(this.#context.baseLatency || 0);
		return delay.saturatingSub(this.#heard).max(base.min(delay));
	}

	// The terms as the worklet takes them, for what the ring holds.
	#terms(held: Duration): Omit<Latency, "type"> {
		return {
			latency: (this.#backpressure.floor / this.rate) * 1000,
			target: held.asMillis(),
			bound: this.#bound.asMillis(),
		};
	}

	// The context time sounding now, in seconds, or undefined before the output has played anything.
	// The browser reports its output position with the instant it was heard, on the page's clock.
	#sounding(): number | undefined {
		const position = this.#context.getOutputTimestamp();
		if (!position.performanceTime || position.contextTime === undefined) return undefined;
		return position.contextTime + (now().since(Instant.ZERO).asMillis() - position.performanceTime) / 1000;
	}

	// Fold in the output latency the context reports now. `baseLatency` is the callback the context
	// renders at once; `outputLatency`, where reported, the rest of the way to the speaker.
	#observeOutput(): void {
		const context = this.#context;
		this.#heard = this.#latency.observe(Duration.secs(context.outputLatency || 0), now());
		this.#output.set(Duration.secs(context.baseLatency || 0).add(this.#heard));
	}

	// Where the playhead sounding at context time `heard` stands, from what the worklet rendered: the
	// report rendered last at or before it, advanced at real time up to the next. Without a position,
	// the reported playhead.
	#heardAt(state: State, heard: number | undefined): number {
		if (heard === undefined) {
			this.#rendered = [];
			return state.timestamp;
		}
		const rendered = this.#rendered;
		rendered.push({ time: state.time, timestamp: state.timestamp });
		while (rendered.length > 1 && rendered[1].time <= heard) rendered.shift();
		const [at, next] = rendered;
		const projected = at.timestamp + (heard - at.time) * 1_000_000;
		return Math.max(0, next ? Math.min(projected, next.timestamp) : Math.min(projected, at.timestamp));
	}

	// Send where the shared clock stands, in the ring's coordinates, against the context time sounding
	// at the same instant, so the ring renders each sample to sound when the clock says it is due.
	#clock(position: number | undefined, reference: number | undefined, heard: number | undefined): void {
		const time = this.#context.currentTime;
		const last = this.#reference;
		this.#reference = reference;
		if (position === undefined || heard === undefined) {
			this.#offset = undefined;
			const msg: Clock = { type: "clock", time };
			this.#worklet.port.postMessage(msg);
			return;
		}
		// A reference move is a step of the clock, not where a reading landed, so the offset takes it
		// whole. One earlier jumped the clock ahead, which the ring skips rather than speeds through.
		const moved = last !== undefined && reference !== undefined ? (last - reference) * 1000 : 0;
		const jumped = Math.max(0, moved);
		const offset = position * 1000 - this.#backpressure.offset - heard * 1_000_000;
		const previous = this.#offset === undefined ? undefined : this.#offset + moved;
		this.#offset =
			previous !== undefined && Math.abs(offset - previous) < JUMP ? Math.max(offset, previous - FALL) : offset;
		const msg: Clock = {
			type: "clock",
			timestamp: this.#offset + time * 1_000_000,
			time,
			...(jumped > 0 && { jumped }),
		};
		this.#worklet.port.postMessage(msg);
	}

	/**
	 * Note that a frame arrived whose media ends at `end`, on the decoder's timeline. How far the
	 * playhead trails it is the level the ring's stretch decides on, decoded or not.
	 */
	received(end: Duration): void {
		const msg: Received = { type: "received", end: end.asMicros() - this.#backpressure.offset };
		this.#worklet.port.postMessage(msg);
	}

	/**
	 * Note that the decoder produced output ending at `end` on its own timeline, which lands in the ring
	 * up to `landed` unless it was discarded.
	 */
	decoded(end: number, landed?: number): void {
		this.#backpressure.decoded(end, landed);
	}

	/** Note that the decoder closed or reset, dropping every frame still decoding. */
	dropped(): void {
		this.#backpressure.dropped();
	}

	/**
	 * Insert audio samples at the given timestamp, transferring their buffers. A write may land out of
	 * order, but samples behind what live playback already staged, up to 30 ms past the playhead, are dropped.
	 */
	insert(timestamp: number, data: Float32Array[]): void {
		const end = timestamp + ((data[0]?.length ?? 0) / this.rate) * 1_000_000;
		this.#backpressure.inserted(timestamp, end);
		const msg: Data = { type: "data", data, timestamp };
		// Transfer the ArrayBuffers to avoid a copy: the caller loses access to them.
		this.#worklet.port.postMessage(
			msg,
			data.map((d) => d.buffer),
		);
	}

	/**
	 * Update the target delay, and how far audio may trail the shared clock before the ring skips
	 * ahead. The ring holds up to a fixed floor of the delay decoded, and the rest stays encoded
	 * upstream. A moved delay moves the clock, which the ring plays onto by stretching: slower onto a
	 * rise, faster onto a fall.
	 */
	setLatency(delay: Duration, bound: Duration = this.#bound): void {
		this.#bound = bound;
		const held = this.held(delay);
		this.#backpressure.setLatency(held);
		// The clock moves with the delay, so its offset starts over with the next reading.
		this.#offset = undefined;
		const msg: Latency = { type: "latency", ...this.#terms(held) };
		this.#worklet.port.postMessage(msg);
	}

	/**
	 * Note the newest media timestamp queued ahead of the decoder, or undefined once none is. The
	 * queue is what covers the part of the target held encoded before playback starts.
	 */
	queued(newest: number | undefined): void {
		this.#backpressure.queued(newest);
	}

	/**
	 * Drop buffered samples at or after `timestamp`, keeping what is already due.
	 *
	 * Used when a new track takes over the timeline: its samples overwrite the slots they land on,
	 * but the previous track's write-ahead tail would otherwise play once the new audio runs out.
	 * Unlike `reset()` this keeps playing, so there's no gap at the handover.
	 */
	truncate(timestamp: number): void {
		this.#backpressure.truncated(timestamp);
		const msg: Truncate = { type: "truncate", timestamp };
		this.#worklet.port.postMessage(msg);
	}

	/**
	 * Note that the publisher declared its audio ends at `timestamp`, less any codec delay that leaves
	 * the decoded audio short of it, so running dry there plays comfort noise rather than counting as
	 * an underrun, until the next audio plays.
	 */
	end(timestamp: number): void {
		const msg: End = { type: "end", timestamp };
		this.#worklet.port.postMessage(msg);
	}

	/** Flush buffered samples and re-stall, ready to anchor the next utterance (buffered mode). */
	reset(): void {
		this.#reset(false);
	}

	/**
	 * The next utterance after a declared end: flush and re-anchor once playback ran out at the end,
	 * or keep playing into it while the end's audio still plays. The playhead is unknown here until
	 * the worklet reports which.
	 */
	next(): void {
		this.#reset(true);
	}

	#reset(next: boolean): void {
		this.#epoch++;
		const msg: Reset = {
			type: "reset",
			epoch: this.#epoch,
			...(next && { next }),
			...(this.#playing && { playing: this.#playing.peek() }),
		};
		this.#worklet.port.postMessage(msg);
		this.#backpressure.reset();
		this.#offset = undefined;
		this.#rendered = [];
	}

	/**
	 * Resolve once a frame at `timestamp` may be decoded, so the caller holds it encoded until then
	 * instead of decoding it too far ahead of the floor-sized ring. Resolves at once for live playback
	 * whose whole target fits the floor. Resolves early, releasing the wait, once `signal` aborts.
	 */
	wait(timestamp: number, signal: AbortSignal): Promise<void> {
		// The worklet-reported playhead lags by a state-message interval, which only holds frames longer.
		return this.#backpressure.wait(timestamp, signal);
	}

	/** Stop observing the worklet and release every pending wait. */
	close(): void {
		this.#backpressure.close(); // never leave a decode loop awaiting a closed buffer
		this.#signals.close();
	}
}
