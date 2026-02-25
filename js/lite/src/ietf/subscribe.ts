import type * as Path from "../path.ts";
import type { Reader, Writer } from "../stream.ts";
import { unreachable } from "../util/error.ts";
import * as Message from "./message.ts";
import * as Namespace from "./namespace.ts";
import { MessageParameters, Parameters } from "./parameters.ts";
import { type IetfVersion, Version } from "./version.ts";

// we only support Group Order descending
const GROUP_ORDER = 0x02;

export class Subscribe {
	static id = 0x03;

	requestId: bigint;
	trackNamespace: Path.Valid;
	trackName: string;
	subscriberPriority: number;

	constructor({
		requestId,
		trackNamespace,
		trackName,
		subscriberPriority,
	}: {
		requestId: bigint;
		trackNamespace: Path.Valid;
		trackName: string;
		subscriberPriority: number;
	}) {
		this.requestId = requestId;
		this.trackNamespace = trackNamespace;
		this.trackName = trackName;
		this.subscriberPriority = subscriberPriority;
	}

	async #encode(w: Writer, version: IetfVersion): Promise<void> {
		await w.u62(this.requestId);
		await Namespace.encode(w, this.trackNamespace);
		await w.string(this.trackName);

		if (version === Version.DRAFT_15 || version === Version.DRAFT_16) {
			// v15: fields moved into parameters
			const params = new MessageParameters();
			params.subscriberPriority = this.subscriberPriority;
			params.groupOrder = GROUP_ORDER;
			params.forward = true;
			params.subscriptionFilter = 0x2; // LargestObject
			await params.encode(w, version);
		} else if (version === Version.DRAFT_14) {
			await w.u8(this.subscriberPriority);
			await w.u8(GROUP_ORDER);
			await w.bool(true); // forward = true
			await w.u53(0x2); // filter type = LargestObject
			await w.u53(0); // no parameters
		} else {
			unreachable(version);
		}
	}

	async encode(w: Writer, version: IetfVersion): Promise<void> {
		return Message.encode(w, (mw) => this.#encode(mw, version));
	}

	static async decode(r: Reader, version: IetfVersion): Promise<Subscribe> {
		return Message.decode(r, (mr) => Subscribe.#decode(mr, version));
	}

	static async #decode(r: Reader, version: IetfVersion): Promise<Subscribe> {
		const requestId = await r.u62();
		const trackNamespace = await Namespace.decode(r);
		const trackName = await r.string();

		if (version === Version.DRAFT_15 || version === Version.DRAFT_16) {
			// v15: fields are in parameters
			const params = await MessageParameters.decode(r, version);
			const subscriberPriority = params.subscriberPriority ?? 128;
			let groupOrder = params.groupOrder ?? GROUP_ORDER;
			if (groupOrder > 2) {
				throw new Error(`unknown group order: ${groupOrder}`);
			}
			if (groupOrder === 0) {
				groupOrder = GROUP_ORDER; // default to descending
			}

			const forward = params.forward ?? true;
			if (!forward) {
				throw new Error(`unsupported forward value: ${forward}`);
			}

			const filterType = params.subscriptionFilter ?? 0x2;
			if (filterType !== 0x1 && filterType !== 0x2) {
				throw new Error(`unsupported filter type: ${filterType}`);
			}

			return new Subscribe({ requestId, trackNamespace, trackName, subscriberPriority });
		} else if (version === Version.DRAFT_14) {
			const subscriberPriority = await r.u8();

			let groupOrder = await r.u8();
			if (groupOrder > 2) {
				throw new Error(`unknown group order: ${groupOrder}`);
			}
			if (groupOrder === 0) {
				groupOrder = GROUP_ORDER; // default to descending
			}

			const forward = await r.bool();
			if (!forward) {
				throw new Error(`unsupported forward value: ${forward}`);
			}

			const filterType = await r.u53();
			if (filterType !== 0x1 && filterType !== 0x2) {
				throw new Error(`unsupported filter type: ${filterType}`);
			}

			await Parameters.decode(r, version); // ignore parameters

			return new Subscribe({ requestId, trackNamespace, trackName, subscriberPriority });
		} else {
			unreachable(version);
		}
	}
}

export class SubscribeOk {
	static id = 0x04;

	requestId: bigint;
	trackAlias: bigint;

	constructor({ requestId, trackAlias }: { requestId: bigint; trackAlias: bigint }) {
		this.requestId = requestId;
		this.trackAlias = trackAlias;
	}

	async #encode(w: Writer, version: IetfVersion): Promise<void> {
		await w.u62(this.requestId);
		await w.u62(this.trackAlias);

		if (version === Version.DRAFT_15 || version === Version.DRAFT_16) {
			// v15: just parameters after track_alias
			const params = new MessageParameters();
			params.groupOrder = GROUP_ORDER;
			await params.encode(w, version);
		} else if (version === Version.DRAFT_14) {
			await w.u62(0n); // expires = 0
			await w.u8(GROUP_ORDER);
			await w.bool(false); // content exists = false
			await w.u53(0); // no parameters
		} else {
			unreachable(version);
		}
	}

	async encode(w: Writer, version: IetfVersion): Promise<void> {
		return Message.encode(w, (mw) => this.#encode(mw, version));
	}

	static async decode(r: Reader, version: IetfVersion): Promise<SubscribeOk> {
		return Message.decode(r, (mr) => SubscribeOk.#decode(mr, version));
	}

	static async #decode(r: Reader, version: IetfVersion): Promise<SubscribeOk> {
		const requestId = await r.u62();
		const trackAlias = await r.u62();

		if (version === Version.DRAFT_15 || version === Version.DRAFT_16) {
			// v15: just parameters
			await MessageParameters.decode(r, version);
		} else if (version === Version.DRAFT_14) {
			const expires = await r.u62();
			if (expires !== BigInt(0)) {
				throw new Error(`unsupported expires: ${expires}`);
			}

			await r.u8(); // Don't care about group order

			const contentExists = await r.bool();
			if (contentExists) {
				// Ignore largest group/object
				await r.u62();
				await r.u62();
			}

			await Parameters.decode(r, version); // ignore parameters
		} else {
			unreachable(version);
		}

		return new SubscribeOk({ requestId, trackAlias });
	}
}

export class SubscribeError {
	static id = 0x05;

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

	static async decode(r: Reader, _version: IetfVersion): Promise<SubscribeError> {
		return Message.decode(r, SubscribeError.#decode);
	}

	static async #decode(r: Reader): Promise<SubscribeError> {
		const requestId = await r.u62();
		const errorCode = Number(await r.u62());
		const reasonPhrase = await r.string();

		return new SubscribeError({ requestId, errorCode, reasonPhrase });
	}
}

export class Unsubscribe {
	static readonly id = 0x0a;

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

	static async decode(r: Reader, _version: IetfVersion): Promise<Unsubscribe> {
		return Message.decode(r, Unsubscribe.#decode);
	}

	static async #decode(r: Reader): Promise<Unsubscribe> {
		const requestId = await r.u62();
		return new Unsubscribe({ requestId });
	}
}
