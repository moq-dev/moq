import type { Reader, Writer } from "../stream.ts";
import * as Varint from "../varint.ts";
import { type IetfVersion, Version } from "./version.ts";

export const Parameter = {
	MaxRequestId: 2n,
	Implementation: 7n,
} as const;

export class Parameters {
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
		await w.u53(this.vars.size + this.bytes.size);

		if (version === Version.DRAFT_16) {
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

	static async decode(r: Reader, version: IetfVersion): Promise<Parameters> {
		const count = await r.u53();
		const params = new Parameters();

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

		return params;
	}
}

// ---- Message Parameters (used in Subscribe, Publish, Fetch, etc.) ----
// Uses raw bigint keys since parameter IDs have different meanings from setup parameters.

// Varint parameter IDs (even)
const MSG_PARAM_DELIVERY_TIMEOUT = 0x02n;
const MSG_PARAM_MAX_CACHE_DURATION = 0x04n;
const MSG_PARAM_EXPIRES = 0x08n;
const MSG_PARAM_PUBLISHER_PRIORITY = 0x0en;
const MSG_PARAM_FORWARD = 0x10n;
const MSG_PARAM_SUBSCRIBER_PRIORITY = 0x20n;
const MSG_PARAM_GROUP_ORDER = 0x22n;

// Bytes parameter IDs (odd)
const MSG_PARAM_LARGEST_OBJECT = 0x09n;
const MSG_PARAM_SUBSCRIPTION_FILTER = 0x21n;

export class MessageParameters {
	vars: Map<bigint, bigint>;
	bytes: Map<bigint, Uint8Array>;

	constructor() {
		this.vars = new Map();
		this.bytes = new Map();
	}

	// --- Varint accessors ---

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

	get largest(): { groupId: bigint; objectId: bigint } | undefined {
		const data = this.bytes.get(MSG_PARAM_LARGEST_OBJECT);
		if (!data || data.length === 0) return undefined;
		const [groupId, rest] = Varint.decode(data);
		const [objectId] = Varint.decode(rest);
		return { groupId: BigInt(groupId), objectId: BigInt(objectId) };
	}

	set largest(v: { groupId: bigint; objectId: bigint }) {
		const buf1 = Varint.encode(Number(v.groupId));
		const buf2 = Varint.encode(Number(v.objectId));
		const combined = new Uint8Array(buf1.length + buf2.length);
		combined.set(buf1, 0);
		combined.set(buf2, buf1.length);
		this.bytes.set(MSG_PARAM_LARGEST_OBJECT, combined);
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

	async encode(w: Writer, version: IetfVersion) {
		await w.u53(this.vars.size + this.bytes.size);

		if (version === Version.DRAFT_16) {
			// Delta encoding: merge vars and bytes, sort by key
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

	static async decode(r: Reader, version: IetfVersion): Promise<MessageParameters> {
		const count = await r.u53();
		const params = new MessageParameters();

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
					throw new Error(`duplicate message parameter id: ${id.toString()}`);
				}
				const varint = await r.u62();
				params.vars.set(id, varint);
			} else {
				if (params.bytes.has(id)) {
					throw new Error(`duplicate message parameter id: ${id.toString()}`);
				}
				const size = await r.u53();
				const bytes = await r.read(size);
				params.bytes.set(id, bytes);
			}
		}

		return params;
	}
}
