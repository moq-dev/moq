import type { Reader, Writer } from "../stream.ts";
import * as Message from "./message.ts";
import { Version } from "./version.ts";

function guardProbe(version: Version) {
	switch (version) {
		case Version.DRAFT_01:
		case Version.DRAFT_02:
			throw new Error("probe not supported for this version");
		default:
			break;
	}
}

export class Probe {
	bitrate: number;
	rtt: number;

	constructor(bitrate: number, rtt = 0) {
		this.bitrate = bitrate;
		this.rtt = rtt;
	}

	async #encode(w: Writer, version: Version) {
		await w.u53(this.bitrate);
		switch (version) {
			case Version.DRAFT_03:
				break;
			default:
				// Lite04+: rtt field
				await w.u53(this.rtt);
				break;
		}
	}

	static async #decode(r: Reader, version: Version): Promise<Probe> {
		const bitrate = await r.u53();
		let rtt = 0;
		switch (version) {
			case Version.DRAFT_03:
				break;
			default:
				rtt = await r.u53();
				break;
		}
		return new Probe(bitrate, rtt);
	}

	async encode(w: Writer, version: Version): Promise<void> {
		guardProbe(version);
		return Message.encode(w, (w) => this.#encode(w, version));
	}

	static async decode(r: Reader, version: Version): Promise<Probe> {
		guardProbe(version);
		return Message.decode(r, (r) => Probe.#decode(r, version));
	}

	static async decodeMaybe(r: Reader, version: Version): Promise<Probe | undefined> {
		guardProbe(version);
		return Message.decodeMaybe(r, (r) => Probe.#decode(r, version));
	}
}
