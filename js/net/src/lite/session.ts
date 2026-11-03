import type { Reader, Writer } from "../stream.ts";
import * as Message from "./message.ts";
import { Version } from "./version.ts";

export class SessionInfo {
	bitrate: number;

	constructor(bitrate: number) {
		this.bitrate = bitrate;
	}

	static #guard(version: Version) {
		switch (version) {
			case Version.DRAFT_01:
			case Version.DRAFT_02:
				break;
			default:
				throw new Error("session info not supported for this version");
		}
	}

	async #encode(w: Writer) {
		await w.u53(this.bitrate);
	}

	static async #decode(r: Reader): Promise<SessionInfo> {
		const bitrate = await r.u53();
		return new SessionInfo(bitrate);
	}

	async encode(w: Writer, version: Version): Promise<void> {
		SessionInfo.#guard(version);
		return Message.encode(w, this.#encode.bind(this));
	}

	static async decode(r: Reader, version: Version): Promise<SessionInfo> {
		SessionInfo.#guard(version);
		return Message.decode(r, SessionInfo.#decode);
	}

	static async decodeMaybe(r: Reader, version: Version): Promise<SessionInfo | undefined> {
		SessionInfo.#guard(version);
		return Message.decodeMaybe(r, SessionInfo.#decode);
	}
}
