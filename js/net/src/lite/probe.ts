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
	/** Estimated send bitrate in bits per second, or undefined if unknown. */
	bitrate?: number;
	/** Smoothed round-trip time in milliseconds, or undefined if unknown. */
	rtt?: number;

	constructor(bitrate?: number, rtt?: number) {
		this.bitrate = bitrate;
		this.rtt = rtt;
	}

	async #encode(w: Writer, version: Version) {
		// 0 means unknown; round a measured 0 up to 1.
		await w.u53(this.bitrate !== undefined ? Math.max(this.bitrate, 1) : 0);
		switch (version) {
			case Version.DRAFT_03:
				break;
			default: {
				const wire = this.rtt !== undefined ? Math.max(this.rtt, 1) : 0;
				await w.u53(wire);
				break;
			}
		}
	}

	static async #decode(r: Reader, version: Version): Promise<Probe> {
		// 0 means unknown, the same as the RTT below.
		const bitrateWire = await r.u53();
		const bitrate = bitrateWire === 0 ? undefined : bitrateWire;
		let rtt: number | undefined;
		switch (version) {
			case Version.DRAFT_03:
				break;
			default: {
				const wire = await r.u53();
				rtt = wire === 0 ? undefined : wire;
				break;
			}
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
