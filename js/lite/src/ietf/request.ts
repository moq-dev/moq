import type { Reader, Writer } from "../stream.ts";
import * as Message from "./message.ts";
import { MessageParameters } from "./parameters.ts";
import { type IetfVersion, Version } from "./version.ts";

export class MaxRequestId {
	static id = 0x15;

	requestId: bigint;

	constructor({ requestId }: { requestId: bigint }) {
		this.requestId = requestId;
	}

	async #encode(w: Writer): Promise<void> {
		await w.u62(this.requestId);
	}

	async encode(w: Writer, _version: IetfVersion): Promise<void> {
		return Message.encode(w, this.#encode.bind(this));
	}

	static async #decode(r: Reader): Promise<MaxRequestId> {
		return new MaxRequestId({ requestId: await r.u62() });
	}

	static async decode(r: Reader, _version: IetfVersion): Promise<MaxRequestId> {
		return Message.decode(r, MaxRequestId.#decode);
	}
}

export class RequestsBlocked {
	static id = 0x1a;

	requestId: bigint;

	constructor({ requestId }: { requestId: bigint }) {
		this.requestId = requestId;
	}

	async #encode(w: Writer): Promise<void> {
		await w.u62(this.requestId);
	}

	async encode(w: Writer, _version: IetfVersion): Promise<void> {
		return Message.encode(w, this.#encode.bind(this));
	}

	static async #decode(r: Reader): Promise<RequestsBlocked> {
		return new RequestsBlocked({ requestId: await r.u62() });
	}

	static async decode(r: Reader, _version: IetfVersion): Promise<RequestsBlocked> {
		return Message.decode(r, RequestsBlocked.#decode);
	}
}

/// REQUEST_OK (0x07 in v15) - Generic success response for any request.
/// Replaces PublishNamespaceOk, SubscribeNamespaceOk in v15.
export class RequestOk {
	static id = 0x07;

	requestId: bigint;
	parameters: MessageParameters;

	constructor({
		requestId,
		parameters = new MessageParameters(),
	}: { requestId: bigint; parameters?: MessageParameters }) {
		this.requestId = requestId;
		this.parameters = parameters;
	}

	async #encode(w: Writer, version: IetfVersion): Promise<void> {
		await w.u62(this.requestId);
		await this.parameters.encode(w, version);
	}

	async encode(w: Writer, version: IetfVersion): Promise<void> {
		return Message.encode(w, (wr) => this.#encode(wr, version));
	}

	static async #decode(r: Reader, version: IetfVersion): Promise<RequestOk> {
		const requestId = await r.u62();
		const parameters = await MessageParameters.decode(r, version);
		return new RequestOk({ requestId, parameters });
	}

	static async decode(r: Reader, version: IetfVersion): Promise<RequestOk> {
		return Message.decode(r, (rd) => RequestOk.#decode(rd, version));
	}
}

/// REQUEST_ERROR (0x05 in v15) - Generic error response for any request.
/// Replaces SubscribeError, PublishError, PublishNamespaceError, etc. in v15.
export class RequestError {
	static id = 0x05;

	requestId: bigint;
	errorCode: number;
	reasonPhrase: string;
	retryInterval: bigint;

	constructor({
		requestId,
		errorCode,
		reasonPhrase,
		retryInterval = 0n,
	}: {
		requestId: bigint;
		errorCode: number;
		reasonPhrase: string;
		retryInterval?: bigint;
	}) {
		this.requestId = requestId;
		this.errorCode = errorCode;
		this.reasonPhrase = reasonPhrase;
		this.retryInterval = retryInterval;
	}

	async #encode(w: Writer, version: IetfVersion): Promise<void> {
		await w.u62(this.requestId);
		await w.u62(BigInt(this.errorCode));
		if (version === Version.DRAFT_16) {
			await w.u62(this.retryInterval);
		}
		await w.string(this.reasonPhrase);
	}

	async encode(w: Writer, version: IetfVersion): Promise<void> {
		return Message.encode(w, (wr) => this.#encode(wr, version));
	}

	static async #decode(r: Reader, version: IetfVersion): Promise<RequestError> {
		const requestId = await r.u62();
		const errorCode = Number(await r.u62());
		const retryInterval = version === Version.DRAFT_16 ? await r.u62() : 0n;
		const reasonPhrase = await r.string();
		return new RequestError({ requestId, errorCode, reasonPhrase, retryInterval });
	}

	static async decode(r: Reader, version: IetfVersion): Promise<RequestError> {
		return Message.decode(r, (rd) => RequestError.#decode(rd, version));
	}
}
