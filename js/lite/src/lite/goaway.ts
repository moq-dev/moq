import type { Reader, Writer } from "../stream.ts";
import * as Message from "./message.ts";
import { Version } from "./version.ts";

function guardGoaway(version: Version) {
	switch (version) {
		case Version.DRAFT_01:
		case Version.DRAFT_02:
		case Version.DRAFT_03:
			throw new Error("goaway not supported for this version");
		default:
			break;
	}
}

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

	async encode(w: Writer, version: Version): Promise<void> {
		guardGoaway(version);
		return Message.encode(w, this.#encode.bind(this));
	}

	static async decode(r: Reader, version: Version): Promise<Goaway> {
		guardGoaway(version);
		return Message.decode(r, Goaway.#decode);
	}
}
