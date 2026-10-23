import { Milli, Nano, now, sleep } from "@moq/clock";
import {
	type Dispose,
	Effect,
	type Getter,
	getter,
	type Inputs,
	type Readonlys,
	readonlys,
	Signal,
} from "@moq/signals";

/**
 * How far playback trails the live edge.
 *
 * `"auto"` (the default) holds the largest playout target the registered decoders measure from how
 * late their frames arrive (see doc/concept/audio-jitter.md). A `Milli` fixes the delay at
 * exactly that number and disables adaptation.
 *
 * `"instant"` drops the buffer and the pacing together: nothing is held, and {@link Sync.wait}
 * returns without sleeping, so a frame presents as soon as it exists. It also overrides
 * {@link SyncInput.buffer}, since holding a lookahead would contradict holding nothing. Audio is
 * off: a ring with no depth underruns, so the audio decoder unsubscribes and flushes its ring.
 */
export type Delay = "instant" | "auto" | Milli;

/** Signals the sync reads. */
export type SyncInput = {
	/** How far playback trails the live edge. See {@link Delay}. */
	delay: Getter<Delay>;

	/**
	 * Future-dated media held beyond the live edge before playback skips ahead (default: zero).
	 *
	 * Zero minimizes latency, the live default: playback re-anchors as soon as anything arrives
	 * early. A larger value lets faster-than-real-time frames (a TTS response with future
	 * timestamps) build up instead of being skipped. Always finite, so worst case the audio ring
	 * drops its oldest samples rather than exhausting memory.
	 */
	buffer: Getter<Milli>;
};

type SyncOutput = {
	// Derived: whether the delay is "instant". Numeric and "auto" delays share `false`, so readers
	// that only care about the mode don't rerun when the number changes.
	instant: Signal<boolean>;

	// The earliest time we've received a frame, relative to its timestamp: arrival minus timestamp
	// in milliseconds, an offset that is negative when the timestamps run ahead of the local clock.
	// This will keep being updated as we catch up to the live playhead then will be relatively static.
	reference: Signal<number | undefined>;

	// The resolved delay from the live edge to the playhead: the configured number, or in "auto" the
	// largest registered playout target. Zero for "instant".
	delay: Signal<Milli>;

	// The media timestamp of the most recently received frame.
	timestamp: Signal<Milli | undefined>;

	// Derived: true when a lookahead is configured. Buffered playback lets the reference stay
	// anchored so future-dated frames build up, re-anchoring (skipping ahead) only once they
	// would sit further than `maxDelay` ahead of the playhead. See `reset()`.
	buffered: Signal<boolean>;

	// Derived: `delay + buffer`, the furthest a held frame may sit ahead of the playhead. Feeds the
	// transport subscription and the container consumer, which bound the same span. Always finite.
	maxDelay: Signal<Milli>;
};

/** The playback clock: decides when each frame plays so audio and video stay together at the configured delay. */
export class Sync {
	/** The wired inputs, read-only. */
	readonly in: Readonlys<SyncInput>;

