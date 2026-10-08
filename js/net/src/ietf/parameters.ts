import { ProtocolViolation } from "../error.ts";
import type { Reader, Writer } from "../stream.ts";
import * as Varint from "../varint.ts";
import { type IetfVersion, Version } from "./version.ts";

/// Setup Option key constants (separate namespace from Message Parameters).
export const SetupOption = {
	Path: 1n,
	MaxRequestId: 2n,
	AuthorizationToken: 3n,
	MaxAuthTokenCacheSize: 4n,
	Authority: 5n,
	Implementation: 7n,
	/** HOP_ID, from the MoQ Cluster extension. See `cluster.ts`. */
	HopId: 0x40b54n,
	/** RELAY_COST, from the MoQ Cluster extension. See `cluster.ts`. */
	RelayCost: 0x40b56n,
	/** SOLICIT, from the MoQ Solicit extension. See `solicit.ts`. */
	Solicit: 0x40b5an,
	/** HIDDEN, from the MoQ Hidden extension. See `hidden.ts`. */
	Hidden: 0x40b5cn,
} as const;

// Unknown SETUP options may repeat, including GREASE (draft-21 section 9.1).
const KNOWN_SETUP_OPTIONS: readonly bigint[] = Object.values(SetupOption);

/// Setup Options — used in SETUP messages.
///
/// In d14-d16 these are count-prefixed ("Setup Parameters").
/// In d17 these have no count prefix ("Setup Options") and read/write to end of message.
export class SetupOptions {
	vars: Map<bigint, bigint>;
	bytes: Map<bigint, Uint8Array>;

	constructor() {
		this.vars = new Map();
		this.bytes = new Map();
	}

	get size() {
		return this.vars.size + this.bytes.size;
	}

	setBytes(id: bigint, value: Uint8Array) {
		if (id % 2n !== 1n) {
			throw new Error(`invalid parameter id: ${id.toString()}, must be odd`);
		}
		this.bytes.set(id, value);
	}

	setVarint(id: bigint, value: bigint) {
		if (id % 2n !== 0n) {
			throw new Error(`invalid parameter id: ${id.toString()}, must be even`);
		}
		this.vars.set(id, value);
	}

	getBytes(id: bigint): Uint8Array | undefined {
		if (id % 2n !== 1n) {
			throw new Error(`invalid parameter id: ${id.toString()}, must be odd`);
		}
		return this.bytes.get(id);
	}

	getVarint(id: bigint): bigint | undefined {
		if (id % 2n !== 0n) {
			throw new Error(`invalid parameter id: ${id.toString()}, must be even`);
		}
		return this.vars.get(id);
	}

	removeBytes(id: bigint): boolean {
		if (id % 2n !== 1n) {
			throw new Error(`invalid parameter id: ${id.toString()}, must be odd`);
		}
		return this.bytes.delete(id);
	}

	removeVarint(id: bigint): boolean {
		if (id % 2n !== 0n) {
			throw new Error(`invalid parameter id: ${id.toString()}, must be even`);
		}
		return this.vars.delete(id);
	}

	async encode(w: Writer, version: IetfVersion) {
		if (version !== Version.DRAFT_14 && version !== Version.DRAFT_15) {
			// d17+: no count prefix; d16: count prefix
			if (version === Version.DRAFT_16) {
				await w.u53(this.vars.size + this.bytes.size);
			}

			// Delta encoding: collect all keys, sort, encode deltas
			const all: { key: bigint; isVar: boolean }[] = [];
			for (const id of this.vars.keys()) all.push({ key: id, isVar: true });
			for (const id of this.bytes.keys()) all.push({ key: id, isVar: false });
			all.sort((a, b) => (a.key < b.key ? -1 : a.key > b.key ? 1 : 0));

			let prevId = 0n;
			for (let i = 0; i < all.length; i++) {
				const { key, isVar } = all[i];
				const delta = i === 0 ? key : key - prevId;
				prevId = key;
				await w.u62(delta);

				if (isVar) {
					// biome-ignore lint/style/noNonNullAssertion: key is guaranteed to exist in vars map
					await w.u62(this.vars.get(key)!);
				} else {
					// biome-ignore lint/style/noNonNullAssertion: key is guaranteed to exist in bytes map
					const value = this.bytes.get(key)!;
					await w.u53(value.length);
					await w.write(value);
				}
			}
		} else {
			await w.u53(this.vars.size + this.bytes.size);

			for (const [id, value] of this.vars) {
				await w.u62(id);
				await w.u62(value);
			}

			for (const [id, value] of this.bytes) {
				await w.u62(id);
				await w.u53(value.length);
				await w.write(value);
			}
		}
	}

