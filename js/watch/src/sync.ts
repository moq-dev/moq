import { Duration, Instant, now, sleep } from "@moq/clock";
import { Container } from "@moq/hang";
import { Derived, Effect, type Getter, getter, type Inputs, type Readonlys, readonlys, Signal } from "@moq/signals";

/**
 * How far playback trails the live edge.
 *
 * `"auto"` (the default) holds the largest playout target the registered decoders measure from how
 * late their frames arrive (see doc/concept/audio-jitter.md), at most 10 s. A `Duration` fixes the
 * delay at exactly that value and disables adaptation.
 *
 * `"instant"` drops the buffer and the pacing together: nothing is held, and the wait before
 * presenting a frame never sleeps, so it presents as soon as it exists. It also overrides
 * {@link SyncInput.lookahead}, since holding a lookahead would contradict holding nothing. Audio is
 * off: a ring with no depth underruns, so the audio decoder unsubscribes and flushes its ring.
 */
export type Delay = "instant" | "auto" | Duration;

/** The default {@link SyncInput.stretch}: how far live audio may trail the clock before it skips onto it. @internal */
export const STRETCH = Duration.millis(200);

/** The most an `"auto"` delay holds, and how far catalog `warmup` reaches back. @internal */
export const DELAY_MAX = Container.DELAY_MAX;

/** Signals the sync reads. */
export type SyncInput = {
	/** How far playback trails the live edge. See {@link Delay}. */
	delay: Getter<Delay>;

	/**
	 * Future-dated media held beyond the live edge before playback skips ahead (default: zero).
	 *
	 * Zero minimizes latency, the live default: playback re-anchors as soon as anything arrives
	 * early. A larger value lets faster-than-real-time frames (a TTS response with future
	 * timestamps) build up instead of being skipped. Audio that falls behind, as after a stall,
	 * catches up by playing faster, and skips onto the clock only when it trails by more than the
	 * larger of the lookahead and {@link SyncInput.stretch}. Always
	 * finite, so worst case the audio ring drops its oldest samples rather than exhausting memory.
	 * With a nonzero lookahead, the first video
	 * frame is a poster until every enabled media track holds the resolved delay; `lookahead` is
	 * the most held ahead, not the amount required to start.
	 */
	lookahead: Getter<Duration>;

	/**
	 * How far audio may trail the clock before it skips onto it (default: 200 ms).
	 *
	 * Within this, audio that falls behind the clock, as after a stall, speeds up until it is back on
	 * it; past it, audio skips onto the clock, as video does. Audio that leads the clock by more than
	 * this, as a rise in the delay leaves it, waits for it in silence. A buffered player already keeps its
	 * whole lookahead, so audio skips only past the larger of the two. This sizes nothing else: the
	 * audio subscription reaches 2 s past the delay whatever it is set to.
	 *
	 * Zero turns the time stretch off: received audio plays unaltered, late audio is skipped, an
	 * underrun is silence, and drift against the publisher's clock is corrected by a small skip or
	 * pad. Video is unaffected either way: it stays on the clock.
	 */
	stretch: Getter<Duration>;
};

/** Signals the sync derives. */
export type SyncOutput = {
	// Derived: whether the delay is "instant". Numeric and "auto" delays share `false`, so readers
	// that only care about the mode don't rerun when the number changes.
	instant: Signal<boolean>;

	// The earliest time we've received a frame, relative to its timestamp: arrival minus timestamp
	// in milliseconds, an offset that is negative when the timestamps run ahead of the local clock.
	// This will keep being updated as we catch up to the live playhead then will be relatively static.
	// Each track holds its own part of it; see `Track.reset()`.
	reference: Signal<number | undefined>;

	// The resolved delay from the live edge to the playhead: the configured number, or in "auto" the
	// largest registered playout target, at most DELAY_MAX. Zero for "instant".
	delay: Signal<Duration>;

	// The media timestamp of the most recently received frame.
	timestamp: Signal<Duration | undefined>;

	// Derived: true when a lookahead is configured. Buffered playback lets the reference stay
	// anchored so future-dated frames build up, re-anchoring (skipping ahead) only once they
	// would sit further than `maxDelay` ahead of the playhead. See `reset()`.
	buffered: Signal<boolean>;

	// Derived: `delay + lookahead`, the furthest a held frame may sit ahead of the playhead. Feeds the
	// transport subscription and the container consumer, which bound the same span. Always finite.
	maxDelay: Signal<Duration>;
};

