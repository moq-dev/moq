import type { Reader, Writer } from "../stream.ts";
import * as Message from "./message.ts";

export class MaxRequestId {
	static id = 0x15;

	requestId: bigint;

	constructor(requestId: bigint) {
		this.requestId = requestId;
	}

	async #encode(w: Writer): Promise<void> {
		await w.u62(this.requestId);
	}

	async encode(w: Writer): Promise<void> {
		return Message.encode(w, this.#encode.bind(this));
	}

	static async #decode(r: Reader): Promise<MaxRequestId> {
		return new MaxRequestId(await r.u62());
	}

	static async decode(r: Reader): Promise<MaxRequestId> {
		return Message.decode(r, MaxRequestId.#decode);
	}
}

export class RequestsBlocked {
	static id = 0x1a;

	requestId: bigint;

	constructor(requestId: bigint) {
		this.requestId = requestId;
	}

	async #encode(w: Writer): Promise<void> {
		await w.u62(this.requestId);
	}

	async encode(w: Writer): Promise<void> {
		return Message.encode(w, this.#encode.bind(this));
	}

	static async #decode(r: Reader): Promise<RequestsBlocked> {
		return new RequestsBlocked(await r.u62());
	}

	static async decode(r: Reader): Promise<RequestsBlocked> {
		return Message.decode(r, RequestsBlocked.#decode);
	}
}
