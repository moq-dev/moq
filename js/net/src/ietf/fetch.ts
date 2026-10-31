import type { Milli } from "@moq/clock";
import type * as Epoch from "../epoch.ts";
import type * as Path from "../path.ts";
import { type Reader, Writer } from "../stream.ts";
import type { Order } from "../track.ts";
import * as Filter from "./filter.ts";
import * as Message from "./message.ts";
import * as Namespace from "./namespace.ts";
import { Parameters } from "./parameters.ts";
import { DEFAULT_WIRE } from "./priority.ts";
import * as Properties from "./properties.ts";
import { GroupOrder } from "./subscribe.ts";
import type { IetfVersion } from "./version.ts";

/**
 * The header that begins a fetch stream (draft-22 section 11.4), naming the request it
 * answers. Written by the publisher when serving a FETCH or a subscription's fill.
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

/** A `{Group, Object}` Location. */
export interface Location {
	/** The group's sequence number. */
	group: bigint;
	/** The object's id within that group. */
	object: bigint;
}

/**
 * What a FETCH asks for.
 *
 * Through draft-18 a Fetch Type tag picks one of the first three. Draft-22 dropped the tag
 * and the joining forms, leaving `filtered`.
 */
export type FetchType =
	/**
	 * A range of a track, through draft-18. `end` is one past the last object, or Object 0
	 * for the whole End Group.
	 */
	| { kind: "standalone"; trackNamespace: Path.Valid; trackName: string; start: Location; end: Location }
	/** The groups `offset` back from a subscription's Largest Object group, through draft-18. */
	| { kind: "relativeJoining"; subscription: bigint; offset: bigint }
	/** The groups from `group` up to a subscription's Largest Object group, through draft-18. */
	| { kind: "absoluteJoining"; subscription: bigint; group: bigint }
	/** A track with the range as a LOCATION_FILTER, from draft-22 on. */
	| { kind: "filtered"; trackNamespace: Path.Valid; trackName: string; filter: Filter.Filter };

/** The Fetch Type tags of draft-18. */
const STANDALONE = 0x1;
const RELATIVE_JOINING = 0x2;
const ABSOLUTE_JOINING = 0x3;

/** A FETCH request. */
export class Fetch {
	static id = 0x16;

	requestId: bigint;

	/** What the request asks for. */
	type: FetchType;

	/** The SUBSCRIBER_PRIORITY wire byte, lower first. */
	subscriberPriority: number;

	/** The order the response's groups go out in, or the publisher's pick when absent. */
	groupOrder?: Order;

	/** Whether the request carried Range Filters, which we refuse. Never encoded. */
	rangeFilters: boolean;

	/**
	 * FILL_TIMEOUT: the total wait for objects not yet available before they are reported as
	 * a gap, zero for only what is cached. Absent waits as long as the publisher chooses.
	 */
	fillTimeout?: Milli;

	/** Whether the requester wants Track Properties in FETCH_OK (INCLUDE_PROPERTIES, draft-22). */
	propertiesWanted: boolean;

	/** The publisher instance the requester expects (EPOCH, MoQ Epoch), never on a joining fetch. */
	epoch?: Epoch.Valid;

	constructor({
		requestId,
		type,
		subscriberPriority = DEFAULT_WIRE,
		groupOrder,
		rangeFilters = false,
		fillTimeout,
		propertiesWanted = true,
		epoch,
	}: {
		requestId: bigint;
		type: FetchType;
		subscriberPriority?: number;
		groupOrder?: Order;
		rangeFilters?: boolean;
		fillTimeout?: Milli;
		propertiesWanted?: boolean;
		epoch?: Epoch.Valid;
	}) {
		this.requestId = requestId;
		this.type = type;
		this.subscriberPriority = subscriberPriority;
		this.groupOrder = groupOrder;
		this.rangeFilters = rangeFilters;
		this.fillTimeout = fillTimeout;
		this.propertiesWanted = propertiesWanted;
		this.epoch = epoch;
	}

	async #encode(w: Writer, version: IetfVersion): Promise<void> {
		await w.u62(this.requestId);

		const params = new Parameters();
		params.subscriberPriority = this.subscriberPriority;
		if (this.groupOrder !== undefined)
			params.groupOrder = this.groupOrder === "ascending" ? GroupOrder.Ascending : GroupOrder.Descending;
		params.epoch = this.epoch;
		params.fillTimeout = this.fillTimeout;

