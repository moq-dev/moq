import { Milli } from "@moq/clock";
import type { Drain } from "../connection/goaway.ts";
import { ProtocolViolation } from "../error.ts";
import type { Reader, Writer } from "../stream.ts";
import * as Message from "./message.ts";
import { type IetfVersion, Version } from "./version.ts";

export class GoAway {
	static id = 0x10;

	newSessionUri: string;
	timeout: bigint;
	/**
	 * Draft-18 only (#1559): required on the control stream, absent on a request stream.
	 * It names the receiver's Request IDs, so a client sends an odd one and a server an even one.
	 * Draft-22 removed the field again (#1623).
	 */
	requestId?: bigint;

	constructor({
		newSessionUri,
		timeout = 0n,
		requestId,
	}: { newSessionUri: string; timeout?: bigint; requestId?: bigint }) {
		this.newSessionUri = newSessionUri;
		this.timeout = timeout;
		this.requestId = requestId;
	}

	async #encode(w: Writer, version: IetfVersion): Promise<void> {
		await w.string(this.newSessionUri);
		await w.u62(this.timeout);
		if (version === Version.DRAFT_18 && this.requestId !== undefined) {
			await w.u62(this.requestId);
		}
	}

	async encode(w: Writer, version: IetfVersion): Promise<void> {
		return Message.encode(w, (mw) => this.#encode(mw, version));
	}

	static async decode(r: Reader, version: IetfVersion): Promise<GoAway> {
		return Message.decode(r, (mr) => GoAway.#decode(mr, version));
	}

	/** The drain signal this message carries. A zero timeout is the wire saying "none". */
	drain(): Drain {
		return {
			uri: this.newSessionUri,
			timeout: this.timeout > 0n ? Milli(Number(this.timeout)) : undefined,
		};
	}

	static async #decode(r: Reader, version: IetfVersion): Promise<GoAway> {
		const newSessionUri = await r.string();
		// All drafts cap the New Session URI at 8,192 bytes; a longer one is a
		// protocol violation. Matches the Rust decoder.
		if (new TextEncoder().encode(newSessionUri).byteLength > 8192) {
			throw new ProtocolViolation("GOAWAY new session URI exceeds 8,192 bytes");
		}
		const timeout = await r.u62();
		let requestId: bigint | undefined;
		if (version === Version.DRAFT_18 && !(await r.done())) {
			requestId = await r.u62();
		}
		return new GoAway({ newSessionUri, timeout, requestId });
	}
}
