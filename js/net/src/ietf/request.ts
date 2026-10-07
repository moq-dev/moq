import type { Reader, Writer } from "../stream.ts";
import * as Message from "./message.ts";
import { Parameters } from "./parameters.ts";
import * as Properties from "./properties.ts";
import { type IetfVersion, Version } from "./version.ts";

/**
 * Requests a draft-14 to -16 peer may hold open at once.
 *
 * Sized like moq-net's window: twice its default 100,000 announce and 10,000
 * subscription caps. Draft-17 dropped MAX_REQUEST_ID.
 */
export const REQUEST_WINDOW = 220_000n;

/**
 * The SETUP MAX_REQUEST_ID that first admits `window` requests from the peer.
 *
 * It is exclusive (the draft's "maximum request ID plus 1"), and the peer uses
 * only its own parity: even IDs are the client's, odd the server's.
 */
export function initialMaxRequestId(peerClient: boolean, window: bigint = REQUEST_WINDOW): bigint {
	return window * 2n + (peerClient ? 0n : 1n);
}

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

	requestId: bigint | undefined;
	parameters: Parameters;

	constructor({ requestId, parameters = new Parameters() }: { requestId?: bigint; parameters?: Parameters }) {
		this.requestId = requestId;
		this.parameters = parameters;
	}

	async #encode(w: Writer, version: IetfVersion): Promise<void> {
		if (version === Version.DRAFT_14 || version === Version.DRAFT_15 || version === Version.DRAFT_16) {
			if (this.requestId === undefined) throw new Error("requestId required for draft14-16");
			await w.u62(this.requestId);
		}
		await this.parameters.encode(w, version);
	}

	async encode(w: Writer, version: IetfVersion): Promise<void> {
		return Message.encode(w, (wr) => this.#encode(wr, version));
	}

	static async #decode(r: Reader, version: IetfVersion): Promise<RequestOk> {
		const requestId =
			version === Version.DRAFT_14 || version === Version.DRAFT_15 || version === Version.DRAFT_16
				? await r.u62()
				: undefined;
		const parameters = await Parameters.decode(r, version);
		await Properties.decode(r, version);
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

	requestId: bigint | undefined;
	errorCode: number;
	reasonPhrase: string;
	retryInterval: bigint;

	constructor({
		requestId,
		errorCode,
		reasonPhrase,
		retryInterval = 0n,
	}: {
		requestId?: bigint;
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
		if (version === Version.DRAFT_14 || version === Version.DRAFT_15 || version === Version.DRAFT_16) {
			if (this.requestId === undefined) throw new Error("requestId required for draft14-16");
			await w.u62(this.requestId);
		}
		await w.u62(BigInt(this.errorCode));
		if (version !== Version.DRAFT_14 && version !== Version.DRAFT_15) {
			await w.u62(this.retryInterval);
		}
		await w.string(this.reasonPhrase);
	}

	async encode(w: Writer, version: IetfVersion): Promise<void> {
		return Message.encode(w, (wr) => this.#encode(wr, version));
	}

	static async #decode(r: Reader, version: IetfVersion): Promise<RequestError> {
		const requestId =
			version === Version.DRAFT_14 || version === Version.DRAFT_15 || version === Version.DRAFT_16
				? await r.u62()
				: undefined;
		const errorCode = Number(await r.u62());
		const retryInterval = version === Version.DRAFT_14 || version === Version.DRAFT_15 ? 0n : await r.u62();
		const reasonPhrase = await r.string();
		return new RequestError({ requestId, errorCode, reasonPhrase, retryInterval });
	}

	static async decode(r: Reader, version: IetfVersion): Promise<RequestError> {
		return Message.decode(r, (rd) => RequestError.#decode(rd, version));
	}
}
