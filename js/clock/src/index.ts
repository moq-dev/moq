/**
 * Logical monotonic time, wall time, and timers from one clock per JS realm.
 * Tests install a `Manual` clock from `@moq/clock/test` without replacing host timers.
 *
 * @module
 */

import { backend } from "./ambient.ts";
import { type Wall, wallOf } from "./types.ts";

export type { Installed } from "./ambient.ts";
export {
	Elapsed,
	Interval,
	interval,
	MissedTickBehavior,
	now,
	sleep,
	sleepUntil,
	timeout,
	timeoutSignal,
	yieldNow,
} from "./timer.ts";
export { Duration, Instant, Wall } from "./types.ts";

/** Read the installed clock's wall time. */
export function wall(): Wall {
	return wallOf(backend().wall());
}

/** Read the installed clock's wall time at `Instant.ZERO`. */
export function origin(): Wall {
	return wallOf(backend().origin());
}
