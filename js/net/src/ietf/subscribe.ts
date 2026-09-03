import type * as Path from "../path.ts";
import type { Reader, Writer } from "../stream.ts";
import * as Filter from "./filter.ts";
import * as Message from "./message.ts";
import * as Namespace from "./namespace.ts";
import { type MessageLocation, Parameters } from "./parameters.ts";
import * as Properties from "./properties.ts";
import { type IetfVersion, Version } from "./version.ts";

// we only support Group Order descending
const GROUP_ORDER = 0x02;

/**
 * The filter this implementation joins a live track with.
 *
 * moq-lite joins at the start of the current group, which is a decodable point, and
 * draft-20's relative form is the first that can name it without knowing Largest Object.
 * Earlier drafts only get the next Object after the live edge, which begins mid-group.
 */
export function joinFilter(version: IetfVersion): Filter.Filter {
	return Filter.isDraft20(version) ? { kind: "relative", groups: 1n } : { kind: "nextObject" };
}

export class Subscribe {
	static id = 0x03;

	requestId: bigint;
	trackNamespace: Path.Valid;
	trackName: string;
	subscriberPriority: number;

	/** Which Objects the subscription delivers, resolved by the publisher against the live edge. */
	filter: Filter.Filter;

	/** The draft-20 backfill requested alongside it, or `undefined` for none. */
	fill?: Filter.Fill;

	/** Whether the subscriber wants Track Properties on the response (INCLUDE_PROPERTIES). */
	propertiesWanted: boolean;

	constructor({
		requestId,
		trackNamespace,
		trackName,
		subscriberPriority,
		filter,
		fill,
		propertiesWanted,
	}: {
		requestId: bigint;
		trackNamespace: Path.Valid;
		trackName: string;
		subscriberPriority: number;
		filter?: Filter.Filter;
		fill?: Filter.Fill;
		propertiesWanted?: boolean;
	}) {
		this.requestId = requestId;
		this.trackNamespace = trackNamespace;
		this.trackName = trackName;
		this.subscriberPriority = subscriberPriority;
		this.filter = filter ?? { kind: "unfiltered" };
		this.fill = fill;
		this.propertiesWanted = propertiesWanted ?? true;
	}

	async #encode(w: Writer, version: IetfVersion): Promise<void> {
		await w.u62(this.requestId);
		if (version === Version.DRAFT_17) {
			await w.u62(0n); // required_request_id_delta = 0 (draft-17 only, removed in draft-18 per #1615)
		}
		await Namespace.encode(w, this.trackNamespace);
		await w.string(this.trackName);

		if (version === Version.DRAFT_14) {
			await w.u8(this.subscriberPriority);
			await w.u8(GROUP_ORDER);
			await w.bool(true); // forward = true
			await w.write(Filter.encode(this.filter, version));
			await w.u53(0); // no parameters
		} else {
			// v15+: fields moved into parameters
			const params = new Parameters();
			params.subscriberPriority = this.subscriberPriority;
			params.groupOrder = GROUP_ORDER;
			params.forward = true;
			params.subscriptionFilter = Filter.encode(this.filter, version);

			// FILL_PARAMETERS and INCLUDE_PROPERTIES arrived in draft-20. An older peer reads
			// either as an unknown parameter, which is a protocol violation, so they are
			// dropped rather than downgraded.
			if (Filter.isDraft20(version)) {
				if (this.fill) {
					params.fillParameters = Filter.encodeFill(this.fill, version);
				}
				// The default is 1, so only the opt-out is worth bytes.
				if (!this.propertiesWanted) {
					params.includeProperties = false;
				}
			}

			await params.encode(w, version);
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
		if (version === Version.DRAFT_17) {
			await r.u62(); // required_request_id_delta (read and ignore)
		}
		const trackNamespace = await Namespace.decode(r);
		const trackName = await r.string();

		if (version === Version.DRAFT_14) {
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

			const filter = await Filter.decodeInline(r);

			await Parameters.decode(r, version); // ignore parameters

			return new Subscribe({ requestId, trackNamespace, trackName, subscriberPriority, filter });
		}
		// v15+: fields are in parameters
		const params = await Parameters.decode(r, version);
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

		// FILL_PARAMETERS and INCLUDE_PROPERTIES are draft-20 additions. An unknown message
		// parameter is a protocol violation, so they stay rejected on the drafts that predate
		// them rather than being quietly tolerated.
		const draft20 = Filter.isDraft20(version);
		if ((params.fillParameters !== undefined || params.includeProperties !== undefined) && !draft20) {
			throw new Error("FILL_PARAMETERS and INCLUDE_PROPERTIES need draft-20");
		}

		// An absent LOCATION_FILTER means the subscription is unfiltered.
		const raw = params.subscriptionFilter;
		const filter = raw !== undefined ? Filter.decode(raw, version) : { kind: "unfiltered" as const };
		const rawFill = params.fillParameters;
		const fill = rawFill !== undefined ? Filter.decodeFill(rawFill, version) : undefined;

		return new Subscribe({
			requestId,
			trackNamespace,
			trackName,
			subscriberPriority,
			filter,
			fill,
			// Defaults to 1, so an absent parameter means the subscriber wants them.
			propertiesWanted: params.includeProperties ?? true,
		});
	}
}

