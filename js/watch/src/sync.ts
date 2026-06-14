import { Effect, type Getter, getter, type Inputs, type Readonlys, readonlys, Signal } from "@moq/signals";
import type * as Moq from "@moq/wasm";
import { Time } from "@moq/wasm";

/** Latency: `"real-time"` auto-computes jitter from RTT; a `Time.Milli` sets a fixed jitter. */
export type Latency = "real-time" | Time.Milli;

const MIN_JITTER = Time.Milli(20);
const FALLBACK_JITTER = Time.Milli(100);

type SyncInput = {
	// The latency setting: "real-time" auto-computes jitter from RTT, a number sets a fixed jitter.
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
	// When latency is a number, jitter equals that number.
	jitter: Signal<Time.Milli>;

	// The media timestamp of the most recently received frame.
	timestamp: Signal<Time.Milli | undefined>;
};

export class Sync {
	readonly input: Readonlys<SyncInput>;

	readonly #output: SyncOutput = {
		reference: new Signal<Time.Milli | undefined>(undefined),
		buffer: new Signal<Time.Milli>(Time.Milli.zero),
		jitter: new Signal<Time.Milli>(FALLBACK_JITTER),
		timestamp: new Signal<Time.Milli | undefined>(undefined),
	};
	readonly output = readonlys(this.#output);

	// A ghetto way to learn when the reference/buffer changes.
	// There's probably a way to use Effect, but lets keep it simple for now.
	#update: PromiseWithResolvers<void>;

	// Per-label late-frame tracking: accumulate count and max lateness, flush on recovery.
	#late = new Map<string, { count: number; maxMs: number }>();

	// Minimum RTT seen, used as the baseline for jitter calculation.
	// Avoids inflating jitter due to bufferbloat.
	#minRtt: number | undefined;

	signals = new Effect();

	constructor(props?: Inputs<SyncInput>) {
		this.input = {
			latency: getter(props?.latency ?? ("real-time" as Latency)),
			connection: getter(props?.connection),
			audio: getter(props?.audio),
			video: getter(props?.video),
		};

		this.#update = Promise.withResolvers();

		this.signals.run(this.#runJitter.bind(this));
		this.signals.run(this.#runBuffer.bind(this));
	}

	#runJitter(effect: Effect): void {
		const latency = effect.get(this.input.latency);

		if (typeof latency === "number") {
			// Fixed mode: latency value is the jitter.
			this.#minRtt = undefined;
			this.#output.jitter.set(latency);
			return;
		}

		// "real-time" mode: compute jitter from RTT on the established connection.
		const conn = effect.get(this.input.connection);
		const rttSignal = conn?.rtt;
		const rtt = rttSignal ? effect.get(rttSignal) : undefined;
		if (rtt !== undefined) {
			// Track minimum RTT as baseline, ignoring bufferbloat.
			this.#minRtt = this.#minRtt !== undefined ? Math.min(this.#minRtt, rtt) : rtt;

			// Buffer enough for a retransmit (1 RTT for ACK + retransmit).
			const jitter = Time.Milli(Math.max(MIN_JITTER, this.#minRtt * 1.25));
			this.#output.jitter.set(jitter);
			return;
		}

		// No RTT available: fall back to static default.
		this.#minRtt = undefined;
		this.#output.jitter.set(FALLBACK_JITTER);
	}

	#runBuffer(effect: Effect): void {
		const jitter = effect.get(this.#output.jitter);
		const video = effect.get(this.input.video) ?? Time.Milli.zero;
		const audio = effect.get(this.input.audio) ?? Time.Milli.zero;

		const buffer = Time.Milli.add(Time.Milli.max(video, audio), jitter);
		this.#output.buffer.set(buffer);

		this.#update.resolve();
		this.#update = Promise.withResolvers();
	}

	// Update the reference if this is the earliest frame we've seen, relative to its timestamp.
	received(timestamp: Time.Milli, label = ""): void {
		this.#output.timestamp.update((current) =>
			current === undefined || timestamp > current ? timestamp : current,
		);
		const now = Time.Milli.now();
		const ref = Time.Milli.sub(now, timestamp);
		const currentRef = this.#output.reference.peek();

		if (currentRef !== undefined) {
			// Check if `wait()` would not sleep at all.
			// NOTE: We check here instead of in `wait()` so we can identify when frames are received late.
			// Otherwise, chained `wait()` calls would cause a false-positive during CPU starvation.
			const sleep = Time.Milli.add(Time.Milli.sub(currentRef, ref), this.#output.buffer.peek());
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

			if (ref >= currentRef) {
				// Our frame was not relatively newer than any other frame.
				return;
			}
		}

		this.#output.reference.set(ref);
		this.#update.resolve();
		this.#update = Promise.withResolvers();
	}

	// The PTS that should be rendering right now, derived from the reference + buffer.
	// Returns undefined if no frames have been received yet.
	now(): Time.Milli | undefined {
		const reference = this.#output.reference.peek();
		if (reference === undefined) return undefined;
		return Time.Milli.sub(Time.Milli.sub(Time.Milli.now(), reference), this.#output.buffer.peek());
	}

	// Sleep until it's time to render this frame.
	async wait(timestamp: Time.Milli): Promise<void> {
		const reference = this.#output.reference.peek();
		if (reference === undefined) {
			throw new Error("reference not set; call update() first");
		}

		for (;;) {
			// Sleep until it's time to decode the next frame.
			// NOTE: This function runs in parallel for each frame.
			const now = Time.Milli.now();
			const ref = Time.Milli.sub(now, timestamp);

			const currentRef = this.#output.reference.peek();
			if (currentRef === undefined) return;

			const sleep = Time.Milli.add(Time.Milli.sub(currentRef, ref), this.#output.buffer.peek());
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
