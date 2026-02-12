import type * as Path from "../path.ts";
import type { Reader, Writer } from "../stream.ts";
import * as Message from "./message.ts";
import * as Namespace from "./namespace.ts";
import { Parameters } from "./parameters.ts";
import { type IetfVersion, Version } from "./version.ts";

// In draft-14, ANNOUNCE is renamed to PUBLISH_NAMESPACE
export class PublishNamespace {
	static id = 0x06;

	requestId: bigint;
	trackNamespace: Path.Valid;

	constructor({ requestId, trackNamespace }: { requestId: bigint; trackNamespace: Path.Valid }) {
		this.requestId = requestId;
		this.trackNamespace = trackNamespace;
	}

	async #encode(w: Writer, _version: IetfVersion): Promise<void> {
		await w.u62(this.requestId);
		await Namespace.encode(w, this.trackNamespace);
		await w.u53(0); // size of parameters
	}

	async encode(w: Writer, version: IetfVersion): Promise<void> {
		return Message.encode(w, (wr) => this.#encode(wr, version));
	}

	static async decode(r: Reader, version: IetfVersion): Promise<PublishNamespace> {
		return Message.decode(r, (rd) => PublishNamespace.#decode(rd, version));
	}

	static async #decode(r: Reader, version: IetfVersion): Promise<PublishNamespace> {
		const requestId = await r.u62();
		const trackNamespace = await Namespace.decode(r);
		await Parameters.decode(r, version); // ignore parameters
		return new PublishNamespace({ requestId, trackNamespace });
	}
}

export class PublishNamespaceOk {
	static id = 0x07;

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

	static async decode(r: Reader, _version: IetfVersion): Promise<PublishNamespaceOk> {
		return Message.decode(r, PublishNamespaceOk.#decode);
	}

	static async #decode(r: Reader): Promise<PublishNamespaceOk> {
		const requestId = await r.u62();
		return new PublishNamespaceOk({ requestId });
	}
}

export class PublishNamespaceError {
	static id = 0x08;

	requestId: bigint;
	errorCode: number;
	reasonPhrase: string;

	constructor({
		requestId,
		errorCode,
		reasonPhrase,
	}: { requestId: bigint; errorCode: number; reasonPhrase: string }) {
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

	static async decode(r: Reader, _version: IetfVersion): Promise<PublishNamespaceError> {
		return Message.decode(r, PublishNamespaceError.#decode);
	}

	static async #decode(r: Reader): Promise<PublishNamespaceError> {
		const requestId = await r.u62();
		const errorCode = Number(await r.u62());
		const reasonPhrase = await r.string();
		return new PublishNamespaceError({ requestId, errorCode, reasonPhrase });
	}
}

export class PublishNamespaceCancel {
	static id = 0x0c;

	trackNamespace: Path.Valid;
	requestId: bigint; // v16: uses request_id instead of track_namespace
	errorCode: number;
	reasonPhrase: string;

	constructor({
		trackNamespace = "" as Path.Valid,
		errorCode = 0,
		reasonPhrase = "",
		requestId = 0n,
	}: {
		trackNamespace?: Path.Valid;
		errorCode?: number;
		reasonPhrase?: string;
		requestId?: bigint;
	} = {}) {
		this.trackNamespace = trackNamespace;
		this.requestId = requestId;
		this.errorCode = errorCode;
		this.reasonPhrase = reasonPhrase;
	}

	async #encode(w: Writer, version: IetfVersion): Promise<void> {
		if (version === Version.DRAFT_16) {
			await w.u62(this.requestId);
		} else {
			await Namespace.encode(w, this.trackNamespace);
		}
		await w.u62(BigInt(this.errorCode));
		await w.string(this.reasonPhrase);
	}

	async encode(w: Writer, version: IetfVersion): Promise<void> {
		return Message.encode(w, (wr) => this.#encode(wr, version));
	}

	static async decode(r: Reader, version: IetfVersion): Promise<PublishNamespaceCancel> {
		return Message.decode(r, (rd) => PublishNamespaceCancel.#decode(rd, version));
	}

	static async #decode(r: Reader, version: IetfVersion): Promise<PublishNamespaceCancel> {
		let trackNamespace = "" as Path.Valid;
		let requestId = 0n;
		if (version === Version.DRAFT_16) {
			requestId = await r.u62();
		} else {
			trackNamespace = await Namespace.decode(r);
		}
		const errorCode = Number(await r.u62());
		const reasonPhrase = await r.string();
		return new PublishNamespaceCancel({ trackNamespace, errorCode, reasonPhrase, requestId });
	}
}

// In draft-14, UNANNOUNCE is renamed to PUBLISH_NAMESPACE_DONE
// In draft-16, uses request_id instead of track_namespace
export class PublishNamespaceDone {
	static readonly id = 0x09;

	trackNamespace: Path.Valid;
	requestId: bigint; // v16: uses request_id instead of track_namespace

	constructor({
		trackNamespace = "" as Path.Valid,
		requestId = 0n,
	}: {
		trackNamespace?: Path.Valid;
		requestId?: bigint;
	} = {}) {
		this.trackNamespace = trackNamespace;
		this.requestId = requestId;
	}

	async #encode(w: Writer, version: IetfVersion): Promise<void> {
		if (version === Version.DRAFT_16) {
			await w.u62(this.requestId);
		} else {
			await Namespace.encode(w, this.trackNamespace);
		}
	}

	async encode(w: Writer, version: IetfVersion): Promise<void> {
		return Message.encode(w, (wr) => this.#encode(wr, version));
	}

	static async decode(r: Reader, version: IetfVersion): Promise<PublishNamespaceDone> {
		return Message.decode(r, (rd) => PublishNamespaceDone.#decode(rd, version));
	}

	static async #decode(r: Reader, version: IetfVersion): Promise<PublishNamespaceDone> {
		if (version === Version.DRAFT_16) {
			const requestId = await r.u62();
			return new PublishNamespaceDone({ requestId });
		}
		const trackNamespace = await Namespace.decode(r);
		return new PublishNamespaceDone({ trackNamespace });
	}
}
