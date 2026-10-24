import { Milli, Second } from "@moq/clock";

/** The terms of the audio playout target, all in milliseconds. */
export interface Target {
	/** The arrival estimate from the container consumer. */
	measured: Milli;
	/** The batch or reorder span the rendition advertises as `jitter`, if any. */
	advertised?: Milli;
	/** The codec's frame duration, if known. */
	frame?: Milli;
	/** The catalog `delay`: how far this rendition trails the broadcast's others, if any. */
	delay?: Milli;
}

/**
 * The "auto" playout target: the measured term floored by the advertised span, plus one frame, plus
 * the rendition's catalog delay.
 *
 * The advertised span is a floor rather than an addend, because the receiver's measurement already
 * contains the publisher's batching. The catalog delay is an addend, because the measurement is
 * taken against the track's own fastest frame and cancels any offset between tracks. See
 * doc/concept/audio-jitter.md.
 */
export function target(props: Target): Milli {
	const floored = Math.max(props.measured, props.advertised ?? Milli.zero);
	return Milli(floored + (props.frame ?? Milli.zero) + (props.delay ?? Milli.zero));
}

/**
 * The least subscription max delay an "auto" track asks for: the estimator's ceiling.
 *
 * The measured term saturates at 100 buckets of 20 ms, so a frame later than this adds nothing.
 */
export const AUTO_MAX_DELAY = 2000 as Milli;

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
export function ringSamples(rate: number, delay: Milli): number {
	return Math.max(RENDER_QUANTUM, Math.ceil(rate * Second.fromMilli(delay)));
}
