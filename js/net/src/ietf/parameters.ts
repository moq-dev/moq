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
	/** RELAY_HOPS, from the MoQ Cluster extension. See `cluster.ts`. */
	RelayHops: 0x40b55n,
	/** RELAY_COST, from the MoQ Cluster extension. See `cluster.ts`. */
	RelayCost: 0x40b56n,
	/** SOLICIT, from the MoQ Solicit extension. See `solicit.ts`. */
	Solicit: 0x40b5an,
} as const;

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
					if (params.vars.has(id)) {
						throw new Error(`duplicate parameter id: ${id.toString()}`);
					}
					const varint = await r.u62();
					params.setVarint(id, varint);
				} else {
					if (params.bytes.has(id)) {
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
					if (params.vars.has(id)) {
						throw new Error(`duplicate parameter id: ${id.toString()}`);
					}
					const varint = await r.u62();
					params.setVarint(id, varint);
				} else {
					if (params.bytes.has(id)) {
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
const MSG_PARAM_MAX_CACHE_DURATION = 0x04n;
const MSG_PARAM_EXPIRES = 0x08n;
const MSG_PARAM_PUBLISHER_PRIORITY = 0x0en;
const MSG_PARAM_FORWARD = 0x10n;
const MSG_PARAM_SUBSCRIBER_PRIORITY = 0x20n;
const MSG_PARAM_GROUP_ORDER = 0x22n;
/// ROUTE_COST, from the MoQ Cluster extension. See `cluster.ts`.
const MSG_PARAM_ROUTE_COST = 0x40b58n;

// Bytes parameter IDs (odd)
const MSG_PARAM_LARGEST_OBJECT = 0x09n;
const MSG_PARAM_SUBSCRIPTION_FILTER = 0x21n;
/// HOP_PATH, from the MoQ Cluster extension. See `cluster.ts`.
const MSG_PARAM_HOP_PATH = 0x40b57n;

type MessageParamKind = "varint" | "uint8" | "bool" | "location" | "bytes";
type MessageLocation = { groupId: bigint; objectId: bigint };

function getMessageParamKind(id: bigint): MessageParamKind {
	switch (id) {
		case MSG_PARAM_DELIVERY_TIMEOUT:
		case MSG_PARAM_MAX_CACHE_DURATION:
		case MSG_PARAM_EXPIRES:
		case MSG_PARAM_ROUTE_COST:
			return "varint";
		case MSG_PARAM_PUBLISHER_PRIORITY:
		case MSG_PARAM_SUBSCRIBER_PRIORITY:
		case MSG_PARAM_GROUP_ORDER:
			return "uint8";
		case MSG_PARAM_FORWARD:
			return "bool";
		case MSG_PARAM_LARGEST_OBJECT:
			return "location";
		case MSG_PARAM_SUBSCRIPTION_FILTER:
		case MSG_PARAM_HOP_PATH:
			return "bytes";
		default:
			throw new Error(`unknown message parameter id: ${id.toString()}`);
	}
}

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

	constructor() {
		this.vars = new Map();
		this.bytes = new Map();
		this.#locations = new Map();
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

	// --- Bytes accessors ---

	get largest(): MessageLocation | undefined {
		const location = this.#locations.get(MSG_PARAM_LARGEST_OBJECT);
		return location && { ...location };
	}

	set largest(v: MessageLocation) {
		this.#locations.set(MSG_PARAM_LARGEST_OBJECT, { ...v });
	}

	get subscriptionFilter(): number | undefined {
		const data = this.bytes.get(MSG_PARAM_SUBSCRIPTION_FILTER);
		if (!data || data.length === 0) return undefined;
		// Filter type is a varint — for our purposes, the first byte suffices
		return data[0];
	}

	set subscriptionFilter(v: number) {
		this.bytes.set(MSG_PARAM_SUBSCRIPTION_FILTER, new Uint8Array([v]));
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
					if (id === MSG_PARAM_LARGEST_OBJECT) {
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

			if (params.vars.has(id) || params.bytes.has(id) || params.#locations.has(id)) {
				throw new Error(`duplicate message parameter id: ${id.toString()}`);
			}

			switch (getMessageParamKind(id)) {
				case "varint":
					params.vars.set(id, await r.u62());
					break;
				case "uint8":
					params.vars.set(id, BigInt(await r.u8()));
					break;
				case "bool":
					params.vars.set(id, (await r.bool()) ? 1n : 0n);
					break;
				case "location": {
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
