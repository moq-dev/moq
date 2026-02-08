import type { Reader, Writer } from "../stream.ts";
import * as Message from "./message.ts";
import { MessageParameters } from "./parameters.ts";

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

/// REQUEST_OK (0x07 in v15) - Generic success response for any request.
/// Replaces PublishNamespaceOk, SubscribeNamespaceOk in v15.
export class RequestOk {
	static id = 0x07;

	requestId: bigint;
	parameters: MessageParameters;

	constructor(requestId: bigint, parameters = new MessageParameters()) {
		this.requestId = requestId;
		this.parameters = parameters;
	}

	async #encode(w: Writer): Promise<void> {
		await w.u62(this.requestId);
		await this.parameters.encode(w);
	}

	async encode(w: Writer): Promise<void> {
		return Message.encode(w, this.#encode.bind(this));
	}

	static async #decode(r: Reader): Promise<RequestOk> {
		const requestId = await r.u62();
		const parameters = await MessageParameters.decode(r);
		return new RequestOk(requestId, parameters);
	}

	static async decode(r: Reader): Promise<RequestOk> {
		return Message.decode(r, RequestOk.#decode);
	}
}

/// REQUEST_ERROR (0x05 in v15) - Generic error response for any request.
/// Replaces SubscribeError, PublishError, PublishNamespaceError, etc. in v15.
export class RequestError {
	static id = 0x05;

	requestId: bigint;
	errorCode: number;
	reasonPhrase: string;

	constructor(requestId: bigint, errorCode: number, reasonPhrase: string) {
		this.requestId = requestId;
		this.errorCode = errorCode;
		this.reasonPhrase = reasonPhrase;
	}

	async #encode(w: Writer): Promise<void> {
		await w.u62(this.requestId);
		await w.u62(BigInt(this.errorCode));
		await w.string(this.reasonPhrase);
	}

	async encode(w: Writer): Promise<void> {
		return Message.encode(w, this.#encode.bind(this));
	}

	static async #decode(r: Reader): Promise<RequestError> {
		const requestId = await r.u62();
		const errorCode = Number(await r.u62());
		const reasonPhrase = await r.string();
		return new RequestError(requestId, errorCode, reasonPhrase);
	}

	static async decode(r: Reader): Promise<RequestError> {
		return Message.decode(r, RequestError.#decode);
	}
}
