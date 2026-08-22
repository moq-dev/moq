import type { Reader, Writer } from "../stream.ts";
import * as Message from "./message.ts";
import { hasProbeRtt, Version } from "./version.ts";

function guardProbe(version: Version) {
	switch (version) {
		case Version.DRAFT_01:
		case Version.DRAFT_02:
			throw new Error("probe not supported for this version");
		default:
			break;
	}
}

/** The metrics a PROBE message carries. Each is independently optional. */
export interface ProbeInit {
	/** Estimated send bitrate in bits per second. Omit if unknown. */
	bitrate?: number;
	/** Smoothed round-trip time in milliseconds. Omit if unknown. */
	rtt?: number;
}

/**
 * A PROBE message: the publisher's current view of the connection.
 *
 * Both metrics are independently optional and travel as 0 for unknown, so a
 * transport exposing only one still has something to report. A measured 0 is
 * rounded up to 1, since the wire cannot tell it from unknown.
 */
export class Probe {
	/** Estimated send bitrate in bits per second, or undefined if unknown. */
	bitrate?: number;
	/** Smoothed round-trip time in milliseconds, or undefined if unknown. */
	rtt?: number;

	// Named rather than positional: the two fields share a type, so positional
	// arguments could be swapped without a type error.
	constructor({ bitrate, rtt }: ProbeInit = {}) {
		this.bitrate = bitrate;
		this.rtt = rtt;
	}

	async #encode(w: Writer, version: Version) {
		// 0 means unknown; round a measured 0 up to 1.
		await w.u53(this.bitrate !== undefined ? Math.max(this.bitrate, 1) : 0);
		if (hasProbeRtt(version)) {
			await w.u53(this.rtt !== undefined ? Math.max(this.rtt, 1) : 0);
		}
	}

	static async #decode(r: Reader, version: Version): Promise<Probe> {
		// 0 means unknown, the same as the RTT below.
		const bitrateWire = await r.u53();
		const bitrate = bitrateWire === 0 ? undefined : bitrateWire;
		let rtt: number | undefined;
		if (hasProbeRtt(version)) {
			const wire = await r.u53();
			rtt = wire === 0 ? undefined : wire;
		}
		return new Probe({ bitrate, rtt });
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
