import { Milli } from "@moq/clock";
import * as Epoch from "../epoch.ts";
import { ProtocolViolation, reason } from "../error.ts";
import type * as Path from "../path.ts";
import type { Reader, Writer } from "../stream.ts";
import * as Filter from "./filter.ts";
import * as Namespace from "./namespace.ts";
import { AUTHORIZATION_TOKEN, type Token, tokenFromRequest, unique } from "./token.ts";
import { type IetfVersion, Version } from "./version.ts";

// ---- Message Parameters (used in Subscribe, Publish, Fetch, etc.) ----
// A count, then definition-specific Type-Value pairs with delta-encoded types.

// Varint parameter IDs (even)
const MSG_PARAM_DELIVERY_TIMEOUT = 0x02n;
/// FILL_TIMEOUT, on a FETCH: the wait for missing objects before they become a gap.
const MSG_PARAM_FILL_TIMEOUT = 0x0an;
/// NEW_GROUP_REQUEST. Ignored, as the draft lets a publisher without dynamic groups do.
const MSG_PARAM_NEW_GROUP_REQUEST = 0x32n;
/// SUBGROUP_DELIVERY_TIMEOUT, alongside the per-object one above.
const MSG_PARAM_SUBGROUP_DELIVERY_TIMEOUT = 0x06n;
const MSG_PARAM_MAX_CACHE_DURATION = 0x04n;
const MSG_PARAM_EXPIRES = 0x08n;
const MSG_PARAM_PUBLISHER_PRIORITY = 0x0en;
export const MSG_PARAM_FORWARD = 0x10n;
export const MSG_PARAM_SUBSCRIBER_PRIORITY = 0x20n;
const MSG_PARAM_GROUP_ORDER = 0x22n;
/// INCLUDE_PROPERTIES, draft-22's opt-out from Track Properties.
const MSG_PARAM_INCLUDE_PROPERTIES = 0x35n;
/// ROUTE_COST, from the MoQ Cluster extension. See `cluster.ts`.
const MSG_PARAM_ROUTE_COST = 0x40b58n;
/// HIDDEN, from the MoQ Hidden extension. See `hidden.ts`.
const MSG_PARAM_HIDDEN = 0x40b5en;
const MSG_PARAM_MAX_AGE = 0x915c6n;

// Bytes parameter IDs (odd)
/// AUTHORIZATION TOKEN. Decoded, but grants nothing: the session's grant is what authorizes a request.
const MSG_PARAM_AUTHORIZATION_TOKEN = AUTHORIZATION_TOKEN;
const MSG_PARAM_LARGEST_OBJECT = 0x09n;
const MSG_PARAM_SUBSCRIPTION_FILTER = 0x21n;
/// FILL_PARAMETERS, draft-22's request for a backfill.
const MSG_PARAM_FILL_PARAMETERS = 0x23n;
/// HOP_PATH, from the MoQ Cluster extension. See `cluster.ts`.
const MSG_PARAM_HOP_PATH = 0x40b57n;
/// ACTIVE_COUNT, from the MoQ Active Count extension.
const MSG_PARAM_ACTIVE_COUNT = 0x40b66n;
/// EPOCH, from the MoQ Epoch extension: a UUIDv7's 16 bytes. See `epoch.ts`.
const MSG_PARAM_EPOCH = 0x40b69n;

/** Which control message the parameter block belongs to. Omitted keeps the unfiltered decode. */
type ControlMessage =
	| "subscribe"
	| "subscribe-ok"
	| "subscribe-update"
	| "subscribe-namespace"
	| "subscribe-tracks"
	| "tracks-update"
	| "publish-ok"
	| "publish"
	| "publish-namespace"
	| "fetch"
	| "fetch-ok"
	| "request-ok"
	| "request-update"
	| "namespace"
	| "namespace-update";

