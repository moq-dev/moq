import type { Reader, Writer } from "../stream.ts";
import { hasRangeFilters, isDraft20 } from "./filter.ts";
import * as Message from "./message.ts";
import * as Namespace from "./namespace.ts";
import { Parameters } from "./parameters.ts";
import { type IetfVersion, Version } from "./version.ts";

/**
 * The header that begins a fetch stream (draft-20 section 11.4.4), naming the request it
 * answers. Written by the publisher when serving a subscription's fill.
 */
export class FetchHeader {
	/** The uni stream type, not a control message id. */
	static type = 0x5;

	/** The SUBSCRIBE (or FETCH) request this stream carries objects for. */
	requestId: bigint;

	constructor({ requestId }: { requestId: bigint }) {
		this.requestId = requestId;
	}

	/** Write the header, which the stream type must already precede. */
	async encode(w: Writer, _version: IetfVersion): Promise<void> {
		await w.u62(this.requestId);
	}

	/** Read the header, with the stream type already consumed. */
	static async decode(r: Reader, _version: IetfVersion): Promise<FetchHeader> {
		return new FetchHeader({ requestId: await r.u62() });
	}
}

/**
 * A FETCH request. We serve none, so only what a refusal needs is kept; the rest is
 * decoded so a legal request is refused rather than breaking the stream.
 */
export class Fetch {
	static id = 0x16;

	requestId: bigint;

	constructor({ requestId }: { requestId: bigint }) {
		this.requestId = requestId;
	}

	async #encode(_w: Writer): Promise<void> {
		throw new Error("FETCH messages are not supported");
	}

	async encode(w: Writer, _version: IetfVersion): Promise<void> {
		return Message.encode(w, this.#encode.bind(this));
	}

	static async decode(r: Reader, version: IetfVersion): Promise<Fetch> {
		return Message.decode(r, (mr) => Fetch.#decode(mr, version));
	}

	static async #decode(r: Reader, version: IetfVersion): Promise<Fetch> {
		const requestId = await r.u62();
		if (version === Version.DRAFT_17) {
			await r.u62(); // required_request_id_delta
		}

		if (version === Version.DRAFT_14) {
			await r.u8(); // subscriber_priority
			await r.u8(); // group_order
		}

		if (isDraft20(version)) {
			// Draft-20 names the track up front and moves the range into LOCATION_FILTER.
			await Namespace.decode(r);
			await r.string();
		} else {
			const fetchType = await r.u53();
			switch (fetchType) {
				case 0x1: // Standalone: namespace, name, start and end Locations
					await Namespace.decode(r);
					await r.string();
					for (let i = 0; i < 4; i++) await r.u62();
					break;
				case 0x2: // Relative Joining: subscription, group offset
				case 0x3: // Absolute Joining: subscription, group
					await r.u62();
					await r.u62();
					break;
				default:
					throw new Error(`unknown fetch type: ${fetchType}`);
			}
		}

		const params = await Parameters.decode(r, version);
		if (params.rangeFilters && !hasRangeFilters(version)) {
			throw new Error("Range Filters need draft-19");
		}
		if (params.trackPropertyFilter) {
			throw new Error("TRACK_PROPERTY_FILTER is not allowed on FETCH");
		}

		return new Fetch({ requestId });
	}
}

export class FetchOk {
	static id = 0x18;

	requestId: bigint | undefined;

	constructor({ requestId }: { requestId?: bigint }) {
		this.requestId = requestId;
	}

	async #encode(_w: Writer): Promise<void> {
		throw new Error("FETCH_OK messages are not supported");
	}

	async encode(w: Writer, _version: IetfVersion): Promise<void> {
		return Message.encode(w, this.#encode.bind(this));
	}

	static async decode(r: Reader, _version: IetfVersion): Promise<FetchOk> {
		return Message.decode(r, FetchOk.#decode);
	}

	static async #decode(_r: Reader): Promise<FetchOk> {
		throw new Error("FETCH_OK messages are not supported");
	}
}

export class FetchError {
	static id = 0x19;

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

	async #encode(_w: Writer): Promise<void> {
		throw new Error("FETCH_ERROR messages are not supported");
	}

	async encode(w: Writer, _version: IetfVersion): Promise<void> {
		return Message.encode(w, this.#encode.bind(this));
	}

	static async decode(r: Reader, _version: IetfVersion): Promise<FetchError> {
		return Message.decode(r, FetchError.#decode);
	}

	static async #decode(_r: Reader): Promise<FetchError> {
		throw new Error("FETCH_ERROR messages are not supported");
	}
}

// Removed in d17
export class FetchCancel {
	static id = 0x17;

	requestId: bigint;

	constructor({ requestId }: { requestId: bigint }) {
		this.requestId = requestId;
	}

	async #encode(_w: Writer): Promise<void> {
		throw new Error("FETCH_CANCEL messages are not supported");
	}

	async encode(w: Writer, _version: IetfVersion): Promise<void> {
		return Message.encode(w, this.#encode.bind(this));
	}

	static async decode(r: Reader, _version: IetfVersion): Promise<FetchCancel> {
		return Message.decode(r, FetchCancel.#decode);
	}

	static async #decode(_r: Reader): Promise<FetchCancel> {
		throw new Error("FETCH_CANCEL messages are not supported");
	}
}
