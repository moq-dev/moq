import { Time } from "@moq/net";

/** The terms of the audio playout target, all in milliseconds. */
export interface Target {
	/** The arrival estimate from the container consumer. */
	measured: Time.Milli;
	/** The flush span the rendition advertises, if any. */
	advertised?: Time.Milli;
	/** The codec's frame duration, if known. */
	frame?: Time.Milli;
}

/**
 * The "auto" playout target: the measured term floored by the advertised span, plus one frame.
 *
 * A floor rather than a sum, because the receiver's measurement already contains the publisher's
 * flush delay. See doc/concept/audio-jitter.md.
 */
export function target(props: Target): Time.Milli {
	const floored = Time.Milli.max(props.measured, props.advertised ?? Time.Milli.zero);
	return Time.Milli.add(floored, props.frame ?? Time.Milli.zero);
}

/** Whether a deeper target re-stalls the ring, and the baseline the next target is compared against. */
export interface Reanchor {
	stall: boolean;
	baseline: Time.Milli;
}

/**
 * Compare a new target against the depth the ring last filled to.
 *
 * A rise of more than `step` re-stalls. A smaller one rides through but keeps the old baseline, so a
 * run of one-bucket rises still re-stalls once they add up. A fall lowers the baseline, since the
 * ring's latency skip follows the target down on its own.
 */
export function reanchor(baseline: Time.Milli, target: Time.Milli, step: Time.Milli): Reanchor {
	if (target - baseline > step) return { stall: true, baseline: target };
	return { stall: false, baseline: Time.Milli.min(baseline, target) };
}

// An AudioWorkletProcessor renders in fixed 128-sample quanta, so a ring shallower than one can
// never be read from.
const RENDER_QUANTUM = 128;

/**
 * The ring depth for a target delay, floored at one AudioWorklet render quantum.
 *
 * `delay="instant"` reports a zero buffer, which the ring rejects outright: construction throws,
 * the worklet is left with no backend, and every later resize is gated on that backend existing, so
 * audio never recovers. Audio keeps its own floor rather than deriving its depth verbatim from a
 * buffer whose meaning is "video holds nothing".
 */
export function ringSamples(rate: number, delay: Time.Milli): number {
	return Math.max(RENDER_QUANTUM, Math.ceil(rate * Time.Second.fromMilli(delay)));
}