	static async decode(r: Reader, version: IetfVersion): Promise<SetupOptions> {
		const params = new SetupOptions();

		if (version !== Version.DRAFT_14 && version !== Version.DRAFT_15 && version !== Version.DRAFT_16) {
			// d17+: no count prefix, read until reader is done
			let prevType = 0n;
			let i = 0;
			while (!(await r.done())) {
				const delta = await r.u62();
				const id = i === 0 ? delta : prevType + delta;
				prevType = id;
				i++;

				if (id % 2n === 0n) {
					if (KNOWN_SETUP_OPTIONS.includes(id) && params.vars.has(id)) {
						throw new Error(`duplicate parameter id: ${id.toString()}`);
					}
					const varint = await r.u62();
					params.setVarint(id, varint);
				} else {
					if (KNOWN_SETUP_OPTIONS.includes(id) && params.bytes.has(id)) {
						throw new Error(`duplicate parameter id: ${id.toString()}`);
					}
					const size = await r.u53();
					const bytes = await r.read(size);
					params.setBytes(id, bytes);
				}
			}
		} else {
			const count = await r.u53();
			let prevType = 0n;

			for (let i = 0; i < count; i++) {
				let id: bigint;
				if (version === Version.DRAFT_16) {
					const delta = await r.u62();
					id = i === 0 ? delta : prevType + delta;
					prevType = id;
				} else {
					id = await r.u62();
				}

				if (id % 2n === 0n) {
					if (KNOWN_SETUP_OPTIONS.includes(id) && params.vars.has(id)) {
						throw new Error(`duplicate parameter id: ${id.toString()}`);
					}
					const varint = await r.u62();
					params.setVarint(id, varint);
				} else {
					if (KNOWN_SETUP_OPTIONS.includes(id) && params.bytes.has(id)) {
						throw new Error(`duplicate parameter id: ${id.toString()}`);
					}
					const size = await r.u53();
					const bytes = await r.read(size);
					params.setBytes(id, bytes);
				}
			}
		}

		return params;
	}
}

// ---- Message Parameters (used in Subscribe, Publish, Fetch, etc.) ----
// Count-prefixed KVPs through d16, then definition-specific Type-Value pairs.
// Parameter types are delta-encoded from d16 onward.

// Varint parameter IDs (even)
const MSG_PARAM_DELIVERY_TIMEOUT = 0x02n;
/// FILL_TIMEOUT, on a FETCH. Ignored: a refusal doesn't wait.
const MSG_PARAM_FILL_TIMEOUT = 0x0an;
/// NEW_GROUP_REQUEST. Ignored, as the draft lets a publisher without dynamic groups do.
const MSG_PARAM_NEW_GROUP_REQUEST = 0x32n;
/// SUBGROUP_DELIVERY_TIMEOUT, alongside the per-object one above.
const MSG_PARAM_SUBGROUP_DELIVERY_TIMEOUT = 0x06n;
const MSG_PARAM_MAX_CACHE_DURATION = 0x04n;
const MSG_PARAM_EXPIRES = 0x08n;
const MSG_PARAM_PUBLISHER_PRIORITY = 0x0en;
const MSG_PARAM_FORWARD = 0x10n;
const MSG_PARAM_SUBSCRIBER_PRIORITY = 0x20n;
const MSG_PARAM_GROUP_ORDER = 0x22n;
/// INCLUDE_PROPERTIES, draft-20's opt-out from Track Properties.
const MSG_PARAM_INCLUDE_PROPERTIES = 0x35n;
/// ROUTE_COST, from the MoQ Cluster extension. See `cluster.ts`.
const MSG_PARAM_ROUTE_COST = 0x40b58n;
/// HIDDEN, from the MoQ Hidden extension. See `hidden.ts`.
const MSG_PARAM_HIDDEN = 0x40b5en;

