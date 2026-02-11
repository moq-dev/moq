import type * as Path from "../path.ts";
import type { Reader, Writer } from "../stream.ts";
import * as Message from "./message.ts";
import * as Namespace from "./namespace.ts";
import { Parameters } from "./parameters.ts";
import type { IetfVersion } from "./version.ts";

// In draft-14, SUBSCRIBE_ANNOUNCES is renamed to SUBSCRIBE_NAMESPACE
export class SubscribeNamespace {
	static id = 0x11;

	namespace: Path.Valid;
	requestId: bigint;

	constructor(namespace: Path.Valid, requestId: bigint) {
		this.namespace = namespace;
		this.requestId = requestId;
	}

	async #encode(w: Writer, _version: IetfVersion): Promise<void> {
		await w.u62(this.requestId);
		await Namespace.encode(w, this.namespace);
		await w.u53(0); // no parameters
	}

	async encode(w: Writer, version: IetfVersion): Promise<void> {
		return Message.encode(w, (wr) => this.#encode(wr, version));
	}

	static async decode(r: Reader, version: IetfVersion): Promise<SubscribeNamespace> {
		return Message.decode(r, (rd) => SubscribeNamespace.#decode(rd, version));
	}

	static async #decode(r: Reader, version: IetfVersion): Promise<SubscribeNamespace> {
		const requestId = await r.u62();
		const namespace = await Namespace.decode(r);
		await Parameters.decode(r, version);

		return new SubscribeNamespace(namespace, requestId);
	}
}

export class SubscribeNamespaceOk {
	static id = 0x12;

	requestId: bigint;

	constructor(requestId: bigint) {
		this.requestId = requestId;
	}

	async #encode(w: Writer): Promise<void> {
		await w.u62(this.requestId);
	}

	async encode(w: Writer, _version: IetfVersion): Promise<void> {
		return Message.encode(w, this.#encode.bind(this));
	}

	static async decode(r: Reader, _version: IetfVersion): Promise<SubscribeNamespaceOk> {
		return Message.decode(r, SubscribeNamespaceOk.#decode);
	}

	static async #decode(r: Reader): Promise<SubscribeNamespaceOk> {
		const requestId = await r.u62();
		return new SubscribeNamespaceOk(requestId);
	}
}

export class SubscribeNamespaceError {
	static id = 0x13;

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

	async encode(w: Writer, _version: IetfVersion): Promise<void> {
		return Message.encode(w, this.#encode.bind(this));
	}

	static async decode(r: Reader, _version: IetfVersion): Promise<SubscribeNamespaceError> {
		return Message.decode(r, SubscribeNamespaceError.#decode);
	}

	static async #decode(r: Reader): Promise<SubscribeNamespaceError> {
		const requestId = await r.u62();
		const errorCode = Number(await r.u62());
		const reasonPhrase = await r.string();

		return new SubscribeNamespaceError(requestId, errorCode, reasonPhrase);
	}
}

export class UnsubscribeNamespace {
	static id = 0x14;

	requestId: bigint;

	constructor(requestId: bigint) {
		this.requestId = requestId;
	}

	async #encode(w: Writer): Promise<void> {
		await w.u62(this.requestId);
	}

	async encode(w: Writer, _version: IetfVersion): Promise<void> {
		return Message.encode(w, this.#encode.bind(this));
	}

	static async decode(r: Reader, _version: IetfVersion): Promise<UnsubscribeNamespace> {
		return Message.decode(r, UnsubscribeNamespace.#decode);
	}

	static async #decode(r: Reader): Promise<UnsubscribeNamespace> {
		const requestId = await r.u62();
		return new UnsubscribeNamespace(requestId);
	}
}

// Backward compatibility aliases
export const SubscribeAnnounces = SubscribeNamespace;
export const SubscribeAnnouncesOk = SubscribeNamespaceOk;
export const SubscribeAnnouncesError = SubscribeNamespaceError;
export const UnsubscribeAnnounces = UnsubscribeNamespace;
