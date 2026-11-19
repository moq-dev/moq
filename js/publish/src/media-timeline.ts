// The page's media timeline behind `MediaClock`, with what other modules of this package read
// from it that the public namespace leaves out.

import { Instant, now as instant, origin, wall } from "@moq/clock";
import { Slew } from "./slew";

// The timeline, and the installed clock's origin it counts from: another clock starts a new one.
let current: { origin: number; slew: Slew } | undefined;

/** The current instant, and what the media clock reads at it in microseconds. */
export function read(): { at: Instant; reading: number; slew: Slew } {
	const at = instant();
	const zero = origin().asUnixMillis();
	if (current?.origin !== zero) current = { origin: zero, slew: new Slew() };
	const slew = current.slew;
	// Where the wall clock puts this instant, in microseconds from instant zero.
	const target = (wall().asUnixMillis() - zero) * 1000;
	return { at, reading: slew.read(at.since(Instant.ZERO).asMicros(), target), slew };
}

/**
 * Follow the media clock's suspend jumps from now on: each call reads the clock and returns how far
 * it has jumped since, in microseconds, so a source that keeps its own spacing, such as a sample
 * count, adds it to stay on the media clock across a sleep.
 */
export function follow(): () => number {
	let { slew } = read();
	let seen = slew.jumped;
	let total = 0;
	return () => {
		const now = read().slew;
		// Another clock installed since starts a new timeline, whose jumps count from zero.
		if (now !== slew) {
			slew = now;
			seen = 0;
		}
		total += slew.jumped - seen;
		seen = slew.jumped;
		return total;
	};
}