/**
 * A decoded span from the opening of a track's timeline. `complete` once the publisher finished the
 * track, so nothing extends it and it is ready however short.
 * @internal
 */
export type Span = { start: Duration; end: Duration; complete?: boolean };

/** What a track holds in the shared clock. @internal */
export type TrackProps = {
	/** The track's "auto" playout target, unset until it has one. */
	target: Getter<Duration | undefined>;
	/** For buffered startup: undefined is inactive, null is selected but has no playable span yet. */
	startup?: Getter<Span | null | undefined>;
};

/** What one track holds in the clock. @internal */
export type Part = {
	props: TrackProps;
	// Bumped whenever this track's timeline ends, so its parked frames drop.
	epoch: Signal<number>;
	// The reference this track's own arrivals hold on its current timeline.
	anchor?: number;
	// The furthest playhead this track's own skips jumped to, restored when it takes the reference.
	floor?: number;
};

/**
 * Where the clock's current timeline stands. `idle` has not played and holds nothing back: it is
 * unanchored, or anchored with no buffered startup. `starting` holds the media consumers until every
 * startup span fills. `playing` has played, so turning on a lookahead does not start it over.
 * @internal
 */
export type Phase = "idle" | "starting" | "playing";

// The monotonic clock in milliseconds, the unit the reference offsets are kept in.
const wall = (): number => millis(now());
const millis = (at: Instant): number => at.since(Instant.ZERO).asMillis();

/** The state a {@link Sync} and its track handles share. @internal */
export class Clock {
	readonly in: Readonlys<SyncInput>;
	readonly out: SyncOutput = {
		instant: new Signal<boolean>(false),
		reference: new Signal<number | undefined>(undefined),
		delay: new Signal<Duration>(Duration.ZERO),
		timestamp: new Signal<Duration | undefined>(undefined),
		buffered: new Signal<boolean>(false),
		maxDelay: new Signal<Duration>(Duration.ZERO),
	};

	readonly parts = new Signal<Part[]>([]);
	readonly phase = new Signal<Phase>("idle");
	// Whether the media consumers are released.
	readonly playing = new Derived([this.phase], (phase) => phase !== "starting");

	// Bumped by every `reset()`. A frame that waited across a bump belongs to the previous timeline,
	// even when the next frame restores the same reference value.
	readonly epoch = new Signal(0);

	// The furthest playhead a skip has jumped to on this timeline. A frame below it was passed over.
	floor: number | undefined;

	// Releases parked waits on `close()`.
	readonly closed = new Signal(false);

	// The track whose arrivals set the reference, which releases it when its timeline ends.
	owner?: Part;

	constructor(props?: Inputs<SyncInput>) {
		if (props && "buffer" in props) throw new Error("Watch.Sync: `buffer` was renamed to `lookahead`");
		this.in = {
			delay: getter(props?.delay ?? ("auto" as Delay)),
			lookahead: getter(props?.lookahead ?? Duration.ZERO),
			stretch: getter(props?.stretch ?? STRETCH),
		};
	}

	// Fold an arrival into a reference: keep it while the frame adds lookahead within `maxDelay`, and
	// lower it (skip ahead) only as far as that budget requires.
	fold(current: number | undefined, ref: number): number {
		if (current === undefined || ref >= current) return current ?? ref;
		const delay = this.out.delay.peek().asMillis();
		const sleep = current - ref + delay;
		const cap = this.out.maxDelay.peek().asMillis();
		if (sleep <= cap) return current;
		return ref + cap - delay;
	}

