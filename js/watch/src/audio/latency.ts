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
