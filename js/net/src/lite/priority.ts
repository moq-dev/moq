/**
 * Priority conversion between the moq-lite wire and the model, mirroring the Rust
 * `lite::priority`.
 *
 * The wire carries `priority + 128`: one-to-one and order preserving, so a byte means what it
 * always meant and the unset priority 0 is byte 128.
 *
 * @module
 */

/** Convert a moq-lite priority byte into the model's priority. */
export function fromWire(byte: number): number {
	return byte - 128;
}

/** Convert the model's priority into a moq-lite priority byte. */
export function toWire(priority: number): number {
	return priority + 128;
}
