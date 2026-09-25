import type * as Moq from "@moq/net";
import { Time } from "@moq/net";
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
 * `"auto"` (the default) sizes the jitter buffer from the connection RTT; a `Time.Milli` fixes it.
 *
 * `"instant"` drops the buffer and the pacing together: nothing is held, and {@link Sync.wait}
 * returns without sleeping, so a frame presents as soon as it exists. It also overrides
 * {@link SyncInput.buffer}, since holding a lookahead would contradict holding nothing. Audio is
 * the owner's business: a ring with no depth underruns, so whoever wants this mode is expected to
 * turn audio off.
 */
export type Delay = "instant" | "auto" | Time.Milli;

const MIN_JITTER = Time.Milli(20);
const FALLBACK_JITTER = Time.Milli(100);

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
	buffer: Getter<Time.Milli>;

	/**
	 * The connection's PROBE estimates, whose RTT drives "auto" jitter. Usually wired
	 * from a `Connection`'s `probe`.
	 */
	probe: Getter<Moq.Connection.Probe | undefined>;
};

type SyncOutput = {
	// The earliest time we've received a frame, relative to its timestamp.
	// This will keep being updated as we catch up to the live playhead then will be relatively static.
	reference: Signal<Time.Milli | undefined>;

	// The resolved delay from the live edge to the playhead: jitter + max(audio, video).
	delay: Signal<Time.Milli>;

	// The jitter component of `delay` (always numeric).
	// In "auto" mode this is updated automatically from RTT.
	// When the delay is a number, jitter equals that number.
	jitter: Signal<Time.Milli>;

	// The media timestamp of the most recently received frame.
	timestamp: Signal<Time.Milli | undefined>;

	// Derived: true when a lookahead is configured. Buffered playback lets the reference stay
	// anchored so future-dated frames build up, re-anchoring (skipping ahead) only once they
	// would sit further than `maxAge` ahead of the playhead. See `reset()`.
	buffered: Signal<boolean>;

	// Derived: `delay + buffer`, the furthest a held frame may sit ahead of the playhead. Feeds the
	// transport subscription and the container consumer, which bound the same span. Always finite.
	maxAge: Signal<Time.Milli>;
};

export class Sync {
	readonly in: Readonlys<SyncInput>;