		const type = this.type;
		if (Filter.isDraft20(version)) {
			// The joining forms and the Fetch Type tag are gone in draft-22.
			if (type.kind !== "filtered") throw new Error(`${type.kind} FETCH needs draft-18`);
			await Namespace.encodeTrack(w, type.trackNamespace, type.trackName);
			params.subscriptionFilter = type.filter;
			// The default is 1, so only the opt-out is worth bytes.
			if (!this.propertiesWanted) params.includeProperties = false;
		} else {
			switch (type.kind) {
				case "standalone":
					await w.u53(STANDALONE);
					await Namespace.encodeTrack(w, type.trackNamespace, type.trackName);
					await w.u62(type.start.group);
					await w.u62(type.start.object);
					await w.u62(type.end.group);
					await w.u62(type.end.object);
					break;
				case "relativeJoining":
					await w.u53(RELATIVE_JOINING);
					await w.u62(type.subscription);
					await w.u62(type.offset);
					break;
				case "absoluteJoining":
					await w.u53(ABSOLUTE_JOINING);
					await w.u62(type.subscription);
					await w.u62(type.group);
					break;
				case "filtered":
					throw new Error("filtered FETCH needs draft-22");
			}
		}
		await params.encode(w, version);
	}

	async encode(w: Writer, version: IetfVersion): Promise<void> {
		return Message.encode(w, (mw) => this.#encode(mw, version));
	}

	static async decode(r: Reader, version: IetfVersion): Promise<Fetch> {
		return Message.decode(r, (mr) => Fetch.#decode(mr, version));
	}

	static async #decode(r: Reader, version: IetfVersion): Promise<Fetch> {
		const requestId = await r.u62();

		let type: FetchType;
		if (Filter.isDraft20(version)) {
			// Draft-22 names the track up front and moves the range into LOCATION_FILTER.
			const { namespace, name } = await Namespace.decodeTrack(r);
			// Placeholder until the parameters name the filter.
			type = { kind: "filtered", trackNamespace: namespace, trackName: name, filter: { kind: "unfiltered" } };
		} else {
			const fetchType = await r.u53();
			switch (fetchType) {
				case STANDALONE: {
					const { namespace, name } = await Namespace.decodeTrack(r);
					const start = { group: await r.u62(), object: await r.u62() };
					const end = { group: await r.u62(), object: await r.u62() };
					type = { kind: "standalone", trackNamespace: namespace, trackName: name, start, end };
					break;
				}
				case RELATIVE_JOINING:
					type = { kind: "relativeJoining", subscription: await r.u62(), offset: await r.u62() };
					break;
				case ABSOLUTE_JOINING:
					type = { kind: "absoluteJoining", subscription: await r.u62(), group: await r.u62() };
					break;
				default:
					throw new Error(`unknown fetch type: ${fetchType}`);
			}
		}

		const params = await Parameters.decode(r, version, "fetch");
		if (params.rangeFilters && !Filter.hasRangeFilters(version)) {
			throw new Error("Range Filters need draft-22");
		}
		if (params.trackPropertyFilter) {
			throw new Error("TRACK_PROPERTY_FILTER is not allowed on FETCH");
		}
		// A joining FETCH is served by its subscription's instance, so naming one is malformed.
		const epoch = params.epoch;
		if (epoch !== undefined && (type.kind === "relativeJoining" || type.kind === "absoluteJoining")) {
			throw new Error("EPOCH is not allowed on a joining FETCH");
		}
		// An absent LOCATION_FILTER fetches the whole track.
		if (type.kind === "filtered") type.filter = params.subscriptionFilter ?? { kind: "unfiltered" };

		const order = params.groupOrder;
		return new Fetch({
			requestId,
			type,
			subscriberPriority: params.subscriberPriority ?? DEFAULT_WIRE,
			// Parameters refuses anything but Ascending or Descending.
			groupOrder: order === undefined ? undefined : order === GroupOrder.Ascending ? "ascending" : "descending",
			rangeFilters: params.rangeFilters,
			fillTimeout: params.fillTimeout,
			// Defaults to 1, so an absent parameter means the requester wants them.
			propertiesWanted: params.includeProperties ?? true,
			epoch,
		});
	}
}

/** The answer to a FETCH, sent before its fetch stream. */
export class FetchOk {
	static id = 0x18;

	/** Whether the response holds the track's final object. */
	endOfTrack: boolean;

	/**
	 * Where the response ends. Draft-18 uses the last object plus one, or Object 0 for the
	 * whole End Group; draft-22 and later an inclusive End Location.
	 */
	endLocation: Location;

	/** The track's properties, as SUBSCRIBE_OK carries them. */
	properties: Properties.Properties;

	constructor({
		endOfTrack,
		endLocation,
		properties = {},
	}: {
		endOfTrack: boolean;
		endLocation: Location;
		properties?: Properties.Properties;
	}) {
		this.endOfTrack = endOfTrack;
		this.endLocation = endLocation;
		this.properties = properties;
	}

	async #encode(w: Writer, version: IetfVersion): Promise<void> {
		await w.bool(this.endOfTrack);
		await w.u62(this.endLocation.group);
		await w.u62(this.endLocation.object);
		// FETCH_OK defines no parameters.
		await new Parameters().encode(w, version);
		// Track Properties are the final field, so nothing may follow.
		await Properties.encode(w, this.properties, version);
	}

	async encode(w: Writer, version: IetfVersion): Promise<void> {
		return Message.encode(w, (mw) => this.#encode(mw, version));
	}

	static async decode(r: Reader, version: IetfVersion): Promise<FetchOk> {
		return Message.decode(r, (mr) => FetchOk.#decode(mr, version));
	}

	static async #decode(r: Reader, version: IetfVersion): Promise<FetchOk> {
		const endOfTrack = await r.bool();
		const endLocation = { group: await r.u62(), object: await r.u62() };
		// FETCH_OK lists no parameters, so any is a protocol violation.
		await Parameters.decode(r, version, "fetch-ok");
		const properties = await Properties.decode(r, version);
		return new FetchOk({ endOfTrack, endLocation, properties });
	}
}