// Bytes parameter IDs (odd)
/// AUTHORIZATION TOKEN. Ignored: the session's grant is what authorizes a request.
const MSG_PARAM_AUTHORIZATION_TOKEN = 0x03n;
const MSG_PARAM_LARGEST_OBJECT = 0x09n;
const MSG_PARAM_SUBSCRIPTION_FILTER = 0x21n;
/// FILL_PARAMETERS, draft-20's request for a backfill.
const MSG_PARAM_FILL_PARAMETERS = 0x23n;
/// HOP_PATH, from the MoQ Cluster extension. See `cluster.ts`.
const MSG_PARAM_HOP_PATH = 0x40b57n;

/// The object Range Filters (draft-19): SUBGROUP, OBJECTID, PRIORITY and OBJECT_PROPERTY.
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

type MessageParamKind = "varint" | "uint8" | "bool" | "location" | "bytes";
/** A `{Group, Object}` pair carried by a message parameter, such as LARGEST_OBJECT. */
export type MessageLocation = { groupId: bigint; objectId: bigint };

function getMessageParamKind(id: bigint): MessageParamKind {
	switch (id) {
		case MSG_PARAM_DELIVERY_TIMEOUT:
		case MSG_PARAM_FILL_TIMEOUT:
		case MSG_PARAM_NEW_GROUP_REQUEST:
		case MSG_PARAM_SUBGROUP_DELIVERY_TIMEOUT:
		case MSG_PARAM_MAX_CACHE_DURATION:
		case MSG_PARAM_EXPIRES:
		case MSG_PARAM_ROUTE_COST:
		case MSG_PARAM_HIDDEN:
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
		case MSG_PARAM_AUTHORIZATION_TOKEN:
		case MSG_PARAM_SUBSCRIPTION_FILTER:
		case MSG_PARAM_FILL_PARAMETERS:
		case MSG_PARAM_HOP_PATH:
			return "bytes";
		default:
			if (MSG_PARAM_REPEATABLE.includes(id)) return "bytes";
			throw new Error(`unknown message parameter id: ${id.toString()}`);
	}
}

/**
 * The draft-16 and earlier form: draft-16 section 9.2 serializes every Message Parameter as
 * a Key-Value-Pair, and section 9.2.2.7 calls LARGEST_OBJECT "a length-prefixed Location
 * structure", so the two QUIC-style varints sit inside a byte string.
 */
function decodeLocation(data: Uint8Array): MessageLocation {
	const [groupId, objectData] = Varint.decodeBigInt(data);
	const [objectId, trailing] = Varint.decodeBigInt(objectData);
	if (trailing.length !== 0) {
		throw new Error("trailing bytes in message parameter Location");
	}
	return { groupId, objectId };
}

function encodeLocation({ groupId, objectId }: MessageLocation): Uint8Array {
	const group = Varint.encode(groupId);
	const object = Varint.encode(objectId);
	const combined = new Uint8Array(group.length + object.length);
	combined.set(group, 0);
	combined.set(object, group.length);
	return combined;
}

