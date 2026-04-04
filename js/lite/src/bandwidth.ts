import { Signal } from "@moq/signals";

/**
 * A bandwidth estimate in bits per second, or undefined if unknown.
 *
 * This is a Signal that can be read synchronously via `peek()`,
 * observed reactively via `effect.get()`, or updated via `set()`.
 */
export type Bandwidth = Signal<number | undefined>;

/** Create a new bandwidth signal. */
export function createBandwidth(): Bandwidth {
	return new Signal<number | undefined>(undefined);
}
