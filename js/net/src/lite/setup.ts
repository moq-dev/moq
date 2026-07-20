/**
 * The lite-05 SETUP message: each endpoint advertises its capabilities once, as the
 * sole message on a unidirectional Setup Stream ({@link DataType.Setup}), then closes it.
 *
 * @module
 */

import type { Reader, Writer } from "../stream.ts";
import * as Varint from "../varint.ts";
import * as Message from "./message.ts";
import { hasSetupStream, type Version } from "./version.ts";

/** Setup Parameter id for the Probe capability level. */
const PARAM_PROBE = 0x1n;
/** Setup Parameter id for the request Path (client-only, URI-less transports). */
const PARAM_PATH = 0x2n;
/** Setup Parameter id for the client's intended {@link Role} (client-only). */
const PARAM_ROLE = 0x3n;

/** Cap on the number of parameters in a bag, matching the Rust decoder. */
const MAX_PARAMS = 64;

/**
 * The probe capability an endpoint advertises in SETUP.
 *
 * Monotonic: a higher level implies every lower one. An unknown (future) value
 * decodes as the highest level we understand, so a peer that gains a new level is
 * treated as at least {@link ProbeLevel.Increase}.
 */
export const ProbeLevel = {
	/** No probing. Equivalent to omitting the parameter. */
	None: 0,
	/** The publisher can measure and periodically report its estimated bitrate. */
	Report: 1,
	/** The publisher can additionally pad the connection (or send redundant data). */
	Increase: 2,
} as const;

/** A probe capability level. See {@link ProbeLevel}. */
export type ProbeLevel = (typeof ProbeLevel)[keyof typeof ProbeLevel];

/** Map a wire value to a level, saturating unknown values to {@link ProbeLevel.Increase}. */
function probeFromCode(code: bigint): ProbeLevel {
	switch (code) {
		case 0n:
			return ProbeLevel.None;
		case 1n:
			return ProbeLevel.Report;
		default:
			return ProbeLevel.Increase;
	}
}

/**
 * The direction a client intends to use the session for.
 *
 * A client advertises this in its SETUP so the server can reject a token that lacks the
 * matching scope during the handshake, instead of accepting a session that then silently
 * carries no media (a subscribe-only token used to publish, or vice versa). It only ever
 * narrows what the server grants, so it is not a security boundary: the server still
 * enforces the token's scope regardless.
 */
export const Role = {
	/** The client may do either, or declined to say. Equivalent to omitting the parameter. */
	Both: 0,
	/** The client will publish tracks (ingest); the server must consume. */
	Publisher: 1,
	/** The client will subscribe to tracks (egress); the server must publish. */
	Subscriber: 2,
} as const;

/** A session role. See {@link Role}. */
export type Role = (typeof Role)[keyof typeof Role];

/**
 * Map a wire value to a role, falling back to {@link Role.Both}.
 *
 * The draft requires a receiver that does not recognize the value to treat it as `Both`,
 * so a newer client can't break an older server: it just loses the early reject and defers
 * to the token's scope.
 */
function roleFromCode(code: bigint): Role {
	switch (code) {
		case 1n:
			return Role.Publisher;
		case 2n:
			return Role.Subscriber;
		default:
			return Role.Both;
	}
}

/**
 * A bag of `id -> raw bytes` parameters, the body shared by SETUP. Encoded as a varint
 * count followed by `id, length, value` triples; duplicate ids are rejected on decode.
 */
class Parameters {
	#entries = new Map<bigint, Uint8Array>();

	/** Set a parameter to a raw byte value, replacing any existing entry. */
	setBytes(id: bigint, value: Uint8Array) {
		this.#entries.set(id, value);
	}

	/** Return a parameter's raw byte value, if present. */
	getBytes(id: bigint): Uint8Array | undefined {
		return this.#entries.get(id);
	}

	/** Set a parameter to a varint value, replacing any existing entry. */
	setVarint(id: bigint, value: number | bigint) {
		this.#entries.set(id, Varint.encode(Number(value)));
	}

	/** Decode a parameter as a single varint, if present. Throws if trailing bytes remain. */
	getVarint(id: bigint): bigint | undefined {
		const bytes = this.#entries.get(id);
		if (bytes === undefined) return undefined;
		const [value, remain] = Varint.decode(bytes);
		if (remain.byteLength !== 0) {
			throw new Error("trailing bytes after varint parameter");
		}
		return BigInt(value);
	}

