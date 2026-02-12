import type * as Path from "../path.ts";
import type { Reader, Writer } from "../stream.ts";
import * as Message from "./message.ts";
import * as Namespace from "./namespace.ts";
import { MessageParameters, Parameters } from "./parameters.ts";
import { type IetfVersion, Version } from "./version.ts";

// PUBLISH messages are new in draft-14

export class Publish {
	static id = 0x1d;

	requestId: bigint;
	trackNamespace: Path.Valid;
	trackName: string;
	trackAlias: bigint;
	groupOrder: number;
	contentExists: boolean;
	largest: { groupId: bigint; objectId: bigint } | undefined;
	forward: boolean;

	constructor({
		requestId,
		trackNamespace,
		trackName,
		trackAlias,
		groupOrder,
		contentExists,
		largest,
		forward,
	}: {
		requestId: bigint;
		trackNamespace: Path.Valid;
		trackName: string;
		trackAlias: bigint;
		groupOrder: number;
		contentExists: boolean;
		largest: { groupId: bigint; objectId: bigint } | undefined;
		forward: boolean;
	}) {
		this.requestId = requestId;
		this.trackNamespace = trackNamespace;
		this.trackName = trackName;
		this.trackAlias = trackAlias;
		this.groupOrder = groupOrder;
		this.contentExists = contentExists;
		this.largest = largest;
		this.forward = forward;
	}

	async #encode(w: Writer, version: IetfVersion): Promise<void> {
		await w.u62(this.requestId);
		await Namespace.encode(w, this.trackNamespace);
		await w.string(this.trackName);
		await w.u62(this.trackAlias);

		if (version === Version.DRAFT_15 || version === Version.DRAFT_16) {
			// v15: fields in parameters
			const params = new MessageParameters();
			params.groupOrder = this.groupOrder;
			params.forward = this.forward;
			if (this.largest) {
				params.largest = this.largest;
			}
			await params.encode(w, version);
		} else if (version === Version.DRAFT_14) {
			await w.u8(this.groupOrder);
			await w.bool(this.contentExists);
			if (this.contentExists !== !!this.largest) {
				throw new Error("contentExists and largest must both be true or false");
			}
			if (this.largest) {
				await w.u62(this.largest.groupId);
				await w.u62(this.largest.objectId);
			}
			await w.bool(this.forward);
			await w.u53(0); // size of parameters
		} else {
			const _: never = version;
			throw new Error(`unsupported version: ${_}`);
		}
	}

	async encode(w: Writer, version: IetfVersion): Promise<void> {
		return Message.encode(w, (mw) => this.#encode(mw, version));
	}

	static async decode(r: Reader, version: IetfVersion): Promise<Publish> {
		return Message.decode(r, (mr) => Publish.#decode(mr, version));
	}

	static async #decode(r: Reader, version: IetfVersion): Promise<Publish> {
		const requestId = await r.u62();
		const trackNamespace = await Namespace.decode(r);
		const trackName = await r.string();
		const trackAlias = await r.u62();

		if (version === Version.DRAFT_15 || version === Version.DRAFT_16) {
			const params = await MessageParameters.decode(r, version);
			const groupOrder = params.groupOrder ?? 0x02;
			const forward = params.forward ?? true;
			const largest = params.largest;
			return new Publish({
				requestId,
				trackNamespace,
				trackName,
				trackAlias,
				groupOrder,
				contentExists: !!largest,
				largest,
				forward,
			});
		} else if (version === Version.DRAFT_14) {
			const groupOrder = await r.u8();
			const contentExists = await r.bool();
			const largest = contentExists ? { groupId: await r.u62(), objectId: await r.u62() } : undefined;
			const forward = await r.bool();
			await Parameters.decode(r, version); // ignore parameters
			return new Publish({
				requestId,
				trackNamespace,
				trackName,
				trackAlias,
				groupOrder,
				contentExists,
				largest,
				forward,
			});
		} else {
			const _: never = version;
			throw new Error(`unsupported version: ${_}`);
		}
	}
}

export class PublishOk {
	static id = 0x1e;

	async #encode(_w: Writer): Promise<void> {
		throw new Error("PUBLISH_OK messages are not supported");
	}

	async encode(w: Writer, _version: IetfVersion): Promise<void> {
		return Message.encode(w, this.#encode.bind(this));
	}

	static async decode(r: Reader, _version: IetfVersion): Promise<PublishOk> {
		return Message.decode(r, PublishOk.#decode);
	}

	static async #decode(_r: Reader): Promise<PublishOk> {
		throw new Error("PUBLISH_OK messages are not supported");
	}
}

export class PublishError {
	static id = 0x1f;

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

	static async decode(r: Reader, _version: IetfVersion): Promise<PublishError> {
		return Message.decode(r, PublishError.#decode);
	}

	static async #decode(r: Reader): Promise<PublishError> {
		const requestId = await r.u62();
		const errorCode = Number(await r.u62());
		const reasonPhrase = await r.string();
		return new PublishError({ requestId, errorCode, reasonPhrase });
	}
}

// In draft-14, this message is renamed from SUBSCRIBE_DONE to PUBLISH_DONE
export class PublishDone {
	static readonly id = 0x0b;

	requestId: bigint;
	statusCode: number;
	reasonPhrase: string;

	constructor({
		requestId,
		statusCode,
		reasonPhrase,
	}: { requestId: bigint; statusCode: number; reasonPhrase: string }) {
		this.requestId = requestId;
		this.statusCode = statusCode;
		this.reasonPhrase = reasonPhrase;
	}

	async #encode(w: Writer): Promise<void> {
		await w.u62(this.requestId);
		await w.u62(BigInt(this.statusCode));
		await w.u62(BigInt(0)); // stream_count = 0 (unsupported)
		await w.string(this.reasonPhrase);
	}

	async encode(w: Writer, _version: IetfVersion): Promise<void> {
		return Message.encode(w, this.#encode.bind(this));
	}

	static async decode(r: Reader, _version: IetfVersion): Promise<PublishDone> {
		return Message.decode(r, PublishDone.#decode);
	}

	static async #decode(r: Reader): Promise<PublishDone> {
		const requestId = await r.u62();
		const statusCode = Number(await r.u62());
		await r.u62(); // ignore stream_count
		const reasonPhrase = await r.string();

		return new PublishDone({ requestId, statusCode, reasonPhrase });
	}
}
