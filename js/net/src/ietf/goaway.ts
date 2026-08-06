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
		if (version !== Version.DRAFT_14 && version !== Version.DRAFT_15 && version !== Version.DRAFT_16) {
			await w.u62(this.timeout);
		}
		// Draft-18 (#1559) requires a Request ID on the control stream (the only
		// place GOAWAY is sent); omitting it is a length mismatch a conformant
		// peer must reject. We advertise 0 ("no requests processed"), matching
		// the Rust encoder. Draft-19 removed the field again (#1623).
		if (version === Version.DRAFT_18) {
			await w.u62(0n);
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
		// All drafts cap the New Session URI at 8,192 bytes; a longer one is a
		// protocol violation. Matches the Rust decoder.
		if (new TextEncoder().encode(newSessionUri).byteLength > 8192) {
			throw new Error("GOAWAY new session URI exceeds 8,192 bytes");
		}
		let timeout = 0n;
		if (version !== Version.DRAFT_14 && version !== Version.DRAFT_15 && version !== Version.DRAFT_16) {
			timeout = await r.u62();
		}
		// Draft-18 optional trailing Request ID (#1559). Drain remaining bytes
		// so the outer message-frame size check passes.
		// Draft-19 removed this field again (#1623).
		if (version === Version.DRAFT_18) {
			await r.readAll();
		}
		return new GoAway({ newSessionUri, timeout });
	}
}
