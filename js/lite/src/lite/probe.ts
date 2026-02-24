import type { Reader, Writer } from "../stream.ts";
import * as Message from "./message.ts";

export class Probe {
	bitrate: number;

	constructor(bitrate: number) {
		this.bitrate = bitrate;
	}

	async #encode(w: Writer) {
		await w.u53(this.bitrate);
	}

	static async #decode(r: Reader): Promise<Probe> {
		return new Probe(await r.u53());
	}

	async encode(w: Writer): Promise<void> {
		return Message.encode(w, this.#encode.bind(this));
	}

	static async decode(r: Reader): Promise<Probe> {
		return Message.decode(r, Probe.#decode);
	}
}
