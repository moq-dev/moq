import type { Reader, Writer } from "../stream.ts";
import * as Message from "./message.ts";

/// Sent to gracefully shut down a session and optionally redirect to a new URI.
///
/// Lite04+ only.
export class Goaway {
	uri: string;

	constructor(uri: string) {
		this.uri = uri;
	}

	async #encode(w: Writer) {
		await w.string(this.uri);
	}

	static async #decode(r: Reader): Promise<Goaway> {
		return new Goaway(await r.string());
	}

	async encode(w: Writer): Promise<void> {
		return Message.encode(w, this.#encode.bind(this));
	}

	static async decode(r: Reader): Promise<Goaway> {
		return Message.decode(r, Goaway.#decode);
	}
}
