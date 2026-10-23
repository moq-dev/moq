/**
 * Priority conversion between the moq-transport wire and the model, mirroring the Rust
 * `ietf::priority`.
 *
 * moq-transport ranks lower first, so the wire carries `127 - priority`: the unset priority 0
 * goes out as 127, and an absent subscriber priority decodes as 0.
 *
 * @module
 */

/** The wire byte for the unset priority 0, which an absent subscriber priority stands for. */
export const ABSENT_WIRE = 127;

/**
 * The publisher priority of a track whose publisher omits DEFAULT_PUBLISHER_PRIORITY (draft-16
 * section 11.1.1.1 on). Reading it as {@link ABSENT_WIRE} would forward that track at 127.
 */
export const DEFAULT_PUBLISHER_WIRE = 128;

/** Convert an IETF priority byte into the model's higher-first priority. */
export function fromWire(priority: number): number {
	return 127 - priority;
}

/** Convert the model's higher-first priority into an IETF priority byte. */
export function toWire(priority: number): number {
	return 127 - priority;
}