	received(part: Part, timestamp: Duration, arrival: Instant): boolean {
		this.out.timestamp.update((current) => (current === undefined || timestamp.gt(current) ? timestamp : current));
		const ref = millis(arrival) - timestamp.asMillis();
		const current = this.out.reference.peek();
		part.anchor = this.fold(part.anchor, ref);

		// First frame anchors the reference.
		if (current === undefined) {
			this.out.reference.set(ref);
			this.owner = part;
			return false;
		}

		// Retain the opening while another track fills; the ordinary max-delay bounds still own
		// storage, but a later arrival must not advance the shared playhead before startup.
		if (!this.playing.peek()) return false;

		// Check if `wait()` would not sleep at all.
		// NOTE: We check here instead of in `wait()` so we can identify when frames are received late.
		// Otherwise, chained `wait()` calls would cause a false-positive during CPU starvation.
		const delay = this.out.delay.peek().asMillis();
		const late = current - ref + delay < 0;

		const reference = this.fold(current, ref);
		if (reference === current) return late;
		this.out.reference.set(reference);
		this.owner = part;

		// The playhead now sits at `timestamp - cap`, plus however long this frame waited between
		// ingress and here. "instant" has no cap, so every newer frame would pass over whatever is
		// still decoding; it presents frames as they exist instead.
		if (this.in.delay.peek() !== "instant") {
			const floor = wall() - reference - delay;
			if (this.floor === undefined || floor > this.floor) this.floor = floor;
			if (part.floor === undefined || floor > part.floor) part.floor = floor;
		}
		return late;
	}

	// End one track's timeline: its parked frames drop and it releases its part of the reference.
	end(part: Part): void {
		part.epoch.update((epoch) => epoch + 1);
		part.anchor = undefined;
		part.floor = undefined;
		if (this.owner !== part) return;

		// The earliest remaining track holds the reference, else the next frame anchors a new one.
		let earliest: Part | undefined;
		for (const other of this.parts.peek()) {
			if (other.anchor === undefined) continue;
			if (earliest?.anchor === undefined || other.anchor < earliest.anchor) earliest = other;
		}
		this.owner = earliest;
		if (earliest?.anchor === undefined) {
			this.unanchor();
			return;
		}
		const current = this.out.reference.peek();
		const reference = earliest.anchor;
		this.out.reference.set(reference);

		// The floor marked frames a skip on the departed track's reference passed over. Moving back
		// to the survivor's puts its frames ahead of the playhead again, except those its own skips
		// passed over; moving forward is a skip.
		this.floor = earliest.floor;
		if (current !== undefined && reference < current && this.in.delay.peek() !== "instant") {
			this.floor = wall() - reference - this.out.delay.peek().asMillis();
		}
	}

	// Clear the reference so the next frame anchors a new timeline. Every parked frame belongs to
	// the old one.
	unanchor(): void {
		this.epoch.update((epoch) => epoch + 1);
		this.floor = undefined;
		this.owner = undefined;
		this.phase.set(this.out.buffered.peek() ? "starting" : "idle");
		this.out.reference.set(undefined);
	}

	reset(): void {
		for (const part of this.parts.peek()) {
			part.anchor = undefined;
			part.floor = undefined;
		}
		this.unanchor();
	}

	presentable(timestamp: Duration): boolean {
		return this.out.reference.peek() !== undefined && !this.skipped(timestamp);
	}

	// Whether a playhead skip on this timeline passed over `timestamp`.
	skipped(timestamp: Duration): boolean {
		return this.floor !== undefined && timestamp.asMillis() < this.floor;
	}

	now(): Duration | undefined {
		if (!this.playing.peek()) return undefined;
		const reference = this.out.reference.peek();
		if (reference === undefined) return undefined;
		// Media time is nonnegative, so a playhead before the first frame's turn reads as zero.
		return Duration.millis(Math.max(0, wall() - reference - this.out.delay.peek().asMillis()));
	}

	async wait(part: Part, timestamp: Duration, signal?: AbortSignal): Promise<boolean> {
		const epoch = this.epoch.peek();
		const own = part.epoch.peek();

		for (;;) {
			// Closing or aborting wakes the sleep below, so frames parked here leave.
			if (this.closed.peek() || signal?.aborted) return false;

			// A reset clears the reference, so an unanchored frame predates the current timeline.
			const reference = this.out.reference.peek();
			if (reference === undefined || this.epoch.peek() !== epoch || this.skipped(timestamp)) return false;
			if (part.epoch.peek() !== own) return false;

			// A zero delay still sleeps: the sleep comes from the reference, which holds an early frame
			// until its timestamp comes up. "instant" is the only thing that skips the wait itself.
			// Peek the input, not `out.instant`, which lags a same-turn change until effects flush.
			if (this.in.delay.peek() === "instant") return true;

			if (!this.playing.peek()) {
				await this.sleep(part, undefined, signal);
				continue;
			}

			// NOTE: This function runs in parallel for each frame.
			const sleep = reference + timestamp.asMillis() - wall() + this.out.delay.peek().asMillis();

			// Skip the timer for small sleeps; the host's timer resolution (~4ms) would overshoot.
			if (sleep < 5) return true;

			await this.sleep(part, sleep, signal);
		}
	}

