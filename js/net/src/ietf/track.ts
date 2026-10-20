import type * as Epoch from "../epoch.ts";
import type * as Path from "../path.ts";
import type { Reader, Writer } from "../stream.ts";
import { supported as epochSupported } from "./epoch.ts";
import * as Filter from "./filter.ts";
import * as Message from "./message.ts";
import * as Namespace from "./namespace.ts";
import { type MessageLocation, Parameters } from "./parameters.ts";
import * as Properties from "./properties.ts";
import { RequestOk } from "./request.ts";
import { Subscribe, SubscribeOk } from "./subscribe.ts";
import { type IetfVersion, Version } from "./version.ts";

// we only support Group Order descending
const GROUP_ORDER = 0x02;

/** TRACK_STATUS_ERROR on draft-14, with the SUBSCRIBE_ERROR body. Later drafts refuse with REQUEST_ERROR. */
export const TRACK_STATUS_ERROR_14 = 0x0f;

export class TrackStatusRequest {
	static id = 0x0d;

	requestId: bigint;
	trackNamespace: Path.Valid;
	trackName: string;
	/** Whether the requester wants Track Properties on the answer (INCLUDE_PROPERTIES, draft-20). */
	propertiesWanted: boolean;
	/** The publisher instance the requester expects (EPOCH, MoQ Epoch, draft-17+). */
	epoch?: Epoch.Valid;

	constructor({
		requestId,
		trackNamespace,
		trackName,
		propertiesWanted = true,
		epoch,
	}: {
		requestId: bigint;
		trackNamespace: Path.Valid;
		trackName: string;
		propertiesWanted?: boolean;
		epoch?: Epoch.Valid;
	}) {
		this.requestId = requestId;
		this.trackNamespace = trackNamespace;
		this.trackName = trackName;
		this.propertiesWanted = propertiesWanted;
		this.epoch = epoch;
	}

	async #encode(w: Writer, version: IetfVersion): Promise<void> {
		await w.u62(this.requestId);
		if (version === Version.DRAFT_17) {
			await w.u62(0n); // required_request_id_delta — always 0, not supported
		}
		await Namespace.encode(w, this.trackNamespace);
		await w.string(this.trackName);

		if (version === Version.DRAFT_14) {
			await w.u8(0); // subscriber_priority
			await w.u8(GROUP_ORDER); // group_order
			await w.bool(false); // forward
			await w.u53(0x2); // filter_type = LargestObject
			await w.u53(0); // no parameters
		} else {
			// v15+: just parameters. Only the opt-out is worth bytes, and an older peer would
			// read INCLUDE_PROPERTIES as unknown, which is a protocol violation.
			const params = new Parameters();
			if (!this.propertiesWanted && Filter.isDraft20(version)) params.includeProperties = false;
			if (epochSupported(version)) params.epoch = this.epoch;
			await params.encode(w, version);
		}
	}

	async encode(w: Writer, version: IetfVersion): Promise<void> {
		return Message.encode(w, (mw) => this.#encode(mw, version));
	}

	/**
	 * Every draft defines TRACK_STATUS as identical to SUBSCRIBE, so it decodes as one and
	 * keeps what names the track and shapes the answer. The rest goes unread: there is no
	 * subscription for it to configure.
	 */
	static async decode(r: Reader, version: IetfVersion): Promise<TrackStatusRequest> {
		const { requestId, trackNamespace, trackName, propertiesWanted, epoch } = await Subscribe.decode(r, version);
		return new TrackStatusRequest({ requestId, trackNamespace, trackName, propertiesWanted, epoch });
	}
}

/**
 * TRACK_STATUS_OK: what a SUBSCRIBE_OK would say about the track, with no subscription.
 *
 * Draft-14 gives it its own type and the SUBSCRIBE_OK body, with a Track Alias of 0. Later
 * drafts answer with a REQUEST_OK, which gained Track Properties in draft-18; before that
 * only the message parameters carry anything.
 */
export class TrackStatusOk {
	/** The draft-14 message type. */
	static readonly ID_14 = 0x0e;

	/** The message type on `version`. */
	static id(version: IetfVersion): number {
		return version === Version.DRAFT_14 ? TrackStatusOk.ID_14 : RequestOk.id;
	}

	/** Present on drafts 14-16 only. */
	requestId: bigint | undefined;

	/** The largest Location in the track (LARGEST_OBJECT), absent while it has no content. */
	largest: MessageLocation | undefined;

	/** The Track Properties, on the drafts whose answer has room for them. */
	properties: Properties.Properties;

	constructor({
		requestId,
		largest,
		properties = {},
	}: { requestId?: bigint; largest?: MessageLocation; properties?: Properties.Properties }) {
		this.requestId = requestId;
		this.largest = largest;
		this.properties = properties;
	}

	/** Whether the answer carries a Track Properties block on `version`. */
	static #hasProperties(version: IetfVersion): boolean {
		return version >= Version.DRAFT_18;
	}

	async #encode(w: Writer, version: IetfVersion): Promise<void> {
		if (version === Version.DRAFT_15 || version === Version.DRAFT_16) {
			if (this.requestId === undefined) throw new Error("requestId required for draft14-16");
			await w.u62(this.requestId);
		}

		// The parameters SUBSCRIBE_OK carries: GROUP_ORDER is one only on draft-15.
		const params = new Parameters();
		if (this.largest !== undefined) params.largest = this.largest;
		if (version === Version.DRAFT_15 && this.properties.groupOrder !== undefined) {
			params.groupOrder = this.properties.groupOrder;
		}
		await params.encode(w, version);

		// Track Properties are the final field, so nothing may follow.
		if (TrackStatusOk.#hasProperties(version)) await Properties.encode(w, this.properties, version);
	}

	async encode(w: Writer, version: IetfVersion): Promise<void> {
		if (version === Version.DRAFT_14) {
			const { requestId, largest, properties } = this;
			return new SubscribeOk({ requestId, trackAlias: 0n, largest, properties }).encode(w, version);
		}
		return Message.encode(w, (mw) => this.#encode(mw, version));
	}

	static async decode(r: Reader, version: IetfVersion): Promise<TrackStatusOk> {
		if (version === Version.DRAFT_14) {
			const { requestId, largest, properties } = await SubscribeOk.decode(r, version);
			return new TrackStatusOk({ requestId, largest, properties });
		}
		return Message.decode(r, (mr) => TrackStatusOk.#decode(mr, version));
	}

	static async #decode(r: Reader, version: IetfVersion): Promise<TrackStatusOk> {
		const requestId = version === Version.DRAFT_15 || version === Version.DRAFT_16 ? await r.u62() : undefined;
		const params = await Parameters.decode(r, version, "subscribe-ok");
		const properties = TrackStatusOk.#hasProperties(version) ? await Properties.decode(r, version) : {};
		if (version === Version.DRAFT_15) {
			properties.groupOrder = params.groupOrder;
			properties.maxCacheDuration = params.maxCacheDuration;
		}
		return new TrackStatusOk({ requestId, largest: params.largest, properties });
	}
}
