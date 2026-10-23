import type * as Epoch from "../epoch.ts";
import type * as Path from "../path.ts";
import type { Reader, Writer } from "../stream.ts";
import type { Order } from "../track.ts";
import { supported as epochSupported } from "./epoch.ts";
import * as Filter from "./filter.ts";
import * as Message from "./message.ts";
import * as Namespace from "./namespace.ts";
import { type MessageLocation, MSG_PARAM_FORWARD, MSG_PARAM_SUBSCRIBER_PRIORITY, Parameters } from "./parameters.ts";
import { ABSENT_WIRE } from "./priority.ts";
import * as Properties from "./properties.ts";
import { type IetfVersion, Version } from "./version.ts";

/**
 * The GROUP_ORDER wire values.
 *
 * `Default` (0x0) is the wire's "no preference": the publisher picks, which in practice
 * means its own DEFAULT_PUBLISHER_GROUP_ORDER track property. It decodes as descending, the
 * order this publisher prefers, so the model never sees it and this client never sends it.
 * Only drafts 14 to 16 can carry it; {@link Parameters} rejects it as a protocol violation
 * on later drafts.
 */
export const GroupOrder = {
	Default: 0x00,
	Ascending: 0x01,
	Descending: 0x02,
} as const;

/** A GROUP_ORDER wire value. */
export type GroupOrder = (typeof GroupOrder)[keyof typeof GroupOrder];

/** The GROUP_ORDER byte for a model {@link Order}. */
function encodeGroupOrder(order: Order): GroupOrder {
	return order === "ascending" ? GroupOrder.Ascending : GroupOrder.Descending;
}

/** The model {@link Order} for a GROUP_ORDER byte, refusing a value the draft does not define. */
function decodeGroupOrder(value: number): Order {
	switch (value) {
		case GroupOrder.Ascending:
			return "ascending";
		case GroupOrder.Default:
		case GroupOrder.Descending:
			return "descending";
		default:
			throw new Error(`unknown group order: ${value}`);
	}
}

/**
 * The filter this implementation joins a live track with.
 *
 * moq-lite joins at the start of the current group, which is a decodable point, and
 * draft-20's relative form is the first that can name it without knowing Largest Object.
 * Earlier drafts request unfiltered delivery because this client does not send joining FETCH.
 */
export function joinFilter(version: IetfVersion): Filter.Filter {
	return Filter.isDraft20(version) ? { kind: "relative", groups: 1n } : { kind: "unfiltered" };
}

export class Subscribe {
	static id = 0x03;

	requestId: bigint;
	trackNamespace: Path.Valid;
	trackName: string;
	subscriberPriority: number;
	/** Subscriber max age in milliseconds (MoQ Timestamp), if negotiated. */
	maxAge?: number;

	/** The order the publisher is asked to deliver groups in (GROUP_ORDER). Defaults to descending. */
	groupOrder: Order;

	/** Which Objects the subscription delivers, resolved by the publisher against the live edge. */
	filter: Filter.Filter;

	/** The draft-20 backfill requested alongside it, or `undefined` for none. */
	fill?: Filter.Fill;

	/** Whether the subscriber wants Track Properties on the response (INCLUDE_PROPERTIES). */
	propertiesWanted: boolean;

	/** Whether Objects are forwarded (FORWARD). We only serve forwarding subscriptions. */
	forward: boolean;

	/** Whether the request carried a Range Filter, which we refuse. Never encoded. */
	rangeFilters: boolean;

	/** Whether RENDEZVOUS_TIMEOUT requested a wait; never encoded. */
	rendezvous: boolean;

	/**
	 * The publisher instance the subscriber expects (EPOCH, MoQ Epoch, draft-17+). Only a
	 * session that negotiated the extension names one; the session refuses one otherwise.
	 */
	epoch?: Epoch.Valid;

