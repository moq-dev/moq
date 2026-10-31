import { parse } from "../parse.ts";
import type { Config } from "./encoder.ts";

/**
 * Decodes JSON records from frame payloads.
 *
 * The track-free core of {@link Consumer}, and the mirror of {@link Encoder}.
 */
export class Decoder<T> {
	#schema?: Config<T>["schema"];

	constructor(config: Config<T> = {}) {
		this.#schema = config.schema;
	}

	/** Decode the next frame payload back into a record. */
	decode(payload: Uint8Array): T {
		const value = parse(payload);
		return this.#schema ? this.#schema.parse(value) : (value as T);
	}
}
