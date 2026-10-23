import { Micro, Milli, Second } from "@moq/clock";

/** The terms of the audio playout target, all in milliseconds. */
export interface Target {
	/** The arrival estimate from the container consumer. */
	measured: Milli;
	/** The flush span the rendition advertises, if any. */
	advertised?: Milli;
	/** The codec's frame duration, if known. */
	frame?: Milli;
	/** The catalog `delay`: how far this rendition trails the broadcast's earliest one, if any. */
	delay?: Milli;
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

// The most of the target the ring holds decoded. The rest stays encoded upstream until the playhead
// nears it, so a catalog advertising an hour of jitter costs the bytes actually received rather than
// an hour of PCM. Comfortably above a main-thread stall or a state-message interval.
const FLOOR_MAX = Milli(500);

/**
 * The ring's capacity in samples: twice the deepest floor, so a ring sitting on its floor has a
 * whole floor of headroom for a burst. Fixed by the receiver, never by the target.
 */
export function ringCapacity(rate: number): number {
	return Math.ceil(rate * Second.fromMilli(FLOOR_MAX) * 2);
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
export function ringSamples(rate: number, delay: Milli): number {
	return Math.max(RENDER_QUANTUM, Math.ceil(rate * Second.fromMilli(Milli(Math.min(delay, FLOOR_MAX)))));
}

/**
 * The part of a target delay above the ring's floor, held encoded upstream rather than decoded.
 *
 * Measured against the cap rather than the rounded floor, so a target within the cap holds exactly
 * nothing instead of the float residue of a sample count.
 */
export function ringHold(delay: Milli): Micro {
	return Micro.fromMilli(Milli(Math.max(0, delay - FLOOR_MAX)));
}
