import type { Reader, Writer } from "../stream.ts";
import * as Message from "./message.ts";
import { Parameters } from "./parameters.ts";
import * as Properties from "./properties.ts";
import type { IetfVersion } from "./version.ts";

/// REQUEST_OK (0x07) - Generic success response for any request.
export class RequestOk {
	static id = 0x07;

	parameters: Parameters;

	constructor({ parameters = new Parameters() }: { parameters?: Parameters } = {}) {
		this.parameters = parameters;
	}

	async #encode(w: Writer, version: IetfVersion): Promise<void> {
		await this.parameters.encode(w, version);
	}

	async encode(w: Writer, version: IetfVersion): Promise<void> {
		return Message.encode(w, (wr) => this.#encode(wr, version));
	}

	static async #decode(r: Reader, version: IetfVersion): Promise<RequestOk> {
		const parameters = await Parameters.decode(r, version, "request-ok");
		await Properties.decode(r, version);
		return new RequestOk({ parameters });
	}

	static async decode(r: Reader, version: IetfVersion): Promise<RequestOk> {
		return Message.decode(r, (rd) => RequestOk.#decode(rd, version));
	}
}

/// REQUEST_ERROR (0x05) - Generic error response for any request.
export class RequestError {
	static id = 0x05;

	errorCode: number;
	reasonPhrase: string;
	retryInterval: bigint;

	constructor({
		errorCode,
		reasonPhrase,
		retryInterval = 0n,
	}: {
		errorCode: number;
		reasonPhrase: string;
		retryInterval?: bigint;
	}) {
		this.errorCode = errorCode;
		this.reasonPhrase = reasonPhrase;
		this.retryInterval = retryInterval;
	}

	async #encode(w: Writer): Promise<void> {
		await w.u62(BigInt(this.errorCode));
		await w.u62(this.retryInterval);
		await w.string(this.reasonPhrase);
	}

	async encode(w: Writer, _version: IetfVersion): Promise<void> {
		return Message.encode(w, (wr) => this.#encode(wr));
	}

	static async #decode(r: Reader): Promise<RequestError> {
		const errorCode = Number(await r.u62());
		const retryInterval = await r.u62();
		const reasonPhrase = await r.string();
		return new RequestError({ errorCode, reasonPhrase, retryInterval });
	}

	static async decode(r: Reader, _version: IetfVersion): Promise<RequestError> {
		return Message.decode(r, RequestError.#decode);
	}
}
