import type { Reader, Writer } from "../stream.ts";
import { toNumber } from "../util/bigint.ts";
import * as Message from "./message.ts";
import { Parameters } from "./parameters.ts";
import * as Properties from "./properties.ts";
import { type IetfVersion, isDraft23 } from "./version.ts";

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
		await w.u53(this.errorCode);
		await w.u62(this.retryInterval);
		await w.string(this.reasonPhrase);
	}

	async encode(w: Writer, _version: IetfVersion): Promise<void> {
		return Message.encode(w, (wr) => this.#encode(wr));
	}

	static async #decode(r: Reader, version: IetfVersion): Promise<RequestError> {
		const errorCode = await r.u53();
		const retryInterval = await r.u62();
		toNumber(retryInterval); // The namespace re-offer timer uses numeric milliseconds.
		const reasonPhrase = await r.string();
		if (isDraft23(version)) await skipDetails(r);
		return new RequestError({ errorCode, reasonPhrase, retryInterval });
	}

	static async decode(r: Reader, version: IetfVersion): Promise<RequestError> {
		return Message.decode(r, (rd) => RequestError.#decode(rd, version));
	}
}

/** The most Error Details one REQUEST_ERROR may carry before we refuse to walk them. */
const MAX_DETAILS = 64;

/**
 * Read past draft-23's Error Details (section 9.4.2): Key-Value-Pairs running to the end of
 * the message. None is acted on (a REDIRECT is not followed) and unknown ones must be ignored,
 * so each only has to be well formed: an even type is a varint, an odd one length-prefixed.
 */
async function skipDetails(r: Reader): Promise<void> {
	let kind = 0n;
	for (let count = 0; !(await r.done()); count++) {
		if (count === MAX_DETAILS) throw new Error(`too many Error Details: more than ${MAX_DETAILS}`);
		// The first Delta Type counts from 0, so one sum covers every pair.
		kind += await r.u62();
		if (kind % 2n === 0n) await r.u62();
		else await r.read(await r.u53());
	}
}