	readonly #out: SyncOutput = {
		instant: new Signal<boolean>(false),
		reference: new Signal<number | undefined>(undefined),
		delay: new Signal<Milli>(Milli.zero),
		timestamp: new Signal<Milli | undefined>(undefined),
		buffered: new Signal<boolean>(false),
		maxDelay: new Signal<Milli>(Milli.zero),
	};
	/** The derived outputs, read-only. */
	readonly out = readonlys(this.#out);

	#media = new Signal<{ target: Getter<Milli | undefined> }[]>([]);

	// Bumped by every `reset()`. A frame that waited across a bump belongs to the previous timeline,
	// even when the next frame restores the same reference value.
	#epoch = new Signal(0);

	// The furthest playhead a skip has jumped to on this timeline. A frame below it was passed over.
	#floor: number | undefined;

	// Releases parked waits on `close()`.
	#closed = new Signal(false);

	#signals = new Effect();

	constructor(props?: Inputs<SyncInput>) {
		this.in = {
			delay: getter(props?.delay ?? ("auto" as Delay)),
			buffer: getter(props?.buffer ?? Milli.zero),
		};

		this.#signals.run(this.#runInstant.bind(this));
		this.#signals.run(this.#runDelay.bind(this));
		this.#signals.run(this.#runMaxDelay.bind(this));
	}

	/**
	 * Hold a decoder's "auto" playout target in the shared playback clock until disposed.
	 * @internal
	 */
	register(target: Getter<Milli | undefined>): Dispose {
		const registered = { target };
		this.#media.update((media) => [...media, registered]);
		return () => this.#media.update((media) => media.filter((candidate) => candidate !== registered));
	}

	#runInstant(effect: Effect): void {
		this.#out.instant.set(effect.get(this.in.delay) === "instant");
	}

	// Derive `buffered` / `maxDelay` from the resolved delay and the configured lookahead.
	#runMaxDelay(effect: Effect): void {
		const delay = effect.get(this.#out.delay);
		// "instant" holds nothing, so a configured lookahead doesn't apply.
		const buffer = effect.get(this.#out.instant) ? Milli.zero : effect.get(this.in.buffer);

		this.#out.buffered.set(buffer > 0);
		this.#out.maxDelay.set(Milli(delay + buffer));
	}

	// A fixed delay is taken literally and "auto" holds the deepest track's target, so every track
	// plays from one clock. See doc/concept/audio-jitter.md for how a target is measured.
	#runDelay(effect: Effect): void {
		const mode = effect.get(this.in.delay);

		let delay = Milli.zero;
		if (typeof mode === "number") {
			delay = mode;
		} else if (mode === "auto") {
			for (const registered of effect.get(this.#media)) {
				delay = Milli(Math.max(delay, effect.get(registered.target) ?? Milli.zero));
			}
		}

		this.#out.delay.set(delay);
	}

	/**
	 * Anchor playback from ingress time and report whether the frame arrived late.
	 * @internal
	 */
	// Fold a newly received frame into the reference. The reference anchors playback to the
	// wall clock; we lower it (skip ahead) only when keeping it would push the lookahead past `maxDelay`.
	received(timestamp: Milli, arrival = Nano.toMilli(Nano(now()))): boolean {
		this.#out.timestamp.update((current) => (current === undefined || timestamp > current ? timestamp : current));
		const ref = arrival - timestamp;
		const currentRef = this.#out.reference.peek();

		// First frame anchors the reference.
		if (currentRef === undefined) {
			this.#out.reference.set(ref);
			return false;
		}

		// Check if `wait()` would not sleep at all.
		// NOTE: We check here instead of in `wait()` so we can identify when frames are received late.
		// Otherwise, chained `wait()` calls would cause a false-positive during CPU starvation.
		const delay = this.#out.delay.peek();
		const sleep = currentRef - ref + delay;
		const late = sleep < 0;

		// Frame isn't earlier than the anchor: it can't add lookahead, so keep the reference.
		if (ref >= currentRef) return late;

		// Frame is earlier (more lookahead). `sleep` is how far ahead of the playhead keeping the
		// anchor would put it.
		const cap = this.#out.maxDelay.peek();
		if (sleep <= cap) return late; // within budget: let the buffer grow instead of skipping ahead

		// Over the cap: re-anchor down so the resulting lookahead is exactly the cap.
		const reference = ref + cap - delay;
		this.#out.reference.set(reference);

		// The playhead now sits at `timestamp - cap`, plus however long this frame waited between
		// ingress and here. "instant" has no cap, so every newer frame would pass over whatever is
		// still decoding; it presents frames as they exist instead.
		if (this.in.delay.peek() !== "instant") {
			const floor = Nano.toMilli(Nano(now())) - reference - delay;
			if (this.#floor === undefined || floor > this.#floor) this.#floor = floor;
		}
		return late;
	}

	/**
	 * Re-anchor playback to the next frame received. Call this at an utterance boundary
	 * in buffered mode (typically alongside flushing the audio buffer) so the new content
	 * plays from its own first frame instead of inheriting the previous reference.
	 */
	reset(): void {
		this.#epoch.update((epoch) => epoch + 1);
		this.#floor = undefined;
		this.#out.reference.set(undefined);
	}

	/**
	 * Whether a frame at `timestamp` is anchored on the current timeline and ahead of any skip, so
	 * it may still present. Advisory, for showing a frame early; {@link Sync.wait} decides on its own.
	 * @internal
	 */
	presentable(timestamp: Milli): boolean {
		return this.#out.reference.peek() !== undefined && !this.#skipped(timestamp);
	}

	// Whether a playhead skip on this timeline passed over `timestamp`.
	#skipped(timestamp: Milli): boolean {
		return this.#floor !== undefined && timestamp < this.#floor;
	}

	/**
	 * The PTS that should be rendering right now, derived from the reference + delay; negative
	 * before the first frame's turn. Undefined if no frames have been received yet.
	 */
	now(): Milli | undefined {
		const reference = this.#out.reference.peek();
		if (reference === undefined) return undefined;
		return Milli(Nano.toMilli(Nano(now())) - reference - this.#out.delay.peek());
	}

	/**
	 * Sleep until it's time to render this frame, then resolve whether to present it.
	 *
	 * `true` presents the frame now. `false` drops it: it was unanchored, a skip passed over it, a
	 * `reset()` ended its timeline, or `close()` or `signal` released the wait.
	 * @internal
	 */
	async wait(timestamp: Milli, signal?: AbortSignal): Promise<boolean> {
		const epoch = this.#epoch.peek();

		for (;;) {
			// Closing or aborting wakes the sleep below, so frames parked here leave.
			if (this.#closed.peek() || signal?.aborted) return false;

			// A reset clears the reference, so an unanchored frame predates the current timeline.
			const reference = this.#out.reference.peek();
			if (reference === undefined || this.#epoch.peek() !== epoch || this.#skipped(timestamp)) return false;

			// A zero delay still sleeps: the sleep comes from the reference, which holds an early frame
			// until its timestamp comes up. "instant" is the only thing that skips the wait itself.
			// Peek the input, not `out.instant`, which lags a same-turn change until effects flush.
			if (this.in.delay.peek() === "instant") return true;

			// NOTE: This function runs in parallel for each frame.
			const sleep = reference + timestamp - Nano.toMilli(Nano(now())) + this.#out.delay.peek();

			// Skip the timer for small sleeps; the host's timer resolution (~4ms) would overshoot.
			if (sleep < 5) return true;

			await this.#sleep(sleep, signal);
		}
	}

	// Sleeps for `ms`, or wakes early once anything the sleep was computed from changes or `signal`
	// aborts. Releases every listener either way: frames sleep once each, so a listener left on a
	// signal that never changes would pile up for the life of the player.
	#sleep(ms: number, signal?: AbortSignal): Promise<void> {
		return new Promise((resolve) => {
			const timer = new AbortController();
			const wake = () => {
				timer.abort();
				for (const dispose of disposes) dispose();
				resolve();
			};
			void sleep(Nano.fromMilli(Milli(ms)), { signal: timer.signal }).then(wake, () => {});
			const disposes = [
				this.in.delay.changed(wake),
				this.#out.delay.changed(wake),
				this.#out.reference.changed(wake),
				this.#epoch.changed(wake),
				this.#closed.changed(wake),
			];
			if (signal) {
				signal.addEventListener("abort", wake, { once: true });
				disposes.push(() => signal.removeEventListener("abort", wake));
			}
		});
	}

	/** Stop and release everything this holds. */
	close() {
		this.#closed.set(true);
		this.#signals.close();
	}
}