/** True when this draft's definition of `message` includes `id`. */
function allows(message: ControlMessage, version: IetfVersion, id: bigint): boolean {
	const from22 = version >= Version.DRAFT_22;
	const auth = id === MSG_PARAM_AUTHORIZATION_TOKEN;
	const forward = id === MSG_PARAM_FORWARD;
	const priority = id === MSG_PARAM_SUBSCRIBER_PRIORITY;
	const filter = id === MSG_PARAM_SUBSCRIPTION_FILTER;
	const order = id === MSG_PARAM_GROUP_ORDER;
	const expires = id === MSG_PARAM_EXPIRES;
	const largest = id === MSG_PARAM_LARGEST_OBJECT;
	const objectTimeout = id === MSG_PARAM_DELIVERY_TIMEOUT;
	const subgroupTimeout = id === MSG_PARAM_SUBGROUP_DELIVERY_TIMEOUT;
	const range = id >= 0x25n && id <= 0x28n && from22;
	const trackRange = id === MSG_PARAM_TRACK_PROPERTY_FILTER && from22;
	const fill = id === MSG_PARAM_FILL_PARAMETERS && from22;
	const include = id === MSG_PARAM_INCLUDE_PROPERTIES && from22;
	const newGroup = id === MSG_PARAM_NEW_GROUP_REQUEST;
	const epoch = id === MSG_PARAM_EPOCH;

	switch (message) {
		case "subscribe-tracks":
			return (
				auth ||
				forward ||
				(from22 &&
					(objectTimeout ||
						id === MSG_PARAM_MAX_CACHE_DURATION ||
						subgroupTimeout ||
						priority ||
						filter ||
						order ||
						fill ||
						range ||
						trackRange ||
						newGroup ||
						include))
			);
		case "tracks-update":
			return auth || id === 0x34n || forward || priority || filter || fill || range || trackRange || newGroup;
		case "publish-ok":
			return (
				expires ||
				(!from22 && (forward || priority || filter || order || objectTimeout || subgroupTimeout || newGroup))
			);
		case "subscribe":
			return (
				id === MSG_PARAM_MAX_AGE ||
				objectTimeout ||
				auth ||
				id === MSG_PARAM_MAX_CACHE_DURATION ||
				subgroupTimeout ||
				forward ||
				priority ||
				filter ||
				order ||
				fill ||
				range ||
				newGroup ||
				include ||
				epoch
			);
		case "subscribe-ok":
			return expires || largest;
		case "subscribe-update":
		case "request-update":
			return (
				id === MSG_PARAM_MAX_AGE ||
				objectTimeout ||
				auth ||
				subgroupTimeout ||
				forward ||
				priority ||
				filter ||
				fill ||
				range ||
				trackRange ||
				newGroup
			);
		case "namespace-update":
			return auth || id === 0x34n;
		case "subscribe-namespace":
			return auth || id === MSG_PARAM_HIDDEN;
		case "publish":
			return (
				(objectTimeout && from22) ||
				auth ||
				(subgroupTimeout && from22) ||
				expires ||
				largest ||
				forward ||
				(priority && from22) ||
				(filter && from22) ||
				(order && from22)
			);
		case "publish-namespace":
			return auth || id === MSG_PARAM_HOP_PATH || id === MSG_PARAM_ROUTE_COST || epoch;
		case "fetch":
			return (
				auth ||
				id === MSG_PARAM_FILL_TIMEOUT ||
				priority ||
				(filter && from22) ||
				order ||
				range ||
				include ||
				epoch
			);
		case "fetch-ok":
			return false;
		case "request-ok":
			return expires || largest || id === MSG_PARAM_ACTIVE_COUNT;
		case "namespace":
			return id === MSG_PARAM_HOP_PATH || id === MSG_PARAM_ROUTE_COST || epoch;
	}
}

/// The object Range Filters (draft-22): SUBGROUP, OBJECTID, PRIORITY and OBJECT_PROPERTY.
/// Each is length prefixed whatever the parity of its id.
const MSG_PARAM_RANGE_FILTERS: readonly bigint[] = [0x25n, 0x26n, 0x27n, 0x28n];
/// TRACK_PROPERTY_FILTER, the Range Filter legal only on SUBSCRIBE_TRACKS and its updates.
const MSG_PARAM_TRACK_PROPERTY_FILTER = 0x29n;

/// The parameters whose definitions let them repeat within one message.
const MSG_PARAM_REPEATABLE: readonly bigint[] = [
	MSG_PARAM_AUTHORIZATION_TOKEN,
	...MSG_PARAM_RANGE_FILTERS,
	MSG_PARAM_TRACK_PROPERTY_FILTER,
];

