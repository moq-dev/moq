import { Milli, Nano, now, sleep } from "@moq/clock";
import type * as Moq from "@moq/net";
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

	/** @internal */
	probe: Getter<Moq.Connection.Probe | undefined>;
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

	// The jitter buffer, always equal to `delay`.
	jitter: Signal<Milli>;

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

export class Sync {
	readonly in: Readonlys<SyncInput>;

	readonly #out: SyncOutput = {
		instant: new Signal<boolean>(false),
		reference: new Signal<number | undefined>(undefined),
		delay: new Signal<Milli>(Milli.zero),
		jitter: new Signal<Milli>(Milli.zero),
		timestamp: new Signal<Milli | undefined>(undefined),
		buffered: new Signal<boolean>(false),
		maxDelay: new Signal<Milli>(Milli.zero),
	};
	readonly out = readonlys(this.#out);

	// Per-label late-frame tracking: accumulate count and max lateness, flush on recovery.
	#late = new Map<string, { count: number; maxMs: number }>();

	#media = new Signal<{ target: Getter<Milli | undefined> }[]>([]);

	#epoch = 0;

	// The furthest playhead a skip has jumped to on this timeline. A frame below it was passed over.
	#floor: number | undefined;

	// Releases parked waits, which may otherwise sleep forever on a reference that never returns.
	#closed = new Signal(false);

	#signals = new Effect();

	constructor(props?: Inputs<SyncInput>) {
		this.in = {
			delay: getter(props?.delay ?? ("auto" as Delay)),
			buffer: getter(props?.buffer ?? Milli.zero),
			probe: getter(props?.probe),
		};

		this.#signals.run(this.#runInstant.bind(this));
		this.#signals.run(this.#runDelay.bind(this));
		this.#signals.run(this.#runMaxDelay.bind(this));
	}

	/** Hold a decoder's "auto" playout target in the shared playback clock until disposed. */
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

		this.#out.jitter.set(delay);
		this.#out.delay.set(delay);
	}

	/** Anchor playback from ingress time and report whether the frame arrived late. */
	// Fold a newly received frame into the reference. The reference anchors playback to the
	// wall clock; we lower it (skip ahead) only when keeping it would push the lookahead past `maxDelay`.
	received(timestamp: Milli, label = "", arrival = Nano.toMilli(Nano(now()))): boolean {
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
		if (sleep < 0) {
			const entry = this.#late.get(label);
			if (entry) {
				entry.count++;
				entry.maxMs = Math.max(entry.maxMs, -sleep);
			} else {
				this.#late.set(label, { count: 1, maxMs: -sleep });
			}
		} else {
			const entry = this.#late.get(label);
			if (entry) {
				const prefix = label ? `sync[${label}]` : "sync";
				const behind = Sync.#formatDuration(entry.maxMs);
				console.debug(`${prefix}: ${entry.count} late frame(s), max ${behind} behind`);
				this.#late.delete(label);
			}
		}

		// Frame isn't earlier than the anchor: it can't add lookahead, so keep the reference.
		if (ref >= currentRef) return sleep < 0;

		// Frame is earlier (more lookahead). `sleep` is how far ahead of the playhead keeping the
		// anchor would put it.
		const cap = this.#out.maxDelay.peek();
		if (sleep <= cap) return sleep < 0; // within budget: let the buffer grow instead of skipping ahead

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
		return sleep < 0;
	}

	// Re-anchor playback to the next frame received. Call this at an utterance boundary
	// in buffered mode (typically alongside flushing the audio buffer) so the new content
	// plays from its own first frame instead of inheriting the previous reference.
	reset(): void {
		this.#epoch++;
		this.#floor = undefined;
		this.#out.reference.set(undefined);
		this.#late.clear();
	}

	/**
	 * Bumped by every `reset()`. A frame that waited across a bump belongs to the previous
	 * timeline, even when the next frame restores the same reference value.
	 * @internal
	 */
	get epoch(): number {
		return this.#epoch;
	}

	/**
	 * Whether a playhead skip on this timeline passed over `timestamp`, so its frame must not render.
	 * @internal
	 */
	skipped(timestamp: Milli): boolean {
		return this.#floor !== undefined && timestamp < this.#floor;
	}

	// The PTS in milliseconds that should be rendering right now, derived from the reference +
	// buffer; negative before the first frame's turn. Undefined if no frames have been received yet.
	now(): number | undefined {
		const reference = this.#out.reference.peek();
		if (reference === undefined) return undefined;
		return Nano.toMilli(Nano(now())) - reference - this.#out.delay.peek();
	}

	/**
	 * Sleep until it's time to render this frame, or until `signal` aborts.
	 *
	 * A reset holds the wait until the next frame re-anchors the clock, then measures the frame
	 * against that new reference. Aborting resolves the wait early and releases it.
	 */
	async wait(timestamp: Milli, signal?: AbortSignal): Promise<void> {
		// A zero delay still sleeps: the sleep comes from the reference, which holds an early frame
		// until its timestamp comes up. "instant" is the only thing that skips the wait itself.
		// Peek the input, not `out.instant`, which lags a same-turn change until effects flush.
		if (this.in.delay.peek() === "instant") return;

		const reference = this.#out.reference.peek();
		if (reference === undefined) {
			throw new Error("reference not set; call received() first");
		}

		for (;;) {
			// Switching to "instant", closing, or aborting wakes the sleep below, so frames parked here leave.
			if (this.in.delay.peek() === "instant" || this.#closed.peek() || signal?.aborted) return;

			// Sleep until it's time to decode the next frame.
			// NOTE: This function runs in parallel for each frame.
			const ref = Nano.toMilli(Nano(now())) - timestamp;

			const currentRef = this.#out.reference.peek();
			if (currentRef === undefined) {
				await this.#sleep(undefined, signal);
				continue;
			}

			const sleep = currentRef - ref + this.#out.delay.peek();
			if (sleep <= 0) return;

			// Skip the timer for small sleeps; the host's timer resolution (~4ms) would overshoot.
			if (sleep < 5) return;

			if (await this.#sleep(sleep, signal)) return;
		}
	}

	// Sleeps for `ms` (forever if undefined), or returns false early once anything the sleep was
	// computed from changes or `signal` aborts. Releases every listener either way: frames sleep
	// once each, so a listener left on a signal that never changes would pile up for the life of
	// the player.
	#sleep(ms: number | undefined, signal?: AbortSignal): Promise<boolean> {
		return new Promise((resolve) => {
			const timer = new AbortController();
			const wake = (ok: boolean) => {
				timer.abort();
				for (const dispose of disposes) dispose();
				resolve(ok);
			};
			if (ms !== undefined) {
				void sleep(Nano.fromMilli(Milli(ms)), { signal: timer.signal }).then(
					() => wake(true),
					() => {},
				);
			}
			const disposes = [
				this.in.delay.changed(() => wake(false)),
				this.#out.delay.changed(() => wake(false)),
				this.#out.reference.changed(() => wake(false)),
				this.#closed.changed(() => wake(false)),
			];
			if (signal) {
				const abort = () => wake(false);
				signal.addEventListener("abort", abort, { once: true });
				disposes.push(() => signal.removeEventListener("abort", abort));
			}
		});
	}

	static #formatDuration(ms: number): string {
		ms = Math.round(ms);
		if (ms < 1000) return `${ms}ms`;
		const s = ms / 1000;
		if (s < 60) return `${Math.round(s * 10) / 10}s`;
		const m = s / 60;
		return `${Math.round(m * 10) / 10}m`;
	}

	close() {
		this.#closed.set(true);
		this.#signals.close();
	}
}
