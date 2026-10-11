import { Unsupported } from "../auth.ts";
import type { AuthWire, WireGrant, WireReply } from "../auth_session.ts";
import { SessionCode, SessionError } from "../error.ts";
import * as Path from "../path.ts";
import type { Reader, Stream, Writer } from "../stream.ts";
import * as Message from "./message.ts";
import * as Namespace from "./namespace.ts";
import { SetupOption, type SetupOptions } from "./parameters.ts";
import { type IetfVersion, Version } from "./version.ts";

/**
 * The MoQ Auth extension (draft-lcurley-moq-auth-00): the moq-lite Auth Stream as
 * moq-transport request streams, negotiated with the AUTH Setup Option on draft-17+.
 *
 * The wire carries namespace prefixes, so a grant is told only when it is a union of
 * subtrees; anything narrower is refused with NOT_SUPPORTED rather than widened.
 *
 * @module
 * @internal
 */

/** Whether a version negotiates the extension. @internal */
export function supported(version: IetfVersion): boolean {
	return version >= Version.DRAFT_17;
}

/**
 * What the peer declared: undefined for no option, otherwise whether it offered the
 * extension. Only an explicit 1 negotiates it.
 *
 * @internal
 */
export function fromSetup(params: SetupOptions, version: IetfVersion): boolean | undefined {
	if (!supported(version)) return undefined;
	const value = params.getVarint(SetupOption.Auth);
	return value === undefined ? undefined : value === 1n;
}

/** Offer the extension, on the versions that negotiate it. @internal */
export function intoSetup(params: SetupOptions, version: IetfVersion) {
	if (supported(version)) params.setVarint(SetupOption.Auth, 1n);
}

/** The REQUEST_ERROR codes AUTH_ERROR carries. */
const UNAUTHORIZED = 0x1;
const NOT_SUPPORTED = 0x3;

/** Longest AUTH_ERROR reason, in bytes, matching the Rust decoder. */
const MAX_REASON = 8192;

function guard(version: IetfVersion) {
	if (!supported(version)) throw new Error("auth not supported for this version");
}

/**
 * AUTH: the first message on an Auth request stream, presenting a token. Each message
 * here encodes its own type, which the dispatcher consumes before decoding.
 *
 * @internal
 */
export class AuthMessage {
	static id = 0x40b61;

	requestId: bigint;
	token: Uint8Array;

	constructor(requestId: bigint, token: Uint8Array) {
		this.requestId = requestId;
		this.token = token;
	}

	async encode(w: Writer, version: IetfVersion): Promise<void> {
		guard(version);
		return Message.encode(
			w,
			async (wr) => {
				await wr.u62(this.requestId);
				await wr.u53(this.token.byteLength);
				if (this.token.byteLength > 0) await wr.write(this.token);
			},
			AuthMessage.id,
		);
	}

	static async decode(r: Reader, version: IetfVersion): Promise<AuthMessage> {
		guard(version);
		return Message.decode(r, async (rd) => {
			const requestId = await rd.u62();
			const size = await rd.u53();
			return new AuthMessage(requestId, await rd.read(size));
		});
	}
}

/** Each pattern as a namespace prefix, or throw {@link Unsupported} for one that is not a subtree. */
function prefixes(patterns: Path.Patterns): Path.Valid[] {
	return patterns.toArray().map((pattern) => {
		const prefix = pattern.asPrefix();
		if (prefix === undefined) throw new Unsupported(`grant not representable as namespace prefixes: ${pattern}`);
		return prefix as Path.Valid;
	});
}

async function encodePrefixes(w: Writer, list: Path.Valid[]) {
	await w.u53(list.length);
	for (const prefix of list) await Namespace.encode(w, prefix);
}

async function decodePrefixes(r: Reader): Promise<Path.Patterns> {
	const count = await r.u53();
	const patterns = new Path.Patterns();
	for (let i = 0; i < count; i++) {
		patterns.insert(Path.Pattern.subtree(await Namespace.decode(r)));
	}
	return patterns;
}

/** AUTH_OK: the grant a token earns, replacing any earlier one on the stream. @internal */
export class AuthOk {
	static id = 0x40b62;

	publish: Path.Patterns;
	subscribe: Path.Patterns;
	/** Milliseconds until the grant lapses, or undefined for never. */
	expires?: number;

	constructor(publish: Path.Patterns, subscribe: Path.Patterns, expires?: number) {
		this.publish = publish;
		this.subscribe = subscribe;
		this.expires = expires;
	}