type MessageParamKind = "varint" | "uint8" | "bool" | "location" | "filter" | "bytes" | "namespace";
/** A `{Group, Object}` pair carried by a message parameter, such as LARGEST_OBJECT. */
export type MessageLocation = { groupId: bigint; objectId: bigint };

function getMessageParamKind(id: bigint): MessageParamKind {
	switch (id) {
		case 0x34n:
			return "namespace";
		case MSG_PARAM_DELIVERY_TIMEOUT:
		case MSG_PARAM_FILL_TIMEOUT:
		case MSG_PARAM_NEW_GROUP_REQUEST:
		case MSG_PARAM_SUBGROUP_DELIVERY_TIMEOUT:
		case MSG_PARAM_MAX_CACHE_DURATION:
		case MSG_PARAM_EXPIRES:
		case MSG_PARAM_ROUTE_COST:
		case MSG_PARAM_HIDDEN:
		case MSG_PARAM_MAX_AGE:
		case MSG_PARAM_ACTIVE_COUNT:
			return "varint";
		case MSG_PARAM_PUBLISHER_PRIORITY:
		case MSG_PARAM_SUBSCRIBER_PRIORITY:
		case MSG_PARAM_GROUP_ORDER:
		case MSG_PARAM_INCLUDE_PROPERTIES:
			return "uint8";
		case MSG_PARAM_FORWARD:
			return "bool";
		case MSG_PARAM_LARGEST_OBJECT:
			return "location";
		case MSG_PARAM_SUBSCRIPTION_FILTER:
			return "filter";
		case MSG_PARAM_AUTHORIZATION_TOKEN:
		case MSG_PARAM_FILL_PARAMETERS:
		case MSG_PARAM_HOP_PATH:
		case MSG_PARAM_EPOCH:
			return "bytes";
		default:
			if (MSG_PARAM_REPEATABLE.includes(id)) return "bytes";
			throw new Error(`unknown message parameter id: ${id.toString()}`);
	}
}

/** Message parameters used in control messages. */
export class Parameters {
	vars: Map<bigint, bigint>;
	bytes: Map<bigint, Uint8Array>;
	/** TRACK_NAMESPACE_PREFIX on a namespace update, decoded only. */
	namespacePrefix: Path.Valid | undefined;
	#locations: Map<bigint, MessageLocation>;
	/** LOCATION_FILTER, kept decoded because its framing depends on the draft. */
	#filter: Filter.Filter | undefined;
	/** Every instance of a parameter that may repeat, decoded only; we never send one. */
	#repeated: Map<bigint, Uint8Array[]>;
	/** Every AUTHORIZATION TOKEN, checked for repeats once the block decoded. */
	#tokens: Token[];

	constructor() {
		this.vars = new Map();
		this.bytes = new Map();
		this.#locations = new Map();
		this.#repeated = new Map();
		this.#tokens = [];
	}