/** Message parameters used in control messages. */
export class Parameters {
	vars: Map<bigint, bigint>;
	bytes: Map<bigint, Uint8Array>;
	#locations: Map<bigint, MessageLocation>;
	/** Every instance of a parameter that may repeat, decoded only; we never send one. */
	#repeated: Map<bigint, Uint8Array[]>;

	constructor() {
		this.vars = new Map();
		this.bytes = new Map();
		this.#locations = new Map();
		this.#repeated = new Map();
	}

	#repeat(id: bigint, value: Uint8Array) {
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

	get deliveryTimeout(): bigint | undefined {
		return this.vars.get(MSG_PARAM_DELIVERY_TIMEOUT);
	}

	set deliveryTimeout(v: bigint) {
		this.vars.set(MSG_PARAM_DELIVERY_TIMEOUT, v);
	}

	get maxCacheDuration(): bigint | undefined {
		return this.vars.get(MSG_PARAM_MAX_CACHE_DURATION);
	}

	set maxCacheDuration(v: bigint) {
		this.vars.set(MSG_PARAM_MAX_CACHE_DURATION, v);
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

	// --- Bytes accessors ---

	get largest(): MessageLocation | undefined {
		const location = this.#locations.get(MSG_PARAM_LARGEST_OBJECT);
		return location && { ...location };
	}

	set largest(v: MessageLocation) {
		this.#locations.set(MSG_PARAM_LARGEST_OBJECT, { ...v });
	}

	/**
	 * LOCATION_FILTER, as its raw parameter value.
	 *
	 * Raw because the encoding is version dependent: a Filter Type tag through draft-19, a
	 * list of up to four varints from draft-20 on. See `filter.ts`.
	 */
	get subscriptionFilter(): Uint8Array | undefined {
		return this.bytes.get(MSG_PARAM_SUBSCRIPTION_FILTER);
	}

	set subscriptionFilter(v: Uint8Array) {
		this.bytes.set(MSG_PARAM_SUBSCRIPTION_FILTER, v);
	}

	/** FILL_PARAMETERS: the draft-20 backfill request, as its raw parameter value. */
	get fillParameters(): Uint8Array | undefined {
		return this.bytes.get(MSG_PARAM_FILL_PARAMETERS);
	}

	set fillParameters(v: Uint8Array) {
		this.bytes.set(MSG_PARAM_FILL_PARAMETERS, v);
	}

	/** INCLUDE_PROPERTIES: whether the peer wants Track Properties on the response. */
	get includeProperties(): boolean | undefined {
		// Draft-16 and earlier frame every parameter as a Key-Value-Pair, so an odd id lands
		// in `bytes`. It still has to read as present, so the draft-20 gate can refuse it.
		const legacy = this.bytes.get(MSG_PARAM_INCLUDE_PROPERTIES);
		const v = legacy ? (legacy.length === 1 ? BigInt(legacy[0]) : 2n) : this.vars.get(MSG_PARAM_INCLUDE_PROPERTIES);
		if (v === undefined) return undefined;
		// The draft allows exactly 0 or 1; anything else is a protocol violation.
		if (v > 1n) throw new Error(`invalid INCLUDE_PROPERTIES value: ${v}`);
		return v === 1n;
	}

	set includeProperties(v: boolean) {
		this.vars.set(MSG_PARAM_INCLUDE_PROPERTIES, v ? 1n : 0n);
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

	async encode(w: Writer, version: IetfVersion) {
		await w.u53(this.vars.size + this.bytes.size + this.#locations.size);

		if (version === Version.DRAFT_14 || version === Version.DRAFT_15) {
			for (const [id, value] of this.vars) {
				await w.u62(id);
				await w.u62(value);
			}

			for (const [id, value] of this.bytes) {
				await w.u62(id);
				await w.u53(value.length);
				await w.write(value);
			}

			for (const [id, value] of this.#locations) {
				const encoded = encodeLocation(value);
				await w.u62(id);
				await w.u53(encoded.length);
				await w.write(encoded);
			}
		} else {
			// d16+: Delta encoding, merge all parameter storage, sort by key
			const all: { key: bigint; storage: "var" | "bytes" | "location" }[] = [];
			for (const id of this.vars.keys()) all.push({ key: id, storage: "var" });
			for (const id of this.bytes.keys()) all.push({ key: id, storage: "bytes" });
			for (const id of this.#locations.keys()) all.push({ key: id, storage: "location" });
			all.sort((a, b) => (a.key < b.key ? -1 : a.key > b.key ? 1 : 0));

			let prevId = 0n;
			for (let i = 0; i < all.length; i++) {
				const { key, storage } = all[i];
				const delta = i === 0 ? key : key - prevId;
				prevId = key;
				await w.u62(delta);

				if (version === Version.DRAFT_16) {
					if (storage === "var") {
						// biome-ignore lint/style/noNonNullAssertion: key is guaranteed to exist in vars map
						await w.u62(this.vars.get(key)!);
					} else {
						const value =
							storage === "bytes"
								? // biome-ignore lint/style/noNonNullAssertion: key is guaranteed to exist in bytes map
									this.bytes.get(key)!
								: // biome-ignore lint/style/noNonNullAssertion: key is guaranteed to exist in locations map
									encodeLocation(this.#locations.get(key)!);
						await w.u53(value.length);
						await w.write(value);
					}
					continue;
				}

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
						// Draft-17 section 9.3, and section 10.2 from draft-18 on, drops the
						// Length from a Message Parameter and defines a Location value as
						// "Two consecutive varints (Group, Object)", so they go out bare.
						await w.u62(location.groupId);
						await w.u62(location.objectId);
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
	}

	static async decode(r: Reader, version: IetfVersion): Promise<Parameters> {
		const count = await r.u53();
		const params = new Parameters();

		let prevType = 0n;

		for (let i = 0; i < count; i++) {
			let id: bigint;
			if (version === Version.DRAFT_14 || version === Version.DRAFT_15) {
				id = await r.u62();
			} else {
				// d16+: delta encoding
				const delta = await r.u62();
				id = i === 0 ? delta : prevType + delta;
				prevType = id;
			}

			if (version === Version.DRAFT_14 || version === Version.DRAFT_15 || version === Version.DRAFT_16) {
				if (id % 2n === 0n) {
					if (params.vars.has(id)) {
						throw new Error(`duplicate message parameter id: ${id.toString()}`);
					}
					const varint = await r.u62();
					params.vars.set(id, varint);
				} else {
					const size = await r.u53();
					const bytes = await r.read(size);
					if (MSG_PARAM_REPEATABLE.includes(id)) {
						params.#repeat(id, bytes);
					} else if (id === MSG_PARAM_LARGEST_OBJECT) {
						if (params.#locations.has(id)) {
							throw new Error(`duplicate message parameter id: ${id.toString()}`);
						}
						params.#locations.set(id, decodeLocation(bytes));
					} else {
						if (params.bytes.has(id)) {
							throw new Error(`duplicate message parameter id: ${id.toString()}`);
						}
						params.bytes.set(id, bytes);
					}
				}
				continue;
			}

			if (MSG_PARAM_REPEATABLE.includes(id)) {
				const size = await r.u53();
				params.#repeat(id, await r.read(size));
				continue;
			}

			if (params.vars.has(id) || params.bytes.has(id) || params.#locations.has(id)) {
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
				case "bool":
					params.vars.set(id, (await r.bool()) ? 1n : 0n);
					break;
				case "location": {
					// Two bare varints from draft-17 on; see the matching comment in encode.
					const groupId = await r.u62();
					const objectId = await r.u62();
					params.#locations.set(id, { groupId, objectId });
					break;
				}
				case "bytes": {
					const size = await r.u53();
					params.bytes.set(id, await r.read(size));
					break;
				}
			}
		}

		return params;
	}
}
