import { Duration } from "@moq/clock";

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
export function target(props: Target): Duration {
	const floored = props.measured.max(props.advertised ?? Duration.ZERO);
	return floored.add(props.frame ?? Duration.ZERO).add(props.delay ?? Duration.ZERO);
}

// The arrival estimate's ceiling, `BUCKET * BUCKETS` in @moq/hang's jitter estimator: no lateness past
// it moves the target, so a subscription reaching that far past the delay observes all it can measure.
const REACH = Duration.secs(2);

/**
 * The audio subscription's max delay, less warmup: the shared `maxDelay`, raised to the delay plus the
 * estimator's 2 s ceiling, so every frame late enough to move the target is still observed. The
 * `stretch` setting does not size it.
 */
export function maxDelay(props: { delay: Duration; maxDelay: Duration }): Duration {
	return props.maxDelay.max(props.delay.add(REACH));
}

/**
 * How far the ring lets audio trail the clock before it skips onto it: the `stretch` threshold, or
 * for a buffered player the larger of it and the lookahead (`maxDelay` less the delay). Zero, which
 * turns the time stretch off, when `stretch` is zero.
 */
export function bound(props: { delay: Duration; maxDelay: Duration; stretch: Duration }): Duration {
	return props.stretch.isZero() ? Duration.ZERO : props.stretch.max(props.maxDelay.saturatingSub(props.delay));
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