	#repeat(id: bigint, value: Uint8Array) {
		// Every token decodes, so one this endpoint cannot use closes the session.
		if (id === MSG_PARAM_AUTHORIZATION_TOKEN) this.#tokens.push(tokenFromRequest(value));
		const values = this.#repeated.get(id) ?? [];
		values.push(value);
		this.#repeated.set(id, values);
	}

	/**
	 * Whether the message carried a Range Filter. We advertise no MAX_FILTER_RANGES, so a
	 * request with one is refused rather than served unfiltered.
	 */
	get rangeFilters(): boolean {
		return MSG_PARAM_RANGE_FILTERS.some((id) => this.#repeated.has(id));
	}

	/** Whether the message carried TRACK_PROPERTY_FILTER, which only a SUBSCRIBE_TRACKS may. */
	get trackPropertyFilter(): boolean {
		return this.#repeated.has(MSG_PARAM_TRACK_PROPERTY_FILTER);
	}

	// --- Numeric accessors ---

	get subscriberPriority(): number | undefined {
		const v = this.vars.get(MSG_PARAM_SUBSCRIBER_PRIORITY);
		return v !== undefined ? Number(v) : undefined;
	}

	set subscriberPriority(v: number) {
		this.vars.set(MSG_PARAM_SUBSCRIBER_PRIORITY, BigInt(v));
	}

	get groupOrder(): number | undefined {
		const v = this.vars.get(MSG_PARAM_GROUP_ORDER);
		return v !== undefined ? Number(v) : undefined;
	}

	set groupOrder(v: number) {
		this.vars.set(MSG_PARAM_GROUP_ORDER, BigInt(v));
	}

	get forward(): boolean | undefined {
		const v = this.vars.get(MSG_PARAM_FORWARD);
		return v !== undefined ? v !== 0n : undefined;
	}

	set forward(v: boolean) {
		this.vars.set(MSG_PARAM_FORWARD, v ? 1n : 0n);
	}

	get publisherPriority(): number | undefined {
		const v = this.vars.get(MSG_PARAM_PUBLISHER_PRIORITY);
		return v !== undefined ? Number(v) : undefined;
	}

	set publisherPriority(v: number) {
		this.vars.set(MSG_PARAM_PUBLISHER_PRIORITY, BigInt(v));
	}

	get expires(): bigint | undefined {
		return this.vars.get(MSG_PARAM_EXPIRES);
	}

	set expires(v: bigint) {
		this.vars.set(MSG_PARAM_EXPIRES, v);
	}

	get maxCacheDuration(): bigint | undefined {
		return this.vars.get(MSG_PARAM_MAX_CACHE_DURATION);
	}

	set maxCacheDuration(v: bigint) {
		this.vars.set(MSG_PARAM_MAX_CACHE_DURATION, v);
	}

	/** Subscriber max age in milliseconds (MoQ Timestamp). */
	get maxAge(): number | undefined {
		const value = this.vars.get(MSG_PARAM_MAX_AGE);
		if (value === undefined) return undefined;
		if (value > BigInt(Number.MAX_SAFE_INTEGER)) throw new RangeError("max age exceeds safe milliseconds");
		return Number(value);
	}
	set maxAge(value: number | undefined) {
		if (value === undefined) this.vars.delete(MSG_PARAM_MAX_AGE);
		else {
			if (!Number.isSafeInteger(value) || value < 0) throw new RangeError("invalid max age");
			this.vars.set(MSG_PARAM_MAX_AGE, BigInt(value));
		}
	}

	/** HIDDEN (MoQ Hidden): also advertise hidden namespaces. Absent and 0 both mean no. */
	get hidden(): boolean {
		const v = this.vars.get(MSG_PARAM_HIDDEN);
		if (v === undefined || v === 0n) return false;
		if (v === 1n) return true;
		throw new Error(`invalid HIDDEN parameter: ${v}`);
	}

	set hidden(v: boolean) {
		if (v) this.vars.set(MSG_PARAM_HIDDEN, 1n);
		else this.vars.delete(MSG_PARAM_HIDDEN);
	}

	/** How many NAMESPACE messages follow a REQUEST_OK before the subscription is synced (MoQ Active Count). */
	get activeCount(): number | undefined {
		const v = this.vars.get(MSG_PARAM_ACTIVE_COUNT);
		if (v === undefined) return undefined;
		if (v > BigInt(Number.MAX_SAFE_INTEGER)) throw new ProtocolViolation(`ACTIVE_COUNT too large: ${v}`);
		return Number(v);
	}

	set activeCount(v: number | undefined) {
		if (v === undefined) this.vars.delete(MSG_PARAM_ACTIVE_COUNT);
		else this.vars.set(MSG_PARAM_ACTIVE_COUNT, BigInt(v));
	}

	// --- Bytes accessors ---

	get largest(): MessageLocation | undefined {
		const location = this.#locations.get(MSG_PARAM_LARGEST_OBJECT);
		return location && { ...location };
	}

	set largest(v: MessageLocation) {
		this.#locations.set(MSG_PARAM_LARGEST_OBJECT, { ...v });
	}

	/** LOCATION_FILTER: which Objects the subscription delivers. See `filter.ts`. */
	get subscriptionFilter(): Filter.Filter | undefined {
		return this.#filter;
	}

	set subscriptionFilter(v: Filter.Filter) {
		this.#filter = v;
	}

	/** FILL_PARAMETERS: the draft-22 backfill request, as its raw parameter value. */
	get fillParameters(): Uint8Array | undefined {
		return this.bytes.get(MSG_PARAM_FILL_PARAMETERS);
	}

	set fillParameters(v: Uint8Array) {
		this.bytes.set(MSG_PARAM_FILL_PARAMETERS, v);
	}

	/** INCLUDE_PROPERTIES: whether the peer wants Track Properties on the response. */
	get includeProperties(): boolean | undefined {
		const v = this.vars.get(MSG_PARAM_INCLUDE_PROPERTIES);
		if (v === undefined) return undefined;
		// The draft allows exactly 0 or 1; anything else is a protocol violation.
		if (v > 1n) throw new Error(`invalid INCLUDE_PROPERTIES value: ${v}`);
		return v === 1n;
	}

	set includeProperties(v: boolean) {
		this.vars.set(MSG_PARAM_INCLUDE_PROPERTIES, v ? 1n : 0n);
	}

	/**
	 * FILL_TIMEOUT: the total wait in milliseconds for objects not yet available. A value past
	 * what a number holds exactly saturates, since no wait lasts that long anyway.
	 */
	get fillTimeout(): Milli | undefined {
		const v = this.vars.get(MSG_PARAM_FILL_TIMEOUT);
		if (v === undefined) return undefined;
		return Milli(v > BigInt(Number.MAX_SAFE_INTEGER) ? Number.MAX_SAFE_INTEGER : Number(v));
	}

	set fillTimeout(v: Milli | undefined) {
		if (v === undefined) this.vars.delete(MSG_PARAM_FILL_TIMEOUT);
		else this.vars.set(MSG_PARAM_FILL_TIMEOUT, BigInt(v));
	}

	/** HOP_PATH: the hop chain an advertisement traversed, as its raw parameter value. */
	get hopPath(): Uint8Array | undefined {
		return this.bytes.get(MSG_PARAM_HOP_PATH);
	}

	set hopPath(v: Uint8Array) {
		this.bytes.set(MSG_PARAM_HOP_PATH, v);
	}

	/** ROUTE_COST: the accumulated cost of that path. Absent means 0. */
	get routeCost(): bigint | undefined {
		return this.vars.get(MSG_PARAM_ROUTE_COST);
	}

	set routeCost(v: bigint) {
		this.vars.set(MSG_PARAM_ROUTE_COST, v);
	}

	/** EPOCH (MoQ Epoch): the publisher instance an advertisement or request names. */
	get epoch(): Epoch.Valid | undefined {
		const value = this.bytes.get(MSG_PARAM_EPOCH);
		return value === undefined ? undefined : Epoch.fromBytes(value);
	}

	set epoch(v: Epoch.Valid | undefined) {
		if (v === undefined) this.bytes.delete(MSG_PARAM_EPOCH);
		else this.bytes.set(MSG_PARAM_EPOCH, Epoch.toBytes(v));
	}

	/** Encode the block. */
	async encode(w: Writer, version: IetfVersion) {
		const filter = this.#filter;
		await w.u53(this.vars.size + this.bytes.size + this.#locations.size + (filter ? 1 : 0));

		// Merge all parameter storage and sort by key, for delta encoding.
		const keys = [...this.vars.keys(), ...this.bytes.keys(), ...this.#locations.keys()];
		if (filter) keys.push(MSG_PARAM_SUBSCRIPTION_FILTER);
		keys.sort((a, b) => (a < b ? -1 : a > b ? 1 : 0));

		let prevId = 0n;
		for (let i = 0; i < keys.length; i++) {
			const key = keys[i];
			const delta = i === 0 ? key : key - prevId;
			prevId = key;
			await w.u62(delta);

			switch (getMessageParamKind(key)) {
				case "varint": {
					const value = this.vars.get(key);
					if (value === undefined) throw new Error(`invalid varint message parameter: ${key.toString()}`);
					await w.u62(value);
					break;
				}
				case "uint8": {
					const value = this.vars.get(key);
					if (value === undefined || value < 0n || value > 0xffn) {
						throw new Error(`invalid uint8 message parameter: ${key.toString()}`);
					}
					await w.u8(Number(value));
					break;
				}
				case "bool": {
					const value = this.vars.get(key);
					if (value !== 0n && value !== 1n) {
						throw new Error(`invalid bool message parameter: ${key.toString()}`);
					}
					await w.bool(value === 1n);
					break;
				}
				case "location": {
					const location = this.#locations.get(key);
					if (location === undefined)
						throw new Error(`invalid Location message parameter: ${key.toString()}`);
					// Section 10.2 has no Length on a Message Parameter and defines a Location
					// value as "Two consecutive varints (Group, Object)", so they go out bare.
					await w.u62(location.groupId);
					await w.u62(location.objectId);
					break;
				}
				case "filter": {
					if (!filter) throw new Error(`invalid LOCATION_FILTER message parameter: ${key.toString()}`);
					await w.write(Filter.encodeParam(filter, version));
					break;
				}
				case "bytes": {
					const value = this.bytes.get(key);
					if (value === undefined) throw new Error(`invalid bytes message parameter: ${key.toString()}`);
					await w.u53(value.length);
					await w.write(value);
					break;
				}
			}
		}
	}

	/**
	 * Decode a parameter block.
	 *
	 * `message`, when set, is the allow-list for that control message: any other id closes
	 * the session. Omitting `message` stores every id.
	 *
	 * Only a request may carry an AUTHORIZATION TOKEN, and every one decodes.
	 *
	 * @throws {MalformedAuthToken} once the block decoded, for tokens that repeat.
	 */
	static async decode(r: Reader, version: IetfVersion, message?: ControlMessage): Promise<Parameters> {
		const count = await r.u53();
		const params = new Parameters();

		let prevType = 0n;

		for (let i = 0; i < count; i++) {
			const delta = await r.u62();
			const id = i === 0 ? delta : prevType + delta;
			prevType = id;

			// A parameter value has no length, so an unlisted id cannot be skipped.
			if (message !== undefined && !allows(message, version, id)) {
				throw new ProtocolViolation(`message parameter ${id} is not defined for ${message}`);
			}

			if (MSG_PARAM_REPEATABLE.includes(id)) {
				const size = await r.u53();
				params.#repeat(id, await r.read(size));
				continue;
			}

			const filter = id === MSG_PARAM_SUBSCRIPTION_FILTER && params.#filter !== undefined;
			if (
				params.vars.has(id) ||
				params.bytes.has(id) ||
				params.#locations.has(id) ||
				filter ||
				(id === 0x34n && params.namespacePrefix !== undefined)
			) {
				throw new Error(`duplicate message parameter id: ${id.toString()}`);
			}

			switch (getMessageParamKind(id)) {
				case "varint":
					params.vars.set(id, await r.u62());
					break;
				case "uint8": {
					const value = await r.u8();
					if (id === MSG_PARAM_GROUP_ORDER && value !== 1 && value !== 2) {
						throw new ProtocolViolation(`invalid group order: ${value}`);
					}
					params.vars.set(id, BigInt(value));
					break;
				}
				case "bool": {
					const value = await r.u8();
					if (value !== 0 && value !== 1) {
						throw new ProtocolViolation(`invalid message parameter value: ${id}`);
					}
					params.vars.set(id, BigInt(value));
					break;
				}
				case "location": {
					// Two bare varints; see the matching comment in encode.
					const groupId = await r.u62();
					const objectId = await r.u62();
					params.#locations.set(id, { groupId, objectId });
					break;
				}
				case "namespace":
					params.namespacePrefix = await Namespace.decode(r);
					break;
				case "filter":
					params.#filter = await Filter.decodeParam(r, version);
					break;
				case "bytes": {
					const size = await r.u53();
					const value = await r.read(size);
					// Anything but a UUIDv7's 16 bytes is malformed, so refuse it here rather than on first read.
					if (id === MSG_PARAM_EPOCH) {
						try {
							Epoch.fromBytes(value);
						} catch (err) {
							throw new ProtocolViolation(`malformed EPOCH: ${reason(err)}`, { cause: err });
						}
					}
					params.bytes.set(id, value);
					break;
				}
			}
		}

		unique(params.#tokens);
		return params;
	}
}