	readonly #out: SyncOutput = {
		reference: new Signal<Time.Milli | undefined>(undefined),
		delay: new Signal<Time.Milli>(Time.Milli.zero),
		jitter: new Signal<Time.Milli>(FALLBACK_JITTER),
		timestamp: new Signal<Time.Milli | undefined>(undefined),
		buffered: new Signal<boolean>(false),
		maxAge: new Signal<Time.Milli>(Time.Milli.zero),
	};
	readonly out = readonlys(this.#out);

	// Per-label late-frame tracking: accumulate count and max lateness, flush on recovery.
	#late = new Map<string, { count: number; maxMs: number }>();

	// Minimum RTT seen, used as the baseline for jitter calculation.
	// Avoids inflating jitter due to bufferbloat.
	#minRtt: number | undefined;
	#media = new Signal<{ jitter: Getter<Time.Milli | undefined> }[]>([]);

	#signals = new Effect();

	constructor(props?: Inputs<SyncInput>) {
		this.in = {
			delay: getter(props?.delay ?? ("auto" as Delay)),
			buffer: getter(props?.buffer ?? Time.Milli.zero),
			probe: getter(props?.probe),
		};

		this.#signals.run(this.#runJitter.bind(this));
		this.#signals.run(this.#runDelay.bind(this));
		this.#signals.run(this.#runMaxAge.bind(this));
	}

	/** Include a decoder's rendition delay in the shared playback clock until disposed. */
	register(jitter: Getter<Time.Milli | undefined>): Dispose {
		const registered = { jitter };
		this.#media.update((media) => [...media, registered]);
		return () => this.#media.update((media) => media.filter((candidate) => candidate !== registered));
	}

	// Derive `buffered` / `maxAge` from the resolved delay and the configured lookahead.
	#runMaxAge(effect: Effect): void {
		const delay = effect.get(this.#out.delay);
		// "instant" holds nothing, so a configured lookahead doesn't apply.
		const buffer = effect.get(this.in.delay) === "instant" ? Time.Milli.zero : effect.get(this.in.buffer);

		this.#out.buffered.set(buffer > 0);
		this.#out.maxAge.set(Time.Milli.add(delay, buffer));
	}

	#runJitter(effect: Effect): void {
		const delay = effect.get(this.in.delay);

		if (delay === "instant") {
			// Holds nothing at all.
			this.#minRtt = undefined;
			this.#out.jitter.set(Time.Milli.zero);
			return;
		}

		if (typeof delay === "number") {
			// Fixed mode: the configured delay is the jitter.
			this.#minRtt = undefined;
			this.#out.jitter.set(delay);
			return;
		}

		// "auto" mode: compute jitter from the connection's RTT estimate.
		const rtt = effect.get(this.in.probe)?.rtt;
		if (rtt !== undefined) {
			// Track minimum RTT as baseline, ignoring bufferbloat.
			this.#minRtt = this.#minRtt !== undefined ? Math.min(this.#minRtt, rtt) : rtt;

			// Buffer enough for a retransmit (1 RTT for ACK + retransmit).
			const jitter = Time.Milli(Math.max(MIN_JITTER, this.#minRtt * 1.25));
			this.#out.jitter.set(jitter);
			return;
		}

		// No RTT available: fall back to static default.
		this.#minRtt = undefined;
		this.#out.jitter.set(FALLBACK_JITTER);
	}

	#runDelay(effect: Effect): void {
		const jitter = effect.get(this.#out.jitter);
		let media = Time.Milli.zero;
		for (const registered of effect.get(this.#media)) {
			media = Time.Milli.max(media, effect.get(registered.jitter) ?? Time.Milli.zero);
		}

		// A zero delay still holds the rendition's own delay, which is a frame interval at 60fps.
		// "instant" holds nothing at all.
		const instant = effect.get(this.in.delay) === "instant";
		const delay = instant ? Time.Milli.zero : Time.Milli.add(media, jitter);
		this.#out.delay.set(delay);
	}

	// Fold a newly received frame into the reference. The reference anchors playback to the
	// wall clock; we lower it (skip ahead) only when keeping it would push the lookahead past `maxAge`.
	received(timestamp: Time.Milli, label = ""): void {
		this.#out.timestamp.update((current) => (current === undefined || timestamp > current ? timestamp : current));
		const now = Time.Milli.now();
		const ref = Time.Milli.sub(now, timestamp);
		const currentRef = this.#out.reference.peek();

		// First frame anchors the reference.
		if (currentRef === undefined) {
			this.#out.reference.set(ref);
			return;
		}

		// Check if `wait()` would not sleep at all.
		// NOTE: We check here instead of in `wait()` so we can identify when frames are received late.
		// Otherwise, chained `wait()` calls would cause a false-positive during CPU starvation.
		const delay = this.#out.delay.peek();
		const sleep = Time.Milli.add(Time.Milli.sub(currentRef, ref), delay);
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
		if (ref >= currentRef) return;

		// Frame is earlier (more lookahead). `sleep` is how far ahead of the playhead keeping the
		// anchor would put it.
		const cap = this.#out.maxAge.peek();
		if (sleep <= cap) return; // within budget: let the buffer grow instead of skipping ahead

		// Over the cap: re-anchor down so the resulting lookahead is exactly the cap.
		this.#out.reference.set(Time.Milli.add(ref, Time.Milli.sub(cap, delay)));
	}

	// Re-anchor playback to the next frame received. Call this at an utterance boundary
	// in buffered mode (typically alongside flushing the audio buffer) so the new content
	// plays from its own first frame instead of inheriting the previous reference.
	reset(): void {
		this.#out.reference.set(undefined);
		this.#late.clear();
	}

	// The PTS that should be rendering right now, derived from the reference + buffer.
	// Returns undefined if no frames have been received yet.
	now(): Time.Milli | undefined {
		const reference = this.#out.reference.peek();
		if (reference === undefined) return undefined;
		return Time.Milli.sub(Time.Milli.sub(Time.Milli.now(), reference), this.#out.delay.peek());
	}

	// Sleep until it's time to render this frame.
	async wait(timestamp: Time.Milli): Promise<void> {
		// A zero delay still sleeps: the sleep comes from the reference, which holds an early frame
		// until its timestamp comes up. "instant" is the only thing that skips the wait itself.
		if (this.in.delay.peek() === "instant") return;

		const reference = this.#out.reference.peek();
		if (reference === undefined) {
			throw new Error("reference not set; call received() first");
		}

		for (;;) {
			// Switching to "instant" wakes the sleep below, so frames parked here leave.
			if (this.in.delay.peek() === "instant") return;

			// Sleep until it's time to decode the next frame.
			// NOTE: This function runs in parallel for each frame.
			const now = Time.Milli.now();
			const ref = Time.Milli.sub(now, timestamp);

			const currentRef = this.#out.reference.peek();
			if (currentRef === undefined) return;

			const sleep = Time.Milli.add(Time.Milli.sub(currentRef, ref), this.#out.delay.peek());
			if (sleep <= 0) return;

			// Skip setTimeout for small sleeps; the timer resolution (~4ms) would overshoot.
			if (sleep < 5) return;

			if (await this.#sleep(sleep)) return;
		}
	}

	// Sleeps for `ms`, or returns false early once anything the sleep was computed from changes.
	// Releases every listener either way: frames sleep once each, so a listener left on a signal
	// that never changes would pile up for the life of the player.
	#sleep(ms: number): Promise<boolean> {
		return new Promise((resolve) => {
			const wake = (ok: boolean) => {
				clearTimeout(timer);
				for (const dispose of disposes) dispose();
				resolve(ok);
			};
			const timer = setTimeout(() => wake(true), ms);
			const disposes = [
				this.in.delay.changed(() => wake(false)),
				this.#out.delay.changed(() => wake(false)),
				this.#out.reference.changed(() => wake(false)),
			];
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
		this.#signals.close();
	}
}