	// Sleeps for `ms`, or wakes early once anything the sleep was computed from changes or `signal`
	// aborts. Releases every listener either way: frames sleep once each, so a listener left on a
	// signal that never changes would pile up for the life of the player.
	sleep(part: Part, ms: number | undefined, signal?: AbortSignal): Promise<void> {
		return new Promise((resolve) => {
			const timer = new AbortController();
			const wake = () => {
				timer.abort();
				for (const dispose of disposes) dispose();
				resolve();
			};
			if (ms !== undefined) void sleep(Duration.millis(ms), { signal: timer.signal }).then(wake, () => {});
			const disposes = [
				this.in.delay.changed(wake),
				this.out.delay.changed(wake),
				this.out.reference.changed(wake),
				this.epoch.changed(wake),
				this.closed.changed(wake),
				this.phase.changed(wake),
				part.epoch.changed(wake),
			];
			if (signal) {
				signal.addEventListener("abort", wake, { once: true });
				disposes.push(() => signal.removeEventListener("abort", wake));
			}
		});
	}
}

/**
 * One track's hold on the shared clock: its playout target and startup span, its arrivals, and the
 * waits that pace its frames. Close it when the track goes away.
 * @internal
 */
export class Track {
	readonly #clock: Clock;
	readonly #part: Part;

