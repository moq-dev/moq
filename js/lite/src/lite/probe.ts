import type { Reader, Writer } from "../stream.ts";
import { unreachable } from "../util/error.ts";
import * as Message from "./message.ts";
import { Version } from "./version.ts";

function guardDraft03(version: Version) {
	switch (version) {
		case Version.DRAFT_03:
			break;
		case Version.DRAFT_01:
		case Version.DRAFT_02:
			throw new Error("probe not supported for this version");
		default:
			unreachable(version);
	}
}

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

	async encode(w: Writer, version: Version): Promise<void> {
		guardDraft03(version);
		return Message.encode(w, this.#encode.bind(this));
	}

	static async decode(r: Reader, version: Version): Promise<Probe> {
		guardDraft03(version);
		return Message.decode(r, Probe.#decode);
	}
}