export class SubscribeOk {
	static id = 0x04;

	requestId: bigint | undefined;
	trackAlias: bigint;

	/**
	 * The largest Location in the track (LARGEST_OBJECT), which the draft requires once the
	 * track has content. It is what a subscriber sizes a fill against.
	 *
	 * Encoded on draft-20 only: the parameter is legal on earlier drafts too, but peers built
	 * before we sent it reject an unexpected SUBSCRIBE_OK parameter by closing the session,
	 * so emitting it there would break existing deployments over a hint.
	 */
	largest: MessageLocation | undefined;

	/**
	 * The Track Properties to send (draft-17+): the track's Timescale and our group order.
	 *
	 * Empty opts the response out of properties entirely, which is what a peer that sent
	 * INCLUDE_PROPERTIES=0 asked for; declaring no Timescale also opts the track out of
	 * timestamps, so the subscriber times objects by arrival.
	 */
	properties: Properties.Properties;

	constructor({
		requestId,
		trackAlias,
		largest,
		properties,
	}: {
		requestId?: bigint;
		trackAlias: bigint;
		largest?: MessageLocation;
		properties?: Properties.Properties;
	}) {
		this.requestId = requestId;
		this.trackAlias = trackAlias;
		this.largest = largest;
		this.properties = properties ?? {};
	}

	async #encode(w: Writer, version: IetfVersion): Promise<void> {
		if (version === Version.DRAFT_14 || version === Version.DRAFT_15 || version === Version.DRAFT_16) {
			if (this.requestId === undefined) throw new Error("requestId required for draft14-16");
			await w.u62(this.requestId);
		}
		await w.u62(this.trackAlias);

