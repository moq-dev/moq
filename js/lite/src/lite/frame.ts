import type { Reader, Writer } from "../stream";
import * as Time from "../time.ts";
import * as Message from "./message.ts";
import { Version } from "./version.js";

export class Frame {
	delta: Time.Milli;
	payload: Uint8Array;

	constructor({ payload, delta }: { delta: Time.Milli; payload: Uint8Array }) {
		this.payload = payload;
		this.delta = delta;
	}

	async #encode(w: Writer, version: Version) {
		switch (version) {
			case Version.DRAFT_03:
				await w.u53(this.delta);
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
		let delta: Time.Milli;

		switch (version) {
			case Version.DRAFT_03:
				delta = (await r.u53()) as Time.Milli;
				break;
			case Version.DRAFT_01:
			case Version.DRAFT_02:
				// NOTE: The caller is responsible for calling Time.Milli.now()
				delta = Time.Milli.zero;
				break;
			default: {
				const v: never = version;
				throw new Error(`unsupported version: ${v}`);
			}
		}

		const payload = await r.readAll();
		return new Frame({ delta, payload });
	}

	async encode(w: Writer, v: Version): Promise<void> {
		return Message.encode(w, (w) => this.#encode(w, v));
	}

	static async decode(r: Reader, v: Version): Promise<Frame> {
		return Message.decode(r, (r) => Frame.#decode(r, v));
	}
}
