// The signed message: what a frame's signature binds besides its payload.
import type { Epoch, Path, Time } from "@moq/net";
import { Failure } from "./failure.ts";
import type { PublicKey, SecretKey } from "./key.ts";

// Length of the Ed25519 signature appended to every signed frame.
const SIGNATURE_LEN = 64;

const encoder = new TextEncoder();
// Domain separation for the profile, with no length prefix of its own.
const LABEL = encoder.encode("moq-sign-00");

/** One track of one publisher instance: the identity every frame on it is signed under. */
export class Scope {
	// The canonical prefix: label, path, epoch, and track, each length-prefixed.
	#prefix: Uint8Array<ArrayBuffer>;

	constructor(path: Path.Valid, epoch: Epoch.Valid, track: string) {
		const fields = [path, epoch, track].map((field) => encoder.encode(field));
		this.#prefix = new Uint8Array(LABEL.byteLength + fields.reduce((sum, field) => sum + 8 + field.byteLength, 0));
		const view = new DataView(this.#prefix.buffer);
		this.#prefix.set(LABEL);
		let offset = LABEL.byteLength;
		for (const field of fields) {
			view.setBigUint64(offset, BigInt(field.byteLength));
			this.#prefix.set(field, offset + 8);
			offset += 8 + field.byteLength;
		}
	}

	/**
	 * The message signed for `payload` at frame `frame` of group `group`, presented at `timestamp`.
	 *
	 * The timestamp is its integer value at the track's timescale, so a frame keeps its signature
	 * only on a hop that carries both unchanged. An untimed frame binds zero for both, which no
	 * timescale can be. A fractional value, which no hop carries, never verifies.
	 */
	message(
		group: number,
		frame: number,
		timestamp: Time.Timestamp | undefined,
		payload: Uint8Array,
	): Uint8Array<ArrayBuffer> {
		if (timestamp && !Number.isSafeInteger(timestamp.value)) throw new Failure("signature");
		const out = new Uint8Array(this.#prefix.byteLength + 32 + payload.byteLength);
		const view = new DataView(out.buffer);
		out.set(this.#prefix);
		let offset = this.#prefix.byteLength;
		for (const field of [group, frame, timestamp?.scale ?? 0, timestamp?.value ?? 0]) {
			view.setBigUint64(offset, BigInt(field));
			offset += 8;
		}
		out.set(payload, offset);
		return out;
	}

	/** The frame body: `payload` followed by its signature. */
	async sign(
		key: SecretKey,
		group: number,
		frame: number,
		timestamp: Time.Timestamp | undefined,
		payload: Uint8Array,
	): Promise<Uint8Array<ArrayBuffer>> {
		const signature = await key.sign(this.message(group, frame, timestamp, payload));
		const body = new Uint8Array(payload.byteLength + SIGNATURE_LEN);
		body.set(payload);
		body.set(signature, payload.byteLength);
		return body;
	}

	/** The payload of a frame body whose signature verifies, as a view into `body`; throws {@link Failure} `signature`. */
	async verify(
		key: PublicKey,
		group: number,
		frame: number,
		timestamp: Time.Timestamp | undefined,
		body: Uint8Array,
	): Promise<Uint8Array> {
		const split = body.byteLength - SIGNATURE_LEN;
		if (split < 0) throw new Failure("signature");
		const payload = body.subarray(0, split);
		if (!(await key.verify(this.message(group, frame, timestamp, payload), body.slice(split)))) {
			throw new Failure("signature");
		}
		return payload;
	}
}