	constructor({
		requestId,
		trackNamespace,
		trackName,
		subscriberPriority,
		maxAge,
		groupOrder,
		filter,
		fill,
		propertiesWanted,
		forward,
		rangeFilters,
		rendezvous,
		epoch,
	}: {
		requestId: bigint;
		trackNamespace: Path.Valid;
		trackName: string;
		subscriberPriority: number;
		maxAge?: number;
		groupOrder?: Order;
		filter?: Filter.Filter;
		fill?: Filter.Fill;
		propertiesWanted?: boolean;
		forward?: boolean;
		rangeFilters?: boolean;
		rendezvous?: boolean;
		epoch?: Epoch.Valid;
	}) {
		this.requestId = requestId;
		this.trackNamespace = trackNamespace;
		this.trackName = trackName;
		this.subscriberPriority = subscriberPriority;
		this.maxAge = maxAge;
		this.groupOrder = groupOrder ?? "descending";
		this.filter = filter ?? { kind: "unfiltered" };
		this.fill = fill;
		this.propertiesWanted = propertiesWanted ?? true;
		this.forward = forward ?? true;
		this.rangeFilters = rangeFilters ?? false;
		this.rendezvous = rendezvous ?? false;
		this.epoch = epoch;
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
			await w.u8(encodeGroupOrder(this.groupOrder));
			await w.bool(this.forward);
			await w.write(Filter.encode(this.filter, version));
			const params = new Parameters();
			params.maxAge = this.maxAge;
			await params.encode(w, version);
		} else {
			// v15+: fields moved into parameters
			const params = new Parameters();
			params.subscriberPriority = this.subscriberPriority;
			params.maxAge = this.maxAge;
			params.groupOrder = encodeGroupOrder(this.groupOrder);
			params.forward = this.forward;
			params.subscriptionFilter = this.filter;

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
			// The same for EPOCH, which exists from draft-17.
			if (epochSupported(version)) params.epoch = this.epoch;

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
		const { namespace: trackNamespace, name: trackName } = await Namespace.decodeTrack(r);

		if (version === Version.DRAFT_14) {
			const subscriberPriority = await r.u8();

			const groupOrder = decodeGroupOrder(await r.u8());

			const forward = await r.bool();
			const filter = await Filter.decodeInline(r);

			const params = await Parameters.decode(r, version, "subscribe");

			return new Subscribe({
				requestId,
				maxAge: params.maxAge,
				trackNamespace,
				trackName,
				subscriberPriority,
				groupOrder,
				filter,
				forward,
			});
		}
		// v15+: fields are in parameters
		const params = await Parameters.decode(r, version, "subscribe");
		const subscriberPriority = params.subscriberPriority ?? ABSENT_WIRE;
		const groupOrder = decodeGroupOrder(params.groupOrder ?? GroupOrder.Descending);

		// FILL_PARAMETERS and INCLUDE_PROPERTIES are draft-20 additions, and the Range
		// Filters draft-19 ones. An unknown message parameter is a protocol violation, so
		// they stay rejected on the drafts that predate them rather than being quietly
		// tolerated.
		const draft20 = Filter.isDraft20(version);
		if ((params.fillParameters !== undefined || params.includeProperties !== undefined) && !draft20) {
			throw new Error("FILL_PARAMETERS and INCLUDE_PROPERTIES need draft-20");
		}
		if (params.rangeFilters && !Filter.hasRangeFilters(version)) {
			throw new Error("Range Filters need draft-19");
		}
		if (params.trackPropertyFilter) {
			throw new Error("TRACK_PROPERTY_FILTER is not allowed on SUBSCRIBE");
		}

		// An absent LOCATION_FILTER means the subscription is unfiltered.
		const filter = params.subscriptionFilter ?? { kind: "unfiltered" as const };
		const rawFill = params.fillParameters;
		const fill = rawFill !== undefined ? Filter.decodeFill(rawFill, version) : undefined;

		return new Subscribe({
			maxAge: params.maxAge,
			requestId,
			trackNamespace,
			trackName,
			subscriberPriority,
			groupOrder,
			filter,
			fill,
			// Defaults to 1, so an absent parameter means the subscriber wants them.
			propertiesWanted: params.includeProperties ?? true,
			forward: params.forward ?? true,
			rangeFilters: params.rangeFilters,
			rendezvous: (params.vars.get(0x04n) ?? 0n) > 0n,
			epoch: params.epoch,
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
			await w.u8(this.properties.groupOrder ?? GroupOrder.Descending);
			await w.bool(this.largest !== undefined);
			if (this.largest !== undefined) {
				await w.u62(this.largest.groupId);
				await w.u62(this.largest.objectId);
			}
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
			if (this.largest !== undefined) {
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
			// EXPIRES is when the publisher expects to end the subscription. That end
			// arrives as PUBLISH_DONE regardless, so there is nothing to act on.
			await r.u62();

			properties.groupOrder = await r.u8();

			const contentExists = await r.bool();
			if (contentExists) {
				const groupId = await r.u62();
				const objectId = await r.u62();
				largest = { groupId, objectId };
			}

			properties.maxCacheDuration = (await Parameters.decode(r, version, "subscribe-ok")).maxCacheDuration;
		} else {
			// v15+: parameters followed by Track Properties (draft-17+). LARGEST_OBJECT is
			// required on every draft once the track has content, so rejecting it would tear
			// down a session over a parameter compliant publishers must send.
			const params = await Parameters.decode(r, version, "subscribe-ok");
			largest = params.largest;
			properties = await Properties.decode(r, version);
			if (version === Version.DRAFT_15) {
				properties.maxCacheDuration = params.maxCacheDuration;
				properties.groupOrder = params.groupOrder;
			}
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

	/** This update's own Request ID, the first field on drafts 14 to 16, which their answer names. */
	ownRequestId?: bigint;

	/** What the update changes. Draft 14's fixed SUBSCRIBER_PRIORITY and FORWARD fields land here too. */
	parameters: Parameters;

	constructor({
		requestId,
		ownRequestId,
		parameters = new Parameters(),
	}: { requestId: bigint; ownRequestId?: bigint; parameters?: Parameters }) {
		this.requestId = requestId;
		this.ownRequestId = ownRequestId;
		this.parameters = parameters;
	}

	async #encode(w: Writer, version: IetfVersion): Promise<void> {
		// Drafts 14 to 16 send a new Request ID and then the request being updated.
		// Later drafts dropped the new id, so the one field left is the target.
		if (version === Version.DRAFT_14 || version === Version.DRAFT_15 || version === Version.DRAFT_16) {
			if (this.ownRequestId === undefined) throw new Error("ownRequestId required for draft14-16");
			await w.u62(this.ownRequestId);
			await w.u62(this.requestId);
			if (version === Version.DRAFT_14) {
				// Every field is mandatory and the start must not move. Our subscriptions start at
				// {0, 0} open ended (`joinFilter`), so zeroes repeat their bounds.
				await w.u62(0n); // start_group
				await w.u62(0n); // start_object
				await w.u62(0n); // end_group
				await w.u8(this.parameters.subscriberPriority ?? ABSENT_WIRE);
				await w.bool(this.parameters.forward ?? true);
				await this.parameters.encode(w, version, [MSG_PARAM_SUBSCRIBER_PRIORITY, MSG_PARAM_FORWARD]);
			} else {
				await this.parameters.encode(w, version);
			}
		} else {
			if (this.ownRequestId !== undefined) throw new Error("ownRequestId is only valid on draft14-16");
			// v17+: REQUEST_UPDATE
			await w.u62(this.requestId);
			if (version === Version.DRAFT_17) {
				await w.u62(0n); // required_request_id_delta (draft-17 only, removed in draft-18 per #1615)
			}
			await this.parameters.encode(w, version);
		}
	}

	async encode(w: Writer, version: IetfVersion): Promise<void> {
		return Message.encode(w, (mw) => this.#encode(mw, version));
	}

	static async decode(r: Reader, version: IetfVersion): Promise<SubscribeUpdate> {
		return Message.decode(r, (mr) => SubscribeUpdate.#decode(mr, version));
	}

	static async #decode(r: Reader, version: IetfVersion): Promise<SubscribeUpdate> {
		if (version === Version.DRAFT_14 || version === Version.DRAFT_15 || version === Version.DRAFT_16) {
			const ownRequestId = await r.u62();
			const requestId = await r.u62();
			if (version !== Version.DRAFT_14) {
				const parameters = await Parameters.decode(r, version, "subscribe-update");
				return new SubscribeUpdate({ requestId, ownRequestId, parameters });
			}
			// The range is left as it is: every field is mandatory, so a narrowed range cannot be
			// told from a repeated one, and draft 14 accepts extra objects as the worst outcome
			// of an update.
			await r.u62(); // start_group
			await r.u62(); // start_object
			await r.u62(); // end_group
			const priority = await r.u8();
			const forward = await r.bool();
			const parameters = await Parameters.decode(r, version, "subscribe-update");
			parameters.subscriberPriority = priority;
			parameters.forward = forward;
			return new SubscribeUpdate({ requestId, ownRequestId, parameters });
		}

		// v17+: REQUEST_UPDATE. Request ID is the existing request.
		const requestId = await r.u62();
		if (version === Version.DRAFT_17) {
			await r.u62(); // required_request_id_delta (draft-17 only, removed in draft-18 per #1615)
		}
		const parameters = await Parameters.decode(r, version, "subscribe-update");
		return new SubscribeUpdate({ requestId, parameters });
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
