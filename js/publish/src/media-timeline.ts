// The page's media timeline behind `MediaClock`, with what other modules of this package read
// from it that the public namespace leaves out.

import { Instant, now as instant, origin, wall } from "@moq/clock";
import { Slew } from "./slew";

// The timeline, and the installed clock's origin it counts from: another clock starts a new one.
let current: { origin: number; slew: Slew } | undefined;

/** The current instant, and what the media clock reads at it in microseconds. */
export function read(): { at: Instant; reading: number } {
	const at = instant();
	const zero = origin().asUnixMillis();
	if (current?.origin !== zero) current = { origin: zero, slew: new Slew() };
	// Where the wall clock puts this instant, in microseconds from instant zero.
	const target = (wall().asUnixMillis() - zero) * 1000;
	return { at, reading: current.slew.read(at.since(Instant.ZERO).asMicros(), target) };
}

/** How far the media clock has jumped across suspends, in microseconds, as of its last read. */
export function jumped(): number {
	return current?.slew.jumped ?? 0;
}
