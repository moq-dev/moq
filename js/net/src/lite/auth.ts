import { Unsupported } from "../auth.ts";
import type { AuthWire, WireGrant, WireReply } from "../auth_session.ts";
import { type SessionCode, SessionError } from "../error.ts";
import * as Path from "../path.ts";
import { type Reader, Stream, type Writer } from "../stream.ts";
import * as Message from "./message.ts";
import { StreamId } from "./stream.ts";
import { hasAuth, type Version } from "./version.ts";

function guardAuth(version: Version) {
	if (!hasAuth(version)) throw new Error("auth not supported for this version");
}

/** Longest AUTH_ERROR reason, in bytes, matching the Rust decoder. */
const MAX_REASON = 8192;

/** The first message on an Auth Stream: the token the opener presents. Lite06+. */
export class AuthMessage {
	token: Uint8Array;

	constructor(token: Uint8Array) {
		this.token = token;
	}

	async #encode(w: Writer) {
		await w.u53(this.token.byteLength);
		if (this.token.byteLength > 0) await w.write(this.token);
	}

	static async #decode(r: Reader): Promise<AuthMessage> {
		const size = await r.u53();
		return new AuthMessage(await r.read(size));
	}

	async encode(w: Writer, version: Version): Promise<void> {
		guardAuth(version);
		return Message.encode(w, this.#encode.bind(this));
	}

	static async decode(r: Reader, version: Version): Promise<AuthMessage> {
		guardAuth(version);
		return Message.decode(r, AuthMessage.#decode);
	}
}

// The wire carries prefixes, the ANNOUNCE_REQUEST encoding, so only a union of subtrees is
// representable. Anything else is refused rather than widened to its head.
async function encodePrefixes(w: Writer, patterns: Path.Patterns) {
	const prefixes = patterns.toArray().map((pattern) => {
		const prefix = pattern.asPrefix();
		if (prefix === undefined) throw new Unsupported(`grant not representable as prefixes: ${pattern}`);
		return prefix;
	});
	await w.u53(prefixes.length);
	for (const prefix of prefixes) await w.string(prefix);
}

async function decodePrefixes(r: Reader): Promise<Path.Patterns> {
	const count = await r.u53();
	const patterns = new Path.Patterns();
	for (let i = 0; i < count; i++) {
		patterns.insert(Path.Pattern.subtree(await r.string()));
	}
	return patterns;
}

/** The grant a token earns, as the acceptor writes it. */
export class AuthOk {
	publish: Path.Patterns;
	subscribe: Path.Patterns;
	/** Milliseconds until the grant lapses, or undefined for never. */
	expires?: number;

	constructor(publish: Path.Patterns, subscribe: Path.Patterns, expires?: number) {
		this.publish = publish;
		this.subscribe = subscribe;
		this.expires = expires;
	}

	async #encode(w: Writer) {
		await encodePrefixes(w, this.publish);
		await encodePrefixes(w, this.subscribe);
		// 0 means never, so a lapsed grant rounds up to the smallest real expiry.
		const expires =
			this.expires === undefined ? 0 : Math.min(Math.max(Math.ceil(this.expires), 1), Number.MAX_SAFE_INTEGER);
		await w.u53(expires);
	}

	static async #decode(r: Reader): Promise<AuthOk> {
		const publish = await decodePrefixes(r);
		const subscribe = await decodePrefixes(r);
		const expires = await r.u53();
		return new AuthOk(publish, subscribe, expires === 0 ? undefined : expires);
	}

	async encode(w: Writer, version: Version): Promise<void> {
		guardAuth(version);
		return Message.encode(w, this.#encode.bind(this));
	}

	static async decode(r: Reader, version: Version): Promise<AuthOk> {
		guardAuth(version);
		return Message.decode(r, AuthOk.#decode);
	}
}

/** The acceptor refusing a token, or revoking it after an AUTH_OK. */
export class AuthError {
	/** A code from the session error registry. */
	code: number;
	reason: string;

	constructor(code: number, reason: string) {
		this.code = code;
		this.reason = reason;
	}

	async #encode(w: Writer) {
		if (new TextEncoder().encode(this.reason).byteLength > MAX_REASON) {
			throw new Error("AUTH_ERROR reason exceeds 8,192 bytes");
		}
		await w.u53(this.code);
		await w.string(this.reason);
	}

	static async #decode(r: Reader): Promise<AuthError> {
		const code = await r.u53();
		const reason = await r.string();
		if (new TextEncoder().encode(reason).byteLength > MAX_REASON) {
			throw new Error("AUTH_ERROR reason exceeds 8,192 bytes");
		}
		return new AuthError(code, reason);
	}

	async encode(w: Writer, version: Version): Promise<void> {
		guardAuth(version);
		return Message.encode(w, this.#encode.bind(this));
	}

	static async decode(r: Reader, version: Version): Promise<AuthError> {
		guardAuth(version);
		return Message.decode(r, AuthError.#decode);
	}
}

/** A message the acceptor writes on the Auth Stream. */
export type AuthReply = AuthOk | AuthError;

const AUTH_OK = 0;
const AUTH_ERROR = 1;

/** Encode an AUTH_OK or AUTH_ERROR behind its type. */
export async function encodeAuthReply(w: Writer, reply: AuthReply, version: Version): Promise<void> {
	await w.u53(reply instanceof AuthOk ? AUTH_OK : AUTH_ERROR);
	await reply.encode(w, version);
}

/** Decode the next AUTH_OK or AUTH_ERROR, or undefined once the stream ends. */
export async function decodeAuthReplyMaybe(r: Reader, version: Version): Promise<AuthReply | undefined> {
	guardAuth(version);
	if (await r.done()) return undefined;
	const typ = await r.u53();
	switch (typ) {
		case AUTH_OK:
			return AuthOk.decode(r, version);
		case AUTH_ERROR:
			return AuthError.decode(r, version);
		default:
			throw new Error(`unknown auth reply type: ${typ}`);
	}
}

/** The moq-lite binding of the token lifecycle: one Auth Stream per token. @internal */
export class LiteAuthWire implements AuthWire {
	#quic: WebTransport;
	#version: Version;

	constructor(quic: WebTransport, version: Version) {
		this.#quic = quic;
		this.#version = version;
	}

	async present(token: Uint8Array): Promise<Stream> {
		const stream = await Stream.open(this.#quic);
		await stream.writer.u53(StreamId.Auth);
		await new AuthMessage(token).encode(stream.writer, this.#version);
		return stream;
	}

	async read(stream: Stream): Promise<WireReply | undefined> {
		const reply = await decodeAuthReplyMaybe(stream.reader, this.#version);
		if (!reply) return undefined;
		if (reply instanceof AuthOk) {
			return { grant: { publish: reply.publish, subscribe: reply.subscribe, expires: reply.expires } };
		}
		return { refused: new SessionError(reply.code as SessionCode, { reason: reply.reason }) };
	}

	/** The stream type is already consumed by the dispatcher. */
	async accept(stream: Stream): Promise<Uint8Array> {
		const msg = await AuthMessage.decode(stream.reader, this.#version);
		return msg.token;
	}

	async grant(stream: Stream, grant: WireGrant): Promise<void> {
		await encodeAuthReply(stream.writer, new AuthOk(grant.publish, grant.subscribe, grant.expires), this.#version);
	}

	async refuse(stream: Stream, code: SessionCode, reason: string): Promise<void> {
		await encodeAuthReply(stream.writer, new AuthError(code, reason), this.#version);
	}

	/** Resetting reads as unsupported to the presenter, the same as a peer that predates AUTH. */
	async unsupported(stream: Stream, reason: string): Promise<void> {
		stream.writer.reset(new Unsupported(reason));
	}
}
