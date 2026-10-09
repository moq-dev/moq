/**
 * A value paired with when it was captured.
 *
 * @module
 */
import type { Timestamp } from "./time.ts";

/** A value, and optionally when it was captured. An absent `at` means untimed, never now. */
export interface Timed<T> {
	/** The value. */
	value: T;
	/** When the value was captured, written as its frame's timestamp. Absent for an untimed track. */
	at?: Timestamp;
}
