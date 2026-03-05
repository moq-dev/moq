import type { Reader, Writer } from "../stream.ts";
import * as Message from "./message.ts";
import { type IetfVersion, Version } from "./version.ts";

export class GoAway {
	static id = 0x10;

	newSessionUri: string;
	timeout: bigint;

	constructor({ newSessionUri, timeout = 0n }: { newSessionUri: string; timeout?: bigint }) {
		this.newSessionUri = newSessionUri;
		this.timeout = timeout;
	}

	async #encode(w: Writer, version: IetfVersion): Promise<void> {
		await w.string(this.newSessionUri);
		if (version === Version.DRAFT_17) {
			await w.u62(this.timeout);
		}
	}

	async encode(w: Writer, version: IetfVersion): Promise<void> {
		return Message.encode(w, (mw) => this.#encode(mw, version));
	}

	static async decode(r: Reader, version: IetfVersion): Promise<GoAway> {
		return Message.decode(r, (mr) => GoAway.#decode(mr, version));
	}

	static async #decode(r: Reader, version: IetfVersion): Promise<GoAway> {
		const newSessionUri = await r.string();
		const timeout = version === Version.DRAFT_17 ? await r.u62() : 0n;
		return new GoAway({ newSessionUri, timeout });
	}
}
