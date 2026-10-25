import type * as Epoch from "../epoch.ts";
import type * as Path from "../path.ts";
import { type Reader, Writer } from "../stream.ts";
import { hasRangeFilters, isDraft22 } from "./filter.ts";
import * as Message from "./message.ts";
import * as Namespace from "./namespace.ts";
import { Parameters } from "./parameters.ts";
import { type IetfVersion, Version } from "./version.ts";

/**
 * The header that begins a fetch stream (draft-22 section 11.4), naming the request it
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

	/** Write the stream type and the header in one write, like a subgroup header. */
	async encode(w: Writer, version: IetfVersion): Promise<void> {
		await w.write(
			await Writer.collect(version, async (h) => {
				await h.u53(FetchHeader.type);
				await h.u62(this.requestId);
			}),
		);
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
	/** Whether the decoded request carried Range Filters. */
	rangeFilters: boolean;
	/** The decoded standalone namespace, absent for a joining fetch. */
	trackNamespace?: Path.Valid;
	/** The publisher instance the requester expects (EPOCH, MoQ Epoch), never on a joining fetch. */
	epoch?: Epoch.Valid;

	constructor({
		requestId,
		rangeFilters = false,
		trackNamespace,
		epoch,
	}: { requestId: bigint; rangeFilters?: boolean; trackNamespace?: Path.Valid; epoch?: Epoch.Valid }) {
		this.requestId = requestId;
		this.rangeFilters = rangeFilters;
		this.trackNamespace = trackNamespace;
		this.epoch = epoch;
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

		let trackNamespace: Path.Valid | undefined;
		if (isDraft22(version)) {
			// Draft-22 names the track up front and moves the range into LOCATION_FILTER.
			trackNamespace = (await Namespace.decodeTrack(r)).namespace;
		} else {
			const fetchType = await r.u53();
			switch (fetchType) {
				case 0x1: // Standalone: namespace, name, start and end Locations
					trackNamespace = (await Namespace.decodeTrack(r)).namespace;
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

		const params = await Parameters.decode(r, version, "fetch");
		if (params.rangeFilters && !hasRangeFilters(version)) {
			throw new Error("Range Filters need draft-22");
		}
		if (params.trackPropertyFilter) {
			throw new Error("TRACK_PROPERTY_FILTER is not allowed on FETCH");
		}
		// A joining FETCH is served by its subscription's instance, so naming one is malformed.
		const epoch = params.epoch;
		if (epoch !== undefined && trackNamespace === undefined) {
			throw new Error("EPOCH is not allowed on a joining FETCH");
		}

		return new Fetch({ requestId, rangeFilters: params.rangeFilters, trackNamespace, epoch });
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