	async encode(w: Writer) {
		if (this.#entries.size > MAX_PARAMS) {
			throw new Error("too many parameters");
		}

		await w.u53(this.#entries.size);
		for (const [id, value] of this.#entries) {
			await w.u62(id);
			await w.u53(value.byteLength);
			await w.write(value);
		}
	}

	static async decode(r: Reader): Promise<Parameters> {
		const params = new Parameters();

		const count = await r.u53();
		if (count > MAX_PARAMS) {
			throw new Error("too many parameters");
		}

		for (let i = 0; i < count; i++) {
			const id = await r.u62();
			if (params.#entries.has(id)) {
				throw new Error(`duplicate parameter id: ${id.toString()}`);
			}
			const size = await r.u53();
			const value = await r.read(size);
			params.#entries.set(id, value);
		}

		return params;
	}
}

/** The capabilities a {@link Setup} advertises. Each defaults to the wire default, which is the absence of the parameter. */
export interface SetupProps {
	/** See {@link Setup.probe}. Defaults to {@link ProbeLevel.None}. */
	probe?: ProbeLevel;
	/** See {@link Setup.path}. Omitted by default. */
	path?: string;
	/** See {@link Setup.role}. Defaults to {@link Role.Both}. */
	role?: Role;
}

/**
 * The SETUP message, sent once per endpoint on the unidirectional Setup Stream.
 *
 * lite-05+ only. The two endpoints' SETUP messages are independent: neither side
 * blocks on the peer's before opening other streams, but a stream whose encoding
 * depends on a negotiated capability (e.g. PROBE) must wait for it.
 */
export class Setup {
	/** The probe capability this endpoint supports. {@link ProbeLevel.None} when absent. */
	probe: ProbeLevel;

	/**
	 * The request path, for transports that carry no request URI (native QUIC, qmux over
	 * TCP/TLS, unix sockets). Sent only by the client; a server never sends one and a relay
	 * never forwards it. `undefined` on URI-carrying bindings such as WebTransport, where
	 * sending one is a protocol violation. An empty string means the same as `undefined`.
	 */
	path?: string;

	/**
	 * The client's intended {@link Role}. `Both` is sent as the absence of the parameter, so
	 * a client that never sets it decodes back to `Both`. Sent only by the client; a server
	 * never sends one and a relay never forwards it.
	 */
	role: Role;

	constructor({ probe, path, role }: SetupProps = {}) {
		this.probe = probe ?? ProbeLevel.None;
		this.path = path;
		this.role = role ?? Role.Both;
	}

	static #guard(version: Version) {
		if (!hasSetupStream(version)) {
			throw new Error("setup stream not supported for this version");
		}
	}

	async #encode(w: Writer) {
		const params = new Parameters();
		// None is the wire default, so omit it to keep the message empty when nothing is set.
		if (this.probe !== ProbeLevel.None) {
			params.setVarint(PARAM_PROBE, this.probe);
		}
		if (this.path !== undefined) {
			params.setBytes(PARAM_PATH, new TextEncoder().encode(this.path));
		}
		// Both is the wire default, sent as the absence of the parameter.
		if (this.role !== Role.Both) {
			params.setVarint(PARAM_ROLE, this.role);
		}
		await params.encode(w);
	}

	static async #decode(r: Reader): Promise<Setup> {
		const params = await Parameters.decode(r);

		const probeCode = params.getVarint(PARAM_PROBE);
		const probe = probeCode === undefined ? ProbeLevel.None : probeFromCode(probeCode);

		// An empty path is valid and means the same as omitting the parameter, so a
		// client that wants the root doesn't have to special-case it.
		const pathBytes = params.getBytes(PARAM_PATH);
		const path = pathBytes === undefined ? undefined : new TextDecoder().decode(pathBytes);

		const roleCode = params.getVarint(PARAM_ROLE);
		const role = roleCode === undefined ? Role.Both : roleFromCode(roleCode);

		return new Setup({ probe, path, role });
	}

	/** Encode the SETUP message with its size prefix. Throws on pre-lite-05 versions. */
	async encode(w: Writer, version: Version): Promise<void> {
		Setup.#guard(version);
		return Message.encode(w, this.#encode.bind(this));
	}

	/** Decode a SETUP message with its size prefix. Throws on pre-lite-05 versions. */
	static async decode(r: Reader, version: Version): Promise<Setup> {
		Setup.#guard(version);
		return Message.decode(r, Setup.#decode);
	}
}