		if (version === Version.DRAFT_14) {
			await w.u62(0n); // expires = 0
			await w.u8(this.properties.groupOrder ?? GROUP_ORDER);
			await w.bool(false); // content exists = false
			await w.u53(0); // no parameters
		} else {
			// v15+: just parameters after track_alias
			const params = new Parameters();
			// GROUP_ORDER is a legal SUBSCRIBE_OK parameter only through draft-15; a later peer
			// closes the session with PROTOCOL_VIOLATION when it sees one. The publisher's
			// preference is a DEFAULT_PUBLISHER_GROUP_ORDER track property instead, which we
			// write from draft-17 on. Draft-16 gets neither form; see Properties.encode.
			if (version === Version.DRAFT_15 && this.properties.groupOrder !== undefined) {
				params.groupOrder = this.properties.groupOrder;
			}
			// See the field doc for why LARGEST_OBJECT stays draft-20 only.
			if (this.largest !== undefined && Filter.isDraft20(version)) {
				params.largest = this.largest;
			}
			await params.encode(w, version);

			// Track Properties are the final field, so nothing may follow.
			await Properties.encode(w, this.properties, version);
		}
	}

	async encode(w: Writer, version: IetfVersion): Promise<void> {
		return Message.encode(w, (mw) => this.#encode(mw, version));
	}

	static async decode(r: Reader, version: IetfVersion): Promise<SubscribeOk> {
		return Message.decode(r, (mr) => SubscribeOk.#decode(mr, version));
	}

	static async #decode(r: Reader, version: IetfVersion): Promise<SubscribeOk> {
		const requestId =
			version === Version.DRAFT_14 || version === Version.DRAFT_15 || version === Version.DRAFT_16
				? await r.u62()
				: undefined;
		const trackAlias = await r.u62();
		let largest: MessageLocation | undefined;
		let properties: Properties.Properties = {};

		if (version === Version.DRAFT_14) {
			const expires = await r.u62();
			if (expires !== BigInt(0)) {
				throw new Error(`unsupported expires: ${expires}`);
			}

			await r.u8(); // Don't care about group order

			const contentExists = await r.bool();
			if (contentExists) {
				const groupId = await r.u62();
				const objectId = await r.u62();
				largest = { groupId, objectId };
			}

			await Parameters.decode(r, version); // ignore parameters
		} else {
			// v15+: parameters followed by Track Properties (draft-17+). LARGEST_OBJECT is
			// required on every draft once the track has content, so rejecting it would tear
			// down a session over a parameter compliant publishers must send.
			largest = (await Parameters.decode(r, version)).largest;
			properties = await Properties.decode(r, version);
		}

		return new SubscribeOk({ requestId, trackAlias, largest, properties });
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

export class SubscribeUpdate {
	static id = 0x02;

	requestId: bigint;

	constructor({ requestId }: { requestId: bigint }) {
		this.requestId = requestId;
	}

	async #encode(w: Writer, version: IetfVersion): Promise<void> {
		if (version === Version.DRAFT_14) {
			await w.u62(this.requestId);
			await w.u62(0n); // subscription_request_id
			await w.u62(0n); // start_group
			await w.u62(0n); // start_object
			await w.u62(0n); // end_group
			await w.u8(128); // subscriber_priority
			await w.bool(true); // forward
			await w.u53(0); // no parameters
		} else if (version === Version.DRAFT_15 || version === Version.DRAFT_16) {
			await w.u62(this.requestId);
			await w.u62(0n); // subscription_request_id
			const params = new Parameters();
			await params.encode(w, version);
		} else {
			// v17+: REQUEST_UPDATE
			await w.u62(this.requestId);
			if (version === Version.DRAFT_17) {
				await w.u62(0n); // required_request_id_delta (draft-17 only, removed in draft-18 per #1615)
			}
			const params = new Parameters();
			await params.encode(w, version);
		}
	}

	async encode(w: Writer, version: IetfVersion): Promise<void> {
		return Message.encode(w, (mw) => this.#encode(mw, version));
	}

	static async decode(r: Reader, version: IetfVersion): Promise<SubscribeUpdate> {
		return Message.decode(r, (mr) => SubscribeUpdate.#decode(mr, version));
	}

	static async #decode(r: Reader, version: IetfVersion): Promise<SubscribeUpdate> {
		if (version === Version.DRAFT_14) {
			const requestId = await r.u62();
			await r.u62(); // subscription_request_id
			await r.u62(); // start_group
			await r.u62(); // start_object
			await r.u62(); // end_group
			await r.u8(); // subscriber_priority
			await r.bool(); // forward
			await Parameters.decode(r, version); // parameters
			return new SubscribeUpdate({ requestId });
		} else if (version === Version.DRAFT_15 || version === Version.DRAFT_16) {
			const requestId = await r.u62();
			await r.u62(); // subscription_request_id
			await Parameters.decode(r, version);
			return new SubscribeUpdate({ requestId });
		} else {
			// v17+: REQUEST_UPDATE
			const requestId = await r.u62();
			if (version === Version.DRAFT_17) {
				await r.u62(); // required_request_id_delta (draft-17 only, removed in draft-18 per #1615)
			}
			await Parameters.decode(r, version);
			return new SubscribeUpdate({ requestId });
		}
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
