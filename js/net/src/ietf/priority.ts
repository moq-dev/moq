/**
 * Priority conversion between the moq-transport wire and the model, mirroring the Rust
 * `ietf::priority`.
 *
 * moq-transport ranks lower first, so the wire carries `127 - priority`: the unset priority 0
 * goes out as 127, and an omitted priority decodes as -1, one step below it.
 *
 * @module
 */

/**
 * The IETF priority an omitted SUBSCRIBER_PRIORITY or DEFAULT_PUBLISHER_PRIORITY stands for, as
 * the drafts define both, and the byte for a message that must carry a priority it was not given.
 * It reads as -1, so a relay forwards an omitted priority as 128, not as the unset 127.
 */
export const DEFAULT_WIRE = 128;

/** Convert an IETF priority byte into the model's higher-first priority. */
export function fromWire(priority: number): number {
	return 127 - priority;
}

/** Convert the model's higher-first priority into an IETF priority byte. */
export function toWire(priority: number): number {
	return 127 - priority;
}
