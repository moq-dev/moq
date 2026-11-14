import { Duration, type Instant } from "@moq/clock";

/** The terms of the audio playout target. */
export interface Target {
	/** The arrival estimate from the container consumer. */
	measured: Duration;
	/** The batch or reorder span the rendition advertises as `jitter`, if any. */
	advertised?: Duration;
	/** The codec's frame duration, if known. */
	frame?: Duration;
	/** The catalog `delay`: how far this rendition trails the broadcast's others, if any. */
	delay?: Duration;
	/** How long the output takes to sound what the ring renders, once known. */
	output?: Duration;
}

/**
 * The "auto" playout target: the measured term floored by the advertised span, plus one frame, plus
 * the rendition's catalog delay, plus the output latency.
 *
 * The advertised span is a floor rather than an addend, because the receiver's measurement already
 * contains the publisher's batching. The catalog delay is an addend, because the measurement is
 * taken against the track's own fastest frame and cancels any offset between tracks. See
 * doc/concept/audio-jitter.md.
 */
export function target(props: Target): Duration {
	const floored = props.measured.max(props.advertised ?? Duration.ZERO);
	return floored
		.add(props.frame ?? Duration.ZERO)
		.add(props.delay ?? Duration.ZERO)
		.add(props.output ?? Duration.ZERO);
}

// How long the highest reported output latency holds. A browser's report moves with where each read
// lands, so the delay the clock holds for it follows the peak rather than every reading, and falls
// once a lower latency, as after a switch off a Bluetooth headset, has held this long.
const OUTPUT_WINDOW = Duration.secs(5);

/** The highest output latency reported within the last five seconds. */
export class Peak {
	// Each reading that no later one is at least as high as, oldest first, so the first is the peak.
	#readings: { at: Instant; latency: Duration }[] = [];

	/** Fold in the latency reported at `at`, returning the peak. */
	observe(latency: Duration, at: Instant): Duration {
		while (this.#readings.length > 0 && (this.#readings.at(-1)?.latency.le(latency) ?? false)) this.#readings.pop();
		this.#readings.push({ at, latency });
		while (this.#readings.length > 1 && at.since(this.#readings[0].at).gt(OUTPUT_WINDOW)) this.#readings.shift();
		return this.#readings[0].latency;
	}
}

/**
 * The audio subscription's max delay, less warmup: the shared `maxDelay`, raised to `delay + stretch`
 * so audio trailing the clock by up to the stretch still arrives for the ring to catch up on.
 */
export function maxDelay(props: { delay: Duration; maxDelay: Duration; stretch: Duration }): Duration {
	return props.maxDelay.max(props.delay.add(props.stretch));
}

/**
 * How far the ring lets audio trail the clock before it skips ahead: the audio max delay less the
 * playout delay, or zero, which turns the time stretch off, when `stretch` is zero.
 */
export function bound(props: { delay: Duration; maxDelay: Duration; stretch: Duration }): Duration {
	return props.stretch.isZero() ? Duration.ZERO : maxDelay(props).saturatingSub(props.delay);
}

// An AudioWorkletProcessor renders in fixed 128-sample quanta, so a ring shallower than one can
// never be read from.
const RENDER_QUANTUM = 128;

// The most of the target the ring holds decoded. The rest stays encoded upstream until the playhead
// nears it, so a catalog advertising an hour of jitter costs the bytes actually received rather than
// an hour of PCM. Comfortably above a main-thread stall or a state-message interval.
const FLOOR_MAX = Duration.millis(500);

/**
 * The ring's capacity in samples: twice the deepest floor, so a ring sitting on its floor has a
 * whole floor of headroom for a burst. Fixed by the receiver, never by the target.
 */
export function ringCapacity(rate: number): number {
	return Math.ceil(rate * FLOOR_MAX.asSecs() * 2);
}

/**
 * The ring's floor for a target delay: the part held as decoded PCM, at least one AudioWorklet render
 * quantum and at most 500 ms.
 *
 * `delay="instant"` reports a zero buffer, which the ring rejects outright: construction throws,
 * the worklet is left with no backend, and every later update is gated on that backend existing, so
 * audio never recovers. Audio keeps its own floor rather than deriving its depth verbatim from a
 * buffer whose meaning is "video holds nothing".
 */
export function ringSamples(rate: number, delay: Duration): number {
	return Math.max(RENDER_QUANTUM, Math.ceil(rate * delay.min(FLOOR_MAX).asSecs()));
}

/**
 * The part of a target delay above the ring's floor, held encoded upstream rather than decoded.
 *
 * Measured against the cap rather than the rounded floor, so a target within the cap holds exactly
 * nothing instead of the float residue of a sample count.
 */
export function ringHold(delay: Duration): Duration {
	return delay.saturatingSub(FLOOR_MAX);
}
