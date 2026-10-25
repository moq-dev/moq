import { ProtocolViolation } from "../error.ts";
import type { Reader, Writer } from "../stream.ts";
import { decodeUtf8 } from "../util/utf8.ts";
import { AUTHORIZATION_TOKEN } from "./token.ts";
import { type IetfVersion, Version } from "./version.ts";

/// Setup Option key constants (separate namespace from Message Parameters).
export const SetupOption = {
	Path: 1n,
	MaxRequestId: 2n,
	AuthorizationToken: AUTHORIZATION_TOKEN,
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
	/** Subscriber max age, from the MoQ Timestamp extension. */
	MaxAge: 0x915c4n,
	/** AUTH, from the MoQ Auth extension. See `auth.ts`. */
	Auth: 0x40b70n,
	/** ACTIVE_COUNT, from the MoQ Active Count extension. See `active_count.ts`. */
	ActiveCount: 0x40b64n,
	/** EPOCH, from the MoQ Epoch extension. See `epoch.ts`. */
	Epoch: 0x40b68n,
} as const;

// Unknown SETUP options may repeat, including GREASE (draft-21 section 9.1).
const KNOWN_SETUP_OPTIONS: readonly bigint[] = Object.values(SetupOption);

/**
 * Requests a draft-14 to -16 peer may hold open at once.
 *
 * Sized like moq-net's window: twice its default 100,000 announces and 20,000
 * other requests. Draft-17 dropped MAX_REQUEST_ID. Grants are batched per
 * half window, so only about half of it is guaranteed to a peer holding requests open.
 */
export const REQUEST_WINDOW = 240_000n;

/**
 * The SETUP MAX_REQUEST_ID that first admits `window` requests from the peer.
 *
 * It is exclusive (the draft's "maximum request ID plus 1"), and the peer uses
 * only its own parity: even IDs are the client's, odd the server's.
 */
export function initialMaxRequestId(peerClient: boolean, window: bigint = REQUEST_WINDOW): bigint {
	return window * 2n + (peerClient ? 0n : 1n);
}

/// Setup Options, used in SETUP messages.
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

/**
 * Advertise our implementation in the IMPLEMENTATION option, or nothing when `value` is empty.
 *
 * @internal
 */
export function implementationIntoSetup(params: SetupOptions, value: string) {
	if (value) params.setBytes(SetupOption.Implementation, new TextEncoder().encode(value));
}

/**
 * The implementation the peer advertised, if any. Malformed UTF-8 is a protocol violation.
 *
 * @internal
 */
export function implementationFromSetup(params: SetupOptions): string | undefined {
	const bytes = params.getBytes(SetupOption.Implementation);
	if (bytes === undefined) return undefined;
	try {
		return decodeUtf8(bytes);
	} catch (cause) {
		throw new ProtocolViolation("malformed IMPLEMENTATION", { cause });
	}
}
