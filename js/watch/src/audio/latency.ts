import { Time } from "@moq/net";

/** The terms of the audio playout target, all in milliseconds. */
export interface Target {
	/** The arrival estimate from the container consumer. */
	measured: Time.Milli;
	/** The flush span the rendition advertises, if any. */
	advertised?: Time.Milli;
	/** The codec's frame duration, if known. */
	frame?: Time.Milli;
	/** The catalog `delay`: how far this rendition trails the broadcast's earliest one, if any. */
	delay?: Time.Milli;
}

/**
 * The "auto" playout target: the measured term floored by the advertised span, plus one frame, plus
 * the rendition's catalog delay.
 *
 * The advertised span is a floor rather than an addend, because the receiver's measurement already
 * contains the publisher's flush delay. The catalog delay is an addend, because the measurement is
 * taken against the track's own fastest frame and cancels any offset between tracks. See
 * doc/concept/audio-jitter.md.
 */
export function target(props: Target): Time.Milli {
	const floored = Time.Milli.max(props.measured, props.advertised ?? Time.Milli.zero);
	const own = Time.Milli.add(floored, props.frame ?? Time.Milli.zero);
	return Time.Milli.add(own, props.delay ?? Time.Milli.zero);
}

/**
 * The least subscription max delay an "auto" track asks for: the estimator's ceiling.
 *
 * The measured term saturates at 100 buckets of 20 ms, so a frame later than this adds nothing.
 */
export const AUTO_MAX_DELAY = 2000 as Time.Milli;

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
