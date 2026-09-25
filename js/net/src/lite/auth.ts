import { type Getter, Signal } from "@moq/signals";
import {
	type Auth as AuthApi,
	type Grant,
	grantsEqual,
	type Issued,
	type Request,
	type Requests,
	type Token,
	Unsupported,
} from "../auth.ts";
import { error, SessionCode, SessionError, StreamCode, StreamError } from "../error.ts";
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

function union(grants: Iterable<Grant>): Grant {
	const publish = new Path.Patterns();
	const subscribe = new Path.Patterns();
	let expires: number | undefined;
	for (const grant of grants) {
		for (const pattern of grant.publish) publish.insert(pattern);
		for (const pattern of grant.subscribe) subscribe.insert(pattern);
		// The earliest expiry is when the union next shrinks.
		if (grant.expires !== undefined) expires = Math.min(expires ?? grant.expires, grant.expires);
	}
	return { publish, subscribe, expires };
}

function cancel(): StreamError {
	return new StreamError(StreamCode.Cancel, { message: "cancel" });
}

/** One token this side presented. */
class Presented implements Token {
	readonly grant = new Signal<Grant | undefined>(undefined);
	readonly closed: Promise<Error | null>;
	readonly answered: Promise<void>;
	readonly setup: boolean;
	readonly token: Uint8Array;

	stream?: Stream;
	withdrawn = false;
	isAnswered = false;
	ended = false;

	#close!: (err: Error | null) => void;
	#answer!: () => void;
	#refuse!: (err: Error) => void;

	constructor(token: Uint8Array, setup: boolean) {
		this.token = token;
		this.setup = setup;
		this.closed = new Promise((resolve) => {
			this.#close = resolve;
		});
		this.answered = new Promise((resolve, reject) => {
			this.#answer = resolve;
			this.#refuse = reject;
		});
		// A caller that never awaits the answer must not see an unhandled rejection.
		this.answered.catch(() => void 0);
	}

	answer() {
		if (this.isAnswered) return;
		this.isAnswered = true;
		this.#answer();
	}

	end(err: Error | null) {
		if (this.ended) return;
		this.ended = true;
		this.grant.set(undefined);
		if (!this.isAnswered) {
			this.isAnswered = true;
			this.#refuse(err ?? new Error("withdrawn"));
		}
		this.#close(err);
	}

	close() {
		if (this.withdrawn) return;
		this.withdrawn = true;
		// The stream's loop notices and ends the token; one still opening checks on arrival.
		this.stream?.abort(cancel());
	}
}

/** The peer's token, answered by the application. */
class PeerRequest implements Request {
	readonly token: Uint8Array;
	#issued: IssuedGrant;
	#answered = false;

	constructor(token: Uint8Array, issued: IssuedGrant) {
		this.token = token;
		this.#issued = issued;
	}