	/** @internal */
	constructor(clock: Clock, props: TrackProps) {
		this.#clock = clock;
		this.#part = { props, epoch: new Signal(0) };
		clock.parts.update((parts) => [...parts, this.#part]);
	}

	/** Fold a received frame into the clock and report whether it arrived too late for its turn. */
	received(timestamp: Duration, arrival: Instant = now()): boolean {
		return this.#clock.received(this.#part, timestamp, arrival);
	}

	/**
	 * Whether a frame at `timestamp` is anchored on the current timeline and ahead of any skip, so it
	 * may still present. Advisory, for showing a frame early; {@link Track.wait} decides on its own.
	 */
	presentable(timestamp: Duration): boolean {
		return this.#clock.presentable(timestamp);
	}

	/**
	 * Sleep until it's time to render this frame, then resolve whether to present it.
	 *
	 * `true` presents the frame now. `false` drops it: it was unanchored, a skip passed over it, a
	 * reset ended its timeline, or `close()` or `signal` released the wait.
	 */
	wait(timestamp: Duration, signal?: AbortSignal): Promise<boolean> {
		return this.#clock.wait(this.#part, timestamp, signal);
	}

	/**
	 * End this track's timeline, at a discontinuity or when it stops receiving. Its parked frames
	 * drop, and its arrivals stop holding the reference; the other tracks' timelines are untouched.
	 */
	reset(): void {
		this.#clock.end(this.#part);
	}

	/** Release this track's hold on the clock. */
	close(): void {
		this.#clock.end(this.#part);
		this.#clock.parts.update((parts) => parts.filter((part) => part !== this.#part));
	}
}

/** The playback clock: decides when each frame plays so audio and video stay together at the configured delay. */
export class Sync {
	/** The wired inputs, read-only. */
	readonly in: Readonlys<SyncInput>;
	/** The derived outputs, read-only. */
	readonly out: Readonlys<SyncOutput>;

	readonly #clock: Clock;
	/** Whether buffered startup has released the media consumers. @internal */
	readonly playing: Getter<boolean>;

	#signals = new Effect();

	constructor(props?: Inputs<SyncInput>) {
		this.#clock = new Clock(props);
		this.in = this.#clock.in;
		this.out = readonlys(this.#clock.out);
		this.playing = this.#clock.playing;

		this.#signals.run(this.#runInstant.bind(this));
		this.#signals.run(this.#runDelay.bind(this));
		this.#signals.run(this.#runMaxDelay.bind(this));
		this.#signals.run(this.#runStartup.bind(this));
	}

	/**
	 * Open a track's hold on the clock: its playout target, startup span, arrivals, and paced frames.
	 * @internal
	 */
	track(props: TrackProps): Track {
		return new Track(this.#clock, props);
	}

	// Startup is data-driven: time passing cannot fill a missing track. The first picture still
	// travels through the ordinary renderer while the consumers wait for this shared gate.
	#runStartup(effect: Effect): void {
		const clock = this.#clock;
		effect.get(clock.epoch);
		const buffered = effect.get(clock.out.buffered);
		const delay = effect.get(clock.out.delay);
		const reference = effect.get(clock.out.reference);
		const spans = effect.get(clock.parts).flatMap((part) => {
			const span = part.props.startup && effect.get(part.props.startup);
			return span === undefined ? [] : [{ part, span }];
		});
		if (!buffered || spans.length === 0) {
			clock.phase.set(reference === undefined ? "idle" : "playing");
			return;
		}
		if (clock.phase.peek() === "playing") return;
		const ready = spans.flatMap(({ part, span }) => (span ? [{ part, span }] : []));
		if (
			reference === undefined ||
			ready.length !== spans.length ||
			ready.some(({ span }) => !span.complete && span.end.saturatingSub(span.start).lt(delay))
		) {
			clock.phase.set("starting");
			return;
		}
		// The track with the earliest opening holds the reference it anchors.
		const earliest = ready.reduce((a, b) => (b.span.start.lt(a.span.start) ? b : a));
		const first = earliest.span.start;
		clock.owner = earliest.part;
		// Anchor at the retained opening, not at its arrival: startup may have waited on another track.
		// Every track's own anchor moves by the same offset, keeping the gaps between tracks that
		// playing would have held, so a later handoff moves the playhead only by that gap.
		const anchored = wall() - first.asMillis() - delay.asMillis();
		for (const part of clock.parts.peek()) {
			if (part.anchor !== undefined) part.anchor += anchored - reference;
		}
		clock.out.reference.set(anchored);
		clock.phase.set("playing");
	}

	#runInstant(effect: Effect): void {
		this.#clock.out.instant.set(effect.get(this.in.delay) === "instant");
	}

	// Derive `buffered` / `maxDelay` from the resolved delay and the configured lookahead.
	#runMaxDelay(effect: Effect): void {
		const out = this.#clock.out;
		const delay = effect.get(out.delay);
		// "instant" holds nothing, so a configured lookahead doesn't apply.
		const lookahead = effect.get(out.instant) ? Duration.ZERO : effect.get(this.in.lookahead);

		out.buffered.set(!lookahead.isZero());
		out.maxDelay.set(delay.add(lookahead));
	}

	// A fixed delay is taken literally and "auto" holds the deepest track's target, so every track
	// plays from one clock. See doc/concept/audio-jitter.md for how a target is measured.
	#runDelay(effect: Effect): void {
		const mode = effect.get(this.in.delay);

		let delay = Duration.ZERO;
		if (mode instanceof Duration) {
			delay = mode;
		} else if (mode === "auto") {
			for (const { props } of effect.get(this.#clock.parts)) {
				delay = delay.max(effect.get(props.target) ?? Duration.ZERO);
			}
			// Capped once, on the resolved delay, so audio and video stay on one clock. The
			// subscription's max delay follows it.
			delay = delay.min(DELAY_MAX);
		}

		this.#clock.out.delay.set(delay);
	}

	/**
	 * Re-anchor playback to the next frame received. Call this at an utterance boundary
	 * in buffered mode (typically alongside flushing the audio buffer) so the new content
	 * plays from its own first frame instead of inheriting the previous reference.
	 */
	reset(): void {
		this.#clock.reset();
	}

	/**
	 * The PTS that should be rendering right now, derived from the reference and delay; zero until
	 * media time zero's turn. Undefined before ingress or while buffered startup is filling.
	 */
	now(): Duration | undefined {
		return this.#clock.now();
	}

	/** Stop and release everything this holds. */
	close() {
		this.#clock.closed.set(true);
		this.#signals.close();
	}
}