	async encode(w: Writer, version: IetfVersion): Promise<void> {
		guard(version);
		// Resolved before anything is written, so an unrepresentable grant leaves nothing
		// half-sent.
		const publish = prefixes(this.publish);
		const subscribe = prefixes(this.subscribe);
		return Message.encode(
			w,
			async (wr) => {
				await encodePrefixes(wr, publish);
				await encodePrefixes(wr, subscribe);
				// 0 means never, so a lapsed grant rounds up to the smallest real expiry.
				const expires =
					this.expires === undefined
						? 0
						: Math.min(Math.max(Math.ceil(this.expires), 1), Number.MAX_SAFE_INTEGER);
				await wr.u53(expires);
			},
			AuthOk.id,
		);
	}

	static async decode(r: Reader, version: IetfVersion): Promise<AuthOk> {
		guard(version);
		return Message.decode(r, async (rd) => {
			const publish = await decodePrefixes(rd);
			const subscribe = await decodePrefixes(rd);
			const expires = await rd.u53();
			return new AuthOk(publish, subscribe, expires === 0 ? undefined : expires);
		});
	}
}

/** AUTH_ERROR: the acceptor refusing a token, or revoking it after an AUTH_OK. @internal */
export class AuthError {
	static id = 0x40b63;

	/** A code from the REQUEST_ERROR registry. */
	code: number;
	reason: string;

	constructor(code: number, reason: string) {
		this.code = code;
		this.reason = reason;
	}

	async encode(w: Writer, version: IetfVersion): Promise<void> {
		guard(version);
		if (new TextEncoder().encode(this.reason).byteLength > MAX_REASON) {
			throw new Error("AUTH_ERROR reason exceeds 8,192 bytes");
		}
		return Message.encode(
			w,
			async (wr) => {
				await wr.u53(this.code);
				await wr.string(this.reason);
			},
			AuthError.id,
		);
	}

	static async decode(r: Reader, version: IetfVersion): Promise<AuthError> {
		guard(version);
		return Message.decode(r, async (rd) => {
			const code = await rd.u53();
			const reason = await rd.string();
			if (new TextEncoder().encode(reason).byteLength > MAX_REASON) {
				throw new Error("AUTH_ERROR reason exceeds 8,192 bytes");
			}
			return new AuthError(code, reason);
		});
	}

	/** What the refusal means to the presenter: NOT_SUPPORTED is a grant it could not tell. */
	toError(): Error {
		if (this.code === NOT_SUPPORTED) return new Unsupported(this.reason);
		return new SessionError(SessionCode.Unauthorized, { reason: this.reason });
	}
}

/** The moq-transport binding of the token lifecycle. @internal */
export class IetfAuthWire implements AuthWire {
	#openBi: () => Promise<Stream>;
	#nextRequestId: () => Promise<bigint | undefined>;
	#version: IetfVersion;

	constructor(props: {
		openBi: () => Promise<Stream>;
		nextRequestId: () => Promise<bigint | undefined>;
		version: IetfVersion;
	}) {
		this.#openBi = props.openBi;
		this.#nextRequestId = props.nextRequestId;
		this.#version = props.version;
	}

	async present(token: Uint8Array): Promise<Stream> {
		const requestId = await this.#nextRequestId();
		if (requestId === undefined) throw new Error("no request id available");
		const stream = await this.#openBi();
		await new AuthMessage(requestId, token).encode(stream.writer, this.#version);
		return stream;
	}

	async read(stream: Stream): Promise<WireReply | undefined> {
		if (await stream.reader.done()) return undefined;
		const id = await stream.reader.u53();
		switch (id) {
			case AuthOk.id: {
				const ok = await AuthOk.decode(stream.reader, this.#version);
				return { grant: { publish: ok.publish, subscribe: ok.subscribe, expires: ok.expires } };
			}
			case AuthError.id: {
				const err = await AuthError.decode(stream.reader, this.#version);
				return { refused: err.toError() };
			}
			default:
				throw new Error(`unexpected message on an auth request: 0x${id.toString(16)}`);
		}
	}

	/** The type id is already consumed by the dispatcher. */
	async accept(stream: Stream): Promise<Uint8Array> {
		const msg = await AuthMessage.decode(stream.reader, this.#version);
		return msg.token;
	}

	async grant(stream: Stream, grant: WireGrant): Promise<void> {
		await new AuthOk(grant.publish, grant.subscribe, grant.expires).encode(stream.writer, this.#version);
	}

	async refuse(stream: Stream, code: SessionCode, reason: string): Promise<void> {
		// The public API speaks session codes; this registry distinguishes only a version mismatch.
		const wire = code === SessionCode.Version ? NOT_SUPPORTED : UNAUTHORIZED;
		await new AuthError(wire, reason).encode(stream.writer, this.#version);
	}

	async unsupported(stream: Stream, reason: string): Promise<void> {
		await new AuthError(NOT_SUPPORTED, reason).encode(stream.writer, this.#version);
		await stream.writer.close();
	}
}