	accept(grant: Grant): Issued {
		if (this.#answered) throw new Error("already answered");
		this.#answered = true;
		this.#issued.update(grant);
		return this.#issued;
	}

	reject(code: SessionCode, reason: string): void {
		if (this.#answered) throw new Error("already answered");
		this.#answered = true;
		this.#issued.revoke(code, reason);
	}
}

/** Our side of one of the peer's tokens: the grant we issued and its stream. */
class IssuedGrant implements Issued {
	readonly closed: Promise<Error | null>;
	#stream: Stream;
	#version: Version;
	#writes = Promise.resolve();
	#done = false;

	constructor(stream: Stream, version: Version) {
		this.#stream = stream;
		this.#version = version;
		// The presenter withdraws by closing or cancelling its side.
		this.closed = stream.reader.closed.then(
			() => null,
			(err: unknown) => (err instanceof StreamError && err.code === StreamCode.Cancel ? null : error(err)),
		);
	}

	#write(reply: AuthReply) {
		this.#writes = this.#writes
			.then(() => encodeAuthReply(this.#stream.writer, reply, this.#version))
			.catch((err: unknown) => {
				// The peer already closed the stream: nothing left to tell it.
				if (err instanceof StreamError) return;
				// This wire carries prefixes only, so a pattern grant cannot be told, only
				// withheld: reset the stream, which the presenter reads as unsupported rather
				// than refused. Never widen it. Any other reply that fails to encode resets
				// the same way.
				if (!(err instanceof Unsupported)) console.warn("auth reply not sent", err);
				this.#done = true;
				this.#stream.writer.reset(err);
			});
	}

	update(grant: Grant): void {
		if (this.#done) return;
		const expires = grant.expires === undefined ? undefined : grant.expires - Date.now();
		this.#write(new AuthOk(grant.publish, grant.subscribe, expires));
	}

	revoke(code: SessionCode, reason: string): void {
		if (this.#done) return;
		this.#write(new AuthError(code, reason));
		this.close();
	}

	close(): void {
		if (this.#done) return;
		this.#done = true;
		this.#writes = this.#writes.then(() => this.#stream.writer.close());
	}
}

/** The peer's tokens, queued for the application. */
class RequestQueue implements Requests {
	#queue: PeerRequest[] = [];
	#waiters: ((request: PeerRequest | undefined) => void)[] = [];
	#closed = false;

	push(request: PeerRequest): boolean {
		if (this.#closed) return false;
		const waiter = this.#waiters.shift();
		if (waiter) waiter(request);
		else this.#queue.push(request);
		return true;
	}

	next(): Promise<Request | undefined> {
		const next = this.#queue.shift();
		if (next || this.#closed) return Promise.resolve(next);
		return new Promise((resolve) => this.#waiters.push(resolve));
	}

	close(): void {
		this.#closed = true;
		for (const request of this.#queue.splice(0)) {
			request.reject(SessionCode.Unauthorized, "not accepting tokens");
		}
		for (const waiter of this.#waiters.splice(0)) waiter(undefined);
	}
}

/** Constructor options for {@link AuthSession}. @internal */
export interface AuthSessionProps {
	quic: WebTransport;
	version: Version;
	/** What the default acceptor grants the peer's connection credential. */
	peerGrant: Grant;
}

/**
 * A lite session's tokens and grants: presents ours, one AUTH stream each, and answers
 * the peer's.
 *
 * @internal
 */
export class AuthSession implements AuthApi {
	#quic: WebTransport;
	#version: Version;
	#peerGrant: Grant;

	#union = new Signal<Grant | undefined>(undefined);
	// The peer replied to some token, so the union is known even when empty.
	#replied = false;
	#tokens = new Set<Presented>();
	#setupPending = new Signal(0);
	#acceptor: "undecided" | "default" | RequestQueue = "undecided";
	#closed = false;

	// Whoever answers the peer's tokens is decided once, after the task that established
	// the session: an app that calls requests() as soon as connect/accept resolves always
	// wins, however quickly the peer's first token arrives.
	#decided = new Promise<void>((resolve) => setTimeout(resolve, 0));

	constructor({ quic, version, peerGrant }: AuthSessionProps) {
		this.#quic = quic;
		this.#version = version;
		this.#peerGrant = peerGrant;

		// Present the connection's own credential right away, so both sides learn their
		// grant without waiting on the app.
		if (hasAuth(version)) this.#present(new Uint8Array(), true);
	}

	get grant(): Getter<Grant | undefined> {
		return this.#union;
	}

	async add(token: string | Uint8Array): Promise<Token> {
		if (!hasAuth(this.#version) || this.#closed) throw new Unsupported();
		const bytes = typeof token === "string" ? new TextEncoder().encode(token) : token;
		const presented = this.#present(bytes, false);
		await presented.answered;
		return presented;
	}

	requests(): Requests {
		if (this.#acceptor !== "undecided") throw new Error("auth requests already taken or answered by default");
		const queue = new RequestQueue();
		if (!hasAuth(this.#version)) queue.close();
		this.#acceptor = queue;
		return queue;
	}

	/** Resolves once every token the session presented at setup has its first reply. */
	async setupAnswered(): Promise<void> {
		while (this.#setupPending.peek() > 0) await this.#setupPending.changed();
	}

	/** Answer one of the peer's AUTH streams, for the life of its token. */
	async serve(stream: Stream): Promise<void> {
		const msg = await AuthMessage.decode(stream.reader, this.#version);
		await this.#decided;
		if (this.#acceptor === "undecided") this.#acceptor = "default";

		const issued = new IssuedGrant(stream, this.#version);
		if (this.#acceptor instanceof RequestQueue) {
			const request = new PeerRequest(msg.token, issued);
			if (!this.#acceptor.push(request)) request.reject(SessionCode.Unauthorized, "not accepting tokens");
		} else if (msg.token.byteLength > 0) {
			// Only the connection's own credential has a default answer. Resetting reads as
			// unsupported to the presenter, the same as a peer that predates AUTH.
			throw new Unsupported("no acceptor for tokens");
		} else {
			issued.update(this.#peerGrant);
		}

		await issued.closed;
		issued.close();
	}

	/** End the session: fail every pending token and close the requests. */
	close() {
		if (this.#closed) return;
		this.#closed = true;
		for (const token of this.#tokens) token.end(new Error("session closed"));
		this.#tokens.clear();
		if (this.#acceptor instanceof RequestQueue) this.#acceptor.close();
	}

	#present(token: Uint8Array, setup: boolean): Presented {
		const presented = new Presented(token, setup);
		this.#tokens.add(presented);
		if (setup) this.#setupPending.update((n) => n + 1);
		void this.#run(presented);
		return presented;
	}

	async #run(token: Presented) {
		let result: Error | null = null;
		try {
			const stream = await Stream.open(this.#quic);
			token.stream = stream;
			if (token.withdrawn) throw cancel();

			await stream.writer.u53(StreamId.Auth);
			await new AuthMessage(token.token).encode(stream.writer, this.#version);

			for (;;) {
				const reply = await decodeAuthReplyMaybe(stream.reader, this.#version);
				if (!reply) {
					// The peer ended the grant without revoking it, or closed without ever
					// answering.
					result = token.isAnswered ? null : new Unsupported();
					break;
				}
				if (reply instanceof AuthOk) {
					const expires = reply.expires === undefined ? undefined : Date.now() + reply.expires;
					token.grant.set({ publish: reply.publish, subscribe: reply.subscribe, expires });
					this.#replied = true;
					this.#answered(token);
					this.#recompute();
					continue;
				}
				console.warn(`auth token refused: code=${reply.code} reason=${reply.reason}`);
				// A refused setup token leaves an empty union, not an unknown (unrestricted) one.
				this.#replied = true;
				result = new SessionError(reply.code as SessionCode, { reason: reply.reason });
				stream.close();
				break;
			}
		} catch (err: unknown) {
			if (token.withdrawn) {
				result = null;
			} else if (!token.isAnswered && err instanceof StreamError) {
				// A peer that predates AUTH resets a stream type it does not know.
				result = new Unsupported();
			} else {
				result = error(err);
				if (!this.#closed) console.warn("auth token ended", result);
			}
		}

		// A token that ends unanswered counts as answered for enforcement: its refusal is
		// the reply.
		const unanswered = !token.isAnswered;
		token.end(result);
		if (unanswered && token.setup) this.#setupPending.update((n) => n - 1);
		this.#tokens.delete(token);
		this.#recompute();
	}

	#answered(token: Presented) {
		if (token.isAnswered) return;
		token.answer();
		if (token.setup) this.#setupPending.update((n) => n - 1);
	}

	#recompute() {
		const granted: Grant[] = [];
		for (const token of this.#tokens) {
			const grant = token.grant.peek();
			if (grant) granted.push(grant);
		}
		// Undefined until the first reply; an empty union afterwards grants nothing.
		if (granted.length === 0 && !this.#replied) return;
		const next = union(granted);
		if (!grantsEqual(next, this.#union.peek())) this.#union.set(next);
	}
}
