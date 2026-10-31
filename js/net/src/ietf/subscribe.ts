import type * as Epoch from "../epoch.ts";
import type * as Path from "../path.ts";
import type { Reader, Writer } from "../stream.ts";
import type { Order } from "../track.ts";
import * as Filter from "./filter.ts";
import * as Message from "./message.ts";
import * as Namespace from "./namespace.ts";
import { type MessageLocation, Parameters } from "./parameters.ts";
import { DEFAULT_WIRE } from "./priority.ts";
import * as Properties from "./properties.ts";
import type { IetfVersion } from "./version.ts";

/**
 * The GROUP_ORDER wire values.
 *
 * `Default` (0x0) is the wire's "no preference": the publisher picks, which in practice
 * means its own DEFAULT_PUBLISHER_GROUP_ORDER track property. It decodes as descending, the
 * order this publisher prefers, so the model never sees it and this client never sends it.
 * {@link Parameters} rejects it as a protocol violation.
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
 * How this implementation joins a live track: at the current group, from its first object,
 * as the Rust subscriber does.
 *
 * The subscription itself starts at the next object (draft 18's Largest Object), and the
 * group's head comes on a fetch stream. Draft 22 asks for it with a fill one group back from
 * the next, its current-group join (section 3.5); earlier drafts have no fill, so the
 * subscriber sends a Relative Joining FETCH once SUBSCRIBE_OK names Largest. Spelling the
 * filter keeps a peer from reading an absent one its own way.
 */
