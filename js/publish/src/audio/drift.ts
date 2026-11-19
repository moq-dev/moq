import { Duration } from "@moq/clock";
import type { AudioFrame } from "./capture";
import { Resampler } from "./resampler";

// Keeps captured audio on the media clock its video is stamped on.
//
// An AudioContext renders on its output device's clock, or on a host timer when it renders to no
// device, and either runs tens of ppm off the media clock, which slews toward UTC. The capture stamps
// by sample count, so left alone a 100 ppm context drifts 0.36 s against its video every hour.
// `Drift` compares each quantum's capture time with the timestamp its first sample will get, and
// steers a resampler's ratio until the two agree. Mirrors `rs/moq-audio/src/encode/drift.rs`.

// How much input each measurement spans, in microseconds. A quantum's capture time is read when the
// audio thread renders it, so it is only ever late, delayed by the render callback; the earliest
// error in a window is the one that measures the clocks rather than the delivery.
const WINDOW = 1_000_000;

// The loop's time constant, in windows: a critically damped loop whose double pole sits at
// `1 - 1 / TAU`. Ten seconds settles within a minute, and holds a 1000 ppm clock within 8 ms while
// it learns the rate.
const TAU = 10;

// The most the ratio moves from nominal: 2000 ppm, twice the 1000 ppm a clock may be off, as native
// capture allows.
const MAX_ADJUST = 2e-3;

// An error, in microseconds, too large to slew at `MAX_ADJUST`: ffmpeg's `min_hard_comp` default.
// Behind by more for a whole window, time passed that no sample covers, such as a host whose
// monotonic clock counted a sleep the context did not, and the timeline re-anchors so the gap lands
// in the timestamps. Ahead by more, the audio already stamped claims time not yet captured, so input
// is dropped until the stamps catch up. Ahead by more in only part of a window is a misreading,
// such as the wall clock stepping while a quantum was in flight, and the window is ignored.
const RESYNC = 100_000;

/** What {@link Drift.observe} asks of the capture. */
export type Steer =
	// Stamp the quantum at the current ratio.
	| { kind: "hold" }
	// Stamp the quantum at this ratio relative to nominal.
	| { kind: "ratio"; ratio: number }
	// Re-anchor on this quantum's capture time, then stamp it.
	| { kind: "resync" }
	// Drop the quantum: the audio already stamped covers its time.
	| { kind: "drop" };

type State =
	// Measuring the current window. Errors are in microseconds, positive when samples were captured
	// later than their timestamp.
	| { kind: "tracking"; low: number; high: number; covered: number }
	// The stamped audio runs ahead of capture: input is dropped until it catches up.
	| { kind: "ahead" };

const tracking = (): State => ({ kind: "tracking", low: Infinity, high: -Infinity, covered: 0 });

/** The resample ratio that cancels a capture clock's drift against the media clock. */
export class Drift {
	#state: State = tracking();
	// The integral term: the clock's rate offset as learned so far, kept across re-anchors since the
	// context's clock does not change with the timeline.
	#rate = 0;

	/**
	 * A quantum `duration` long was captured at `captured`, and its first sample will be stamped `pts`,
	 * both in microseconds on the media clock: decide what to do with it, steering once per window.
	 */
	observe(captured: number, pts: number, duration: number): Steer {
		const error = captured - pts;
		const state = this.#state;
		if (state.kind === "ahead") {
			if (error < 0) return { kind: "drop" };
			this.#state = tracking();
			return { kind: "resync" };
		}

		state.low = Math.min(state.low, error);
		state.high = Math.max(state.high, error);
		state.covered += duration;
		if (state.covered < WINDOW) return { kind: "hold" };

		const { low, high } = state;
		this.#state = tracking();
		if (low > RESYNC) return { kind: "resync" };
		if (high < -RESYNC) {
			this.#state = { kind: "ahead" };
			return { kind: "drop" };
		}
		if (low < -RESYNC) return { kind: "hold" };

		// Critically damped PI on the error per window: 2 / TAU proportional and 1 / TAU² integral
		// gain. The integral clamps with the ratio, so a clock past the bound cannot wind it up.
		const lag = low / WINDOW;
		this.#rate = clamp(this.#rate + lag / (TAU * TAU), MAX_ADJUST);
		return { kind: "ratio", ratio: 1 + clamp(this.#rate + (2 / TAU) * lag, MAX_ADJUST) };
	}

	/** A new timeline begins: measure afresh, keeping the learned rate. */
	restart(): void {
		this.#state = tracking();
	}
}

function clamp(value: number, bound: number): number {
	return Math.min(Math.max(value, -bound), bound);
}

/**
 * Stamps a capture's quanta on the media clock: by sample count from an anchor, resampled by a
 * {@link Drift} loop so the count keeps pace with when each quantum was captured.
 */
export class Stamper {
	readonly #rate: number;
	readonly #drift = new Drift();
	#resampler?: { channels: number; resampler: Resampler };

	// The media time the timeline starts at, unset until the next quantum anchors it, and the samples
	// stamped since, all in microseconds but the count.
	#anchor?: number;
	#stamped = 0;
	// Where the stamped audio ends, so a re-anchor never moves the timeline back.
	#end = 0;
	// The media clock's suspend jumps already added to the anchor.
	#jumped = 0;

	constructor(sampleRate: number) {
		this.#rate = sampleRate;
	}

	/**
	 * A quantum whose first sample was captured at `captured` on the media clock, which has jumped
	 * `jumped` in total across suspends: the frame to publish, if it completes any samples.
	 */
	push(channels: Float32Array[], captured: number, jumped: number): AudioFrame | undefined {
		const samples = channels[0]?.length ?? 0;
		if (samples === 0) return undefined;

		// A suspend the media clock jumped is time no sample covers: the timeline jumps it too.
		if (this.#anchor !== undefined) this.#anchor += jumped - this.#jumped;
		this.#jumped = jumped;

		// The channel count moved (Safari reports what it likes): the filter's state is for the old
		// layout, so start over on a new timeline.
		if (this.#resampler?.channels !== channels.length) {
			const resampler = new Resampler({ from: this.#rate, to: this.#rate, channels: channels.length });
			this.#resampler = { channels: channels.length, resampler };
			this.#anchor = undefined;
			this.#drift.restart();
		}
		const { resampler } = this.#resampler;

		if (this.#anchor !== undefined) {
			const steer = this.#drift.observe(captured, this.#at(this.#stamped), (samples / this.#rate) * 1_000_000);
			if (steer.kind === "drop") return undefined;
			if (steer.kind === "ratio") resampler.adjust(steer.ratio);
			if (steer.kind === "resync") this.#anchor = undefined;
		}
		if (this.#anchor === undefined) {
			this.#anchor = Math.max(captured, this.#end);
			this.#stamped = 0;
		}

		const output = resampler.convert(channels);
		if (!output) return undefined;
		const timestamp = Duration.micros(this.#at(this.#stamped));
		this.#stamped += output[0].length;
		this.#end = this.#at(this.#stamped);
		return { timestamp, channels: output };
	}

	// The timestamp of the `n`th sample since the anchor, in microseconds.
	#at(n: number): number {
		return (this.#anchor ?? 0) + (n / this.#rate) * 1_000_000;
	}
}
