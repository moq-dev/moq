import type * as Moq from "@moq/net";
import { Time } from "@moq/net";
import { Effect, type Getter, getter, type Inputs, type Readonlys, readonlys, Signal } from "@moq/signals";

/** A single latency bound: `"real-time"` adapts to the RTT; a `Time.Milli` fixes the jitter buffer. */
export type Bound = "real-time" | Time.Milli;

/**
 * Latency target. A scalar (or `"real-time"`) collapses the range and minimizes latency, the live
 * default. An object opens a range `[min, max]`: playback buffers freely between the floor and the
 * ceiling and only skips ahead once latency would exceed the ceiling, so faster-than-real-time
 * frames (e.g. a TTS response with future timestamps) build up instead of being skipped. Both
 * bounds default to `"real-time"` when omitted. The ceiling is always finite (no uncapped buffering),
 * so worst case the audio ring drops its oldest samples rather than exhausting memory.
 */
export type Latency = Bound | { min?: Bound; max?: Bound };

/** Resolve a {@link Latency} into explicit floor/ceiling bounds (a scalar collapses to `min == max`). */
export function latencyBounds(latency: Latency): { min: Bound; max: Bound } {
	if (latency === "real-time" || typeof latency === "number") {
		return { min: latency, max: latency };
	}
	return { min: latency.min ?? "real-time", max: latency.max ?? "real-time" };
}

/** Build a {@link Latency} from explicit bounds, collapsing to a scalar when they're equal. */
export function latencyFromBounds(min: Bound, max: Bound): Latency {
	return min === max ? min : { min, max };
}

const MIN_JITTER = Time.Milli(20);
const FALLBACK_JITTER = Time.Milli(100);

export type SyncInput = {
	// Latency target: a scalar minimizes (collapsed range), an object opens a range. See {@link Latency}.
	latency: Getter<Latency>;

	// The connection used for "real-time" jitter: PROBE supplies RTT.
	connection: Getter<Moq.Connection.Established | undefined>;

	// Any additional delay required for audio or video (wired from the per-rendition source).
	audio: Getter<Time.Milli | undefined>;
	video: Getter<Time.Milli | undefined>;
};

type SyncOutput = {
	// The earliest time we've received a frame, relative to its timestamp.
	// This will keep being updated as we catch up to the live playhead then will be relatively static.
	reference: Signal<Time.Milli | undefined>;

	// The total buffer required: jitter + max(audio, video).
	buffer: Signal<Time.Milli>;

	// The jitter buffer in milliseconds (always numeric).
	// In "real-time" mode this is updated automatically from RTT.
	// When the floor is a number, jitter equals that number.
	jitter: Signal<Time.Milli>;

	// The media timestamp of the most recently received frame.
	timestamp: Signal<Time.Milli | undefined>;

	// Derived: true when the ceiling sits above the floor. Buffered playback lets the reference
	// stay anchored so future-dated frames build up a buffer, re-anchoring (skipping ahead) only
	// when latency would exceed the ceiling. See `reset()`.
	buffered: Signal<boolean>;

	// Derived cap on buffered audio (ms), consumed by the audio ring to size itself. Always finite.
	maxBuffer: Signal<Time.Milli>;
};

export class Sync {
	readonly in: Readonlys<SyncInput>;