export function joinFilter(version: IetfVersion): { filter: Filter.Filter; fill?: Filter.Fill } {
	const filter: Filter.Filter = { kind: "nextObject" };
	if (!Filter.isDraft22(version)) return { filter };
	return { filter, fill: { filter: { kind: "relative", groups: 1n }, rangeFilters: false } };
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

	/** The draft-22 backfill requested alongside it, or `undefined` for none. */
	fill?: Filter.Fill;

	/** Whether the subscriber wants Track Properties on the response (INCLUDE_PROPERTIES). */
	propertiesWanted: boolean;

	/** Whether Objects are forwarded (FORWARD); a paused subscription forwards none until resumed. */
	forward: boolean;

	/** Whether the request carried a Range Filter, which we refuse. Never encoded. */
	rangeFilters: boolean;

	/** Whether RENDEZVOUS_TIMEOUT requested a wait; never encoded. */
	rendezvous: boolean;

	/**
	 * The publisher instance the subscriber expects (EPOCH, MoQ Epoch). Only a
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
		await Namespace.encodeTrack(w, this.trackNamespace, this.trackName);

		const params = new Parameters();
		params.subscriberPriority = this.subscriberPriority;
		params.maxAge = this.maxAge;
		params.groupOrder = encodeGroupOrder(this.groupOrder);
		params.forward = this.forward;
		params.subscriptionFilter = this.filter;

		// FILL_PARAMETERS and INCLUDE_PROPERTIES arrived in draft-22. An older peer reads
		// either as an unknown parameter, which is a protocol violation, so they are
		// dropped rather than downgraded.
		if (Filter.isDraft22(version)) {
			if (this.fill) {
				params.fillParameters = Filter.encodeFill(this.fill, version);
			}
			// The default is 1, so only the opt-out is worth bytes.
			if (!this.propertiesWanted) {
				params.includeProperties = false;
			}
		}
		params.epoch = this.epoch;

		await params.encode(w, version);
	}

	async encode(w: Writer, version: IetfVersion): Promise<void> {
		return Message.encode(w, (mw) => this.#encode(mw, version));
	}

	static async decode(r: Reader, version: IetfVersion): Promise<Subscribe> {
		return Message.decode(r, (mr) => Subscribe.#decode(mr, version));
	}

	static async #decode(r: Reader, version: IetfVersion): Promise<Subscribe> {
		const requestId = await r.u62();
		const { namespace: trackNamespace, name: trackName } = await Namespace.decodeTrack(r);

		const params = await Parameters.decode(r, version, "subscribe");
		const subscriberPriority = params.subscriberPriority ?? DEFAULT_WIRE;
		const groupOrder = decodeGroupOrder(params.groupOrder ?? GroupOrder.Descending);

		// FILL_PARAMETERS, INCLUDE_PROPERTIES, and the Range Filters are draft-22
		// additions. An unknown message parameter is a protocol violation, so
		// they stay rejected on the drafts that predate them rather than being quietly
		// tolerated.
		const draft22 = Filter.isDraft22(version);
		if ((params.fillParameters !== undefined || params.includeProperties !== undefined) && !draft22) {
			throw new Error("FILL_PARAMETERS and INCLUDE_PROPERTIES need draft-22");
		}
		if (params.rangeFilters && !Filter.hasRangeFilters(version)) {
			throw new Error("Range Filters need draft-22");
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

	trackAlias: bigint;

	/**
	 * The largest Location in the track (LARGEST_OBJECT), which the draft requires once the
	 * track has content. It is what a subscriber sizes a fill against.
	 */
	largest: MessageLocation | undefined;

	/**
	 * The Track Properties to send: the track's Timescale and our group order.
	 *
	 * Empty opts the response out of properties entirely, which is what a peer that sent
	 * INCLUDE_PROPERTIES=0 asked for; declaring no Timescale also opts the track out of
	 * timestamps, so the subscriber times objects by arrival.
	 */
	properties: Properties.Properties;

	constructor({
		trackAlias,
		largest,
		properties,
	}: {
		trackAlias: bigint;
		largest?: MessageLocation;
		properties?: Properties.Properties;
	}) {
		this.trackAlias = trackAlias;
		this.largest = largest;
		this.properties = properties ?? {};
	}

	async #encode(w: Writer, version: IetfVersion): Promise<void> {
		await w.u62(this.trackAlias);

		// The publisher's group order is a DEFAULT_PUBLISHER_GROUP_ORDER track property: a
		// peer closes the session on a GROUP_ORDER parameter here.
		const params = new Parameters();
		if (this.largest !== undefined) {
			params.largest = this.largest;
		}
		await params.encode(w, version);

		// Track Properties are the final field, so nothing may follow.
		await Properties.encode(w, this.properties, version);
	}

	async encode(w: Writer, version: IetfVersion): Promise<void> {
		return Message.encode(w, (mw) => this.#encode(mw, version));
	}

	static async decode(r: Reader, version: IetfVersion): Promise<SubscribeOk> {
		return Message.decode(r, (mr) => SubscribeOk.#decode(mr, version));
	}

	static async #decode(r: Reader, version: IetfVersion): Promise<SubscribeOk> {
		const trackAlias = await r.u62();
		// LARGEST_OBJECT is required once the track has content, so rejecting it would tear
		// down a session over a parameter compliant publishers must send.
		const params = await Parameters.decode(r, version, "subscribe-ok");
		const properties = await Properties.decode(r, version);

		return new SubscribeOk({ trackAlias, largest: params.largest, properties });
	}
}

export class SubscribeUpdate {
	static id = 0x02;

	/** The request being updated. */
	requestId: bigint;

	/** What the update changes. */
	parameters: Parameters;

	constructor({ requestId, parameters = new Parameters() }: { requestId: bigint; parameters?: Parameters }) {
		this.requestId = requestId;
		this.parameters = parameters;
	}

	async #encode(w: Writer, version: IetfVersion): Promise<void> {
		await w.u62(this.requestId);
		await this.parameters.encode(w, version);
	}

	async encode(w: Writer, version: IetfVersion): Promise<void> {
		return Message.encode(w, (mw) => this.#encode(mw, version));
	}

	static async decode(r: Reader, version: IetfVersion): Promise<SubscribeUpdate> {
		return Message.decode(r, (mr) => SubscribeUpdate.#decode(mr, version));
	}

	static async #decode(r: Reader, version: IetfVersion): Promise<SubscribeUpdate> {
		const requestId = await r.u62();
		const parameters = await Parameters.decode(r, version, "subscribe-update");
		return new SubscribeUpdate({ requestId, parameters });
	}
}
