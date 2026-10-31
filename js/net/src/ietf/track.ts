import type * as Epoch from "../epoch.ts";
import type * as Path from "../path.ts";
import type { Reader, Writer } from "../stream.ts";
import * as Filter from "./filter.ts";
import * as Message from "./message.ts";
import * as Namespace from "./namespace.ts";
import { type MessageLocation, Parameters } from "./parameters.ts";
import * as Properties from "./properties.ts";
import { RequestOk } from "./request.ts";
import { Subscribe } from "./subscribe.ts";
import type { IetfVersion } from "./version.ts";

export class TrackStatusRequest {
	static id = 0x0d;

	requestId: bigint;
	trackNamespace: Path.Valid;
	trackName: string;
	/** Whether the requester wants Track Properties on the answer (INCLUDE_PROPERTIES, draft-22). */
	propertiesWanted: boolean;
	/** The publisher instance the requester expects (EPOCH, MoQ Epoch). */
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
		await Namespace.encodeTrack(w, this.trackNamespace, this.trackName);

		// Only the opt-out is worth bytes, and a peer before draft-22 would read
		// INCLUDE_PROPERTIES as unknown, which is a protocol violation.
		const params = new Parameters();
		if (!this.propertiesWanted && Filter.isDraft20(version)) params.includeProperties = false;
		params.epoch = this.epoch;
		await params.encode(w, version);
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
 * The answer is a REQUEST_OK carrying the message parameters and Track Properties.
 */
export class TrackStatusOk {
	static readonly id = RequestOk.id;

	/** The largest Location in the track (LARGEST_OBJECT), absent while it has no content. */
	largest: MessageLocation | undefined;

	/** The Track Properties. */
	properties: Properties.Properties;

	constructor({ largest, properties = {} }: { largest?: MessageLocation; properties?: Properties.Properties } = {}) {
		this.largest = largest;
		this.properties = properties;
	}

	async #encode(w: Writer, version: IetfVersion): Promise<void> {
		// The parameters SUBSCRIBE_OK carries.
		const params = new Parameters();
		if (this.largest !== undefined) params.largest = this.largest;
		await params.encode(w, version);

		// Track Properties are the final field, so nothing may follow.
		await Properties.encode(w, this.properties, version);
	}

	async encode(w: Writer, version: IetfVersion): Promise<void> {
		return Message.encode(w, (mw) => this.#encode(mw, version));
	}

	static async decode(r: Reader, version: IetfVersion): Promise<TrackStatusOk> {
		return Message.decode(r, (mr) => TrackStatusOk.#decode(mr, version));
	}

	static async #decode(r: Reader, version: IetfVersion): Promise<TrackStatusOk> {
		const params = await Parameters.decode(r, version, "subscribe-ok");
		const properties = await Properties.decode(r, version);
		return new TrackStatusOk({ largest: params.largest, properties });
	}
}
