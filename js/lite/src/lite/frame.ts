import type { Reader, Writer } from "../stream";
import * as Time from "../time.ts";
import * as Message from "./message.ts";
import { Version } from "./version.js";

export class Frame {
	timestamp: Time.Micro;
	payload: Uint8Array;

	constructor({ timestamp, payload }: { timestamp: Time.Micro; payload: Uint8Array }) {
		this.payload = payload;
		this.timestamp = timestamp;
	}

	async #encode(w: Writer, version: Version) {
		switch (version) {
			case Version.DRAFT_03:
				await w.u53(this.timestamp);
				break;
			case Version.DRAFT_01:
			case Version.DRAFT_02:
				break;
			default: {
				const v: never = version;
				throw new Error(`unsupported version: ${v}`);
			}
		}

		await w.write(this.payload);
	}

	static async #decode(r: Reader, version: Version): Promise<Frame> {
		let timestamp: Time.Micro;

		switch (version) {
			case Version.DRAFT_03:
				timestamp = (await r.u53()) as Time.Micro;
				console.log("timestamp", timestamp);
				break;
			case Version.DRAFT_01:
			case Version.DRAFT_02:
				timestamp = Time.Micro.now();
				break;
			default: {
				const v: never = version;
				throw new Error(`unsupported version: ${v}`);
			}
		}

		const payload = await r.readAll();
		return new Frame({ timestamp, payload });
	}

	async encode(w: Writer, v: Version): Promise<void> {
		return Message.encode(w, (w) => this.#encode(w, v));
	}

	static async decode(r: Reader, v: Version): Promise<Frame> {
		return Message.decode(r, (r) => Frame.#decode(r, v));
	}
}
