/**
 * Logical monotonic time, wall time, and timers from one clock per JS realm.
 * Install a Manual clock for deterministic tests without replacing host timers.
 *
 * @module
 */

import { backend } from "./ambient.ts";
import { Wall } from "./types.ts";

export type { Installed } from "./ambient.ts";
export { Manual } from "./manual.ts";
export { Elapsed, Interval, interval, MissedTickBehavior, now, sleep, sleepUntil, timeout, yieldNow } from "./timer.ts";
export { Instant, Micro, Milli, Nano, Second, Wall } from "./types.ts";

/** Read the installed clock's wall time in Unix milliseconds. */
export function wall(): Wall {
	return Wall(backend().wall());
}

/**
 * Read the installed clock's wall time at instant zero, in Unix milliseconds.
 * Rust's `moq_clock::origin()` pairs a nanosecond `Wall` with an instant instead.
 */
export function origin(): Wall {
	return Wall(backend().origin());
}