	readonly #out: SyncOutput = {
		reference: new Signal<Time.Milli | undefined>(undefined),
		buffer: new Signal<Time.Milli>(Time.Milli.zero),
		jitter: new Signal<Time.Milli>(FALLBACK_JITTER),
		timestamp: new Signal<Time.Milli | undefined>(undefined),
		buffered: new Signal<boolean>(false),
		maxBuffer: new Signal<Time.Milli>(Time.Milli.zero),
	};
	readonly out = readonlys(this.#out);

	// A ghetto way to learn when the reference/buffer changes.
	// There's probably a way to use Effect, but lets keep it simple for now.
	#update: PromiseWithResolvers<void>;

	// Per-label late-frame tracking: accumulate count and max lateness, flush on recovery.
	#late = new Map<string, { count: number; maxMs: number }>();

	// Minimum RTT seen, used as the baseline for jitter calculation.
	// Avoids inflating jitter due to bufferbloat.
	#minRtt: number | undefined;

	#signals = new Effect();

	constructor(props?: Inputs<SyncInput>) {
		this.in = {
			latency: getter(props?.latency ?? ("real-time" as Latency)),
			connection: getter(props?.connection),
			audio: getter(props?.audio),
			video: getter(props?.video),
		};

		this.#update = Promise.withResolvers();

		this.#signals.run(this.#runJitter.bind(this));
		this.#signals.run(this.#runBuffer.bind(this));
		this.#signals.run(this.#runRange.bind(this));
	}

	// Derive `buffered` / `maxBuffer` from the floor (`buffer`) and the ceiling (the `max` bound).
	#runRange(effect: Effect): void {
		const { max } = latencyBounds(effect.get(this.in.latency));
		const floor = effect.get(this.#out.buffer);

		if (max === "real-time") {
			// Ceiling tracks the floor: minimize latency, the live default.
			this.#out.buffered.set(false);
			this.#out.maxBuffer.set(floor);
		} else {
			// Buffered only when the ceiling is above the floor; otherwise it collapses to minimize.
			this.#out.buffered.set(max > floor);
			this.#out.maxBuffer.set(Time.Milli.max(max, floor));
		}
	}

	// The maximum total latency (lookahead + floor) we tolerate before re-anchoring, in ms.
	// Used by `received()` to decide when to skip ahead.
	#latencyCap(): Time.Milli {
		const { max } = latencyBounds(this.in.latency.peek());
		const floor = this.#out.buffer.peek();
		if (max === "real-time") return floor;
		return Time.Milli.max(max, floor);
	}

	#runJitter(effect: Effect): void {
		const { min } = latencyBounds(effect.get(this.in.latency));

		if (typeof min === "number") {
			// Fixed mode: the floor value is the jitter.
			this.#minRtt = undefined;
			this.#out.jitter.set(min);
			return;
		}

		// "real-time" mode: compute jitter from RTT on the established connection.
		const conn = effect.get(this.in.connection);
		const rttSignal = conn?.rtt;
		const rtt = rttSignal ? effect.get(rttSignal) : undefined;
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

	#runBuffer(effect: Effect): void {
		const jitter = effect.get(this.#out.jitter);
		const video = effect.get(this.in.video) ?? Time.Milli.zero;
		const audio = effect.get(this.in.audio) ?? Time.Milli.zero;

		const buffer = Time.Milli.add(Time.Milli.max(video, audio), jitter);
		this.#out.buffer.set(buffer);

		this.#update.resolve();
		this.#update = Promise.withResolvers();
	}

	// Fold a newly received frame into the reference. The reference anchors playback to the
	// wall clock; we lower it (skip ahead) only when keeping it would push latency past the cap.
	received(timestamp: Time.Milli, label = ""): void {
		this.#out.timestamp.update((current) => (current === undefined || timestamp > current ? timestamp : current));
		const now = Time.Milli.now();
		const ref = Time.Milli.sub(now, timestamp);
		const currentRef = this.#out.reference.peek();

		// First frame anchors the reference.
		if (currentRef === undefined) {
			this.#setReference(ref);
			return;
		}

		// Check if `wait()` would not sleep at all.
		// NOTE: We check here instead of in `wait()` so we can identify when frames are received late.
		// Otherwise, chained `wait()` calls would cause a false-positive during CPU starvation.
		const floor = this.#out.buffer.peek();
		const sleep = Time.Milli.add(Time.Milli.sub(currentRef, ref), floor);
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

		// Frame isn't earlier than the anchor: it can't lower latency, so keep the reference.
		if (ref >= currentRef) return;

		// Frame is earlier (more lookahead). `sleep` is the latency keeping the anchor would impose.
		const cap = this.#latencyCap();
		if (sleep <= cap) return; // within budget: let the buffer grow instead of skipping ahead

		// Over the cap: re-anchor down so the resulting latency is exactly the cap.
		this.#setReference(Time.Milli.add(ref, Time.Milli.sub(cap, floor)));
	}

	#setReference(ref: Time.Milli): void {
		this.#out.reference.set(ref);
		this.#update.resolve();
		this.#update = Promise.withResolvers();
	}

	// Re-anchor playback to the next frame received. Call this at an utterance boundary
	// in buffered mode (typically alongside flushing the audio buffer) so the new content
	// plays from its own first frame instead of inheriting the previous reference.
	reset(): void {
		this.#out.reference.set(undefined);
		this.#late.clear();
		this.#update.resolve();
		this.#update = Promise.withResolvers();
	}

	// The PTS that should be rendering right now, derived from the reference + buffer.
	// Returns undefined if no frames have been received yet.
	now(): Time.Milli | undefined {
		const reference = this.#out.reference.peek();
		if (reference === undefined) return undefined;
		return Time.Milli.sub(Time.Milli.sub(Time.Milli.now(), reference), this.#out.buffer.peek());
	}

	// Sleep until it's time to render this frame.
	async wait(timestamp: Time.Milli): Promise<void> {
		const reference = this.#out.reference.peek();
		if (reference === undefined) {
			throw new Error("reference not set; call received() first");
		}

		for (;;) {
			// Sleep until it's time to decode the next frame.
			// NOTE: This function runs in parallel for each frame.
			const now = Time.Milli.now();
			const ref = Time.Milli.sub(now, timestamp);

			const currentRef = this.#out.reference.peek();
			if (currentRef === undefined) return;

			const sleep = Time.Milli.add(Time.Milli.sub(currentRef, ref), this.#out.buffer.peek());
			if (sleep <= 0) return;

			// Skip setTimeout for small sleeps; the timer resolution (~4ms) would overshoot.
			if (sleep < 5) return;

			const wait = new Promise((resolve) => setTimeout(resolve, sleep)).then(() => true);

			const ok = await Promise.race([this.#update.promise, wait]);
			if (ok) return;
		}
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
