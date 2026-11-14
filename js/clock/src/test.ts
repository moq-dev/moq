/**
 * Test helpers for code that reads time through @moq/clock.
 *
 * @module
 */

import { guard } from "./host.ts";

export { Manual } from "./manual.ts";

/**
 * Let the current test's timers fire on the host clock, for real sockets, devices, or processes.
 * Call it in the test or a `beforeEach`; it does nothing without the `@moq/clock` test preload.
 */
export function hostTime(): void {
	if (guard.test) guard.test.real = true;
}
