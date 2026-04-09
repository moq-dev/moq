import { Time } from "@moq/lite";
import { Effect, Signal } from "@moq/signals";

/** Jitter mode: a fixed number of milliseconds, or `"real-time"` for automatic RTT-based jitter. */
export type JitterMode = Time.Milli | "real-time";

const MIN_JITTER = 10 as Time.Milli;
const FALLBACK_JITTER = 100 as Time.Milli;

export interface SyncProps {
	jitter?: JitterMode | Signal<JitterMode>;
	rtt?: Signal<number | undefined>;
	audio?: Time.Milli | Signal<Time.Milli | undefined>;
	video?: Time.Milli | Signal<Time.Milli | undefined>;
}

export class Sync {
	// The earliest time we've received a frame, relative to its timestamp.
	// This will keep being updated as we catch up to the live playhead then will be relatively static.
	#reference = new Signal<Time.Milli | undefined>(undefined);
	readonly reference: Signal<Time.Milli | undefined> = this.#reference;

	// The jitter mode: a fixed number or "real-time" for dynamic RTT-based jitter.
	jitter: Signal<JitterMode>;

	// Any additional delay required for audio or video.
	audio: Signal<Time.Milli | undefined>;
	video: Signal<Time.Milli | undefined>;

	// The buffer required, based on both audio and video.
	#latency = new Signal<Time.Milli>(Time.Milli.zero);
	readonly latency: Signal<Time.Milli> = this.#latency;

	// A ghetto way to learn when the reference/latency changes.
	// There's probably a way to use Effect, but lets keep it simple for now.
	#update: PromiseWithResolvers<void>;

	// Per-label late-frame tracking: accumulate count and max lateness, flush on recovery.
	#late = new Map<string, { count: number; maxMs: number }>();

	// RTT signal from the connection (PROBE).
	#rtt?: Signal<number | undefined>;

	// EWMA state for RTT smoothing (RFC 9002 Section 5.3).
	#smoothedRtt: number | undefined;
	#rttVar: number | undefined;

	// The computed jitter value, always a number.
	#computedJitter = new Signal<Time.Milli>(FALLBACK_JITTER);
	readonly computedJitter: Signal<Time.Milli> = this.#computedJitter;

	signals = new Effect();

	constructor(props?: SyncProps) {
		this.jitter = Signal.from(props?.jitter ?? ("real-time" as JitterMode));
		this.#rtt = props?.rtt;
		this.audio = Signal.from(props?.audio);
		this.video = Signal.from(props?.video);

		this.#update = Promise.withResolvers();

		this.signals.run(this.#runJitter.bind(this));
		this.signals.run(this.#runLatency.bind(this));
	}

	#runJitter(effect: Effect): void {
		const mode = effect.get(this.jitter);

		if (typeof mode === "number") {
			this.#smoothedRtt = undefined;
			this.#rttVar = undefined;
			this.#computedJitter.set(mode);
			return;
		}

		// "real-time" mode: compute jitter from RTT.
		if (this.#rtt) {
			const rtt = effect.get(this.#rtt);
			if (rtt !== undefined) {
				const prev = this.#smoothedRtt;
				const { smoothed, variation } = this.#updateRtt(rtt);

				const jitter = Math.max(MIN_JITTER, smoothed / 2 + 4 * variation) as Time.Milli;
				this.#computedJitter.set(jitter);

				// Reset the reference when RTT drops so we re-anchor to the lower baseline.
				if (prev !== undefined && smoothed < prev * 0.8) {
					this.#reference.set(undefined);
				}

				return;
			}
		}

		// No RTT available: fall back to static default.
		this.#smoothedRtt = undefined;
		this.#rttVar = undefined;
		this.#computedJitter.set(FALLBACK_JITTER);
	}

	// RFC 9002 Section 5.3 EWMA update. Returns the updated values.
	#updateRtt(sample: number): { smoothed: number; variation: number } {
		if (this.#smoothedRtt === undefined || this.#rttVar === undefined) {
			this.#smoothedRtt = sample;
			this.#rttVar = sample / 2;
		} else {
			this.#rttVar = 0.75 * this.#rttVar + 0.25 * Math.abs(this.#smoothedRtt - sample);
			this.#smoothedRtt = 0.875 * this.#smoothedRtt + 0.125 * sample;
		}
		return { smoothed: this.#smoothedRtt, variation: this.#rttVar };
	}

	#runLatency(effect: Effect): void {
		const jitter = effect.get(this.#computedJitter);
		const video = effect.get(this.video) ?? Time.Milli.zero;
		const audio = effect.get(this.audio) ?? Time.Milli.zero;

		const latency = Time.Milli.add(Time.Milli.max(video, audio), jitter);
		this.#latency.set(latency);

		this.#update.resolve();
		this.#update = Promise.withResolvers();
	}

	// Update the reference if this is the earliest frame we've seen, relative to its timestamp.
	received(timestamp: Time.Milli, label = ""): void {
		const now = Time.Milli.now();
		const ref = Time.Milli.sub(now, timestamp);
		const currentRef = this.#reference.peek();

		if (currentRef !== undefined) {
			// Check if `wait()` would not sleep at all.
			// NOTE: We check here instead of in `wait()` so we can identify when frames are received late.
			// Otherwise, chained `wait()` calls would cause a false-positive during CPU starvation.
			const sleep = Time.Milli.add(Time.Milli.sub(currentRef, ref), this.#latency.peek());
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
					console.warn(`${prefix}: ${entry.count} late frame(s), max ${behind} behind`);
					this.#late.delete(label);
				}
			}

			if (ref >= currentRef) {
				// Our frame was not relatively newer than any other frame.
				return;
			}
		}

		this.#reference.set(ref);
		this.#update.resolve();
		this.#update = Promise.withResolvers();
	}

	// Sleep until it's time to render this frame.
	async wait(timestamp: Time.Milli): Promise<void> {
		const reference = this.#reference.peek();
		if (reference === undefined) {
			throw new Error("reference not set; call update() first");
		}

		for (;;) {
			// Sleep until it's time to decode the next frame.
			// NOTE: This function runs in parallel for each frame.
			const now = Time.Milli.now();
			const ref = Time.Milli.sub(now, timestamp);

			const currentRef = this.#reference.peek();
			if (currentRef === undefined) return;

			const sleep = Time.Milli.add(Time.Milli.sub(currentRef, ref), this.#latency.peek());
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
		this.signals.close();
	}
}
