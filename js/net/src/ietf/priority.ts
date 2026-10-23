/**
 * Priority conversion between the moq-transport wire and the model, mirroring the Rust
 * `ietf::priority`.
 *
 * moq-transport ranks lower first, so the wire carries `127 - priority`: the unset priority 0
 * goes out as 127, and an absent priority decodes as 0.
 *
 * @module
 */

/** The wire byte for the unset priority 0, which an absent priority stands for. */
export const ABSENT_WIRE = 127;

/** Convert an IETF priority byte into the model's higher-first priority. */
export function fromWire(priority: number): number {
	return 127 - priority;
}

/** Convert the model's higher-first priority into an IETF priority byte. */
export function toWire(priority: number): number {
	return 127 - priority;
}
