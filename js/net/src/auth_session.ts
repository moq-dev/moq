import { type Dispose, type Getter, Signal } from "@moq/signals";
import {
	type Auth as AuthApi,
	type Grant,
	grantsEqual,
	type Issued,
	type Request,
	type Requests,
	type Token,
	Unsupported,
} from "./auth.ts";
import { closeReason, error, SessionCode, StreamCode, StreamError } from "./error.ts";
import * as Path from "./path.ts";
import type { Stream } from "./stream.ts";

/** A grant as the wire carries it: the expiry is milliseconds from now. @internal */
export interface WireGrant {
	publish: Path.Patterns;
	subscribe: Path.Patterns;
	expires?: number;
}

/** One reply read off a presented token's stream. @internal */
export type WireReply = { grant: WireGrant } | { refused: Error };

/**
 * How one wire carries AUTH, AUTH_OK, and AUTH_ERROR, so moq-lite and moq-transport share
 * the token lifecycle.
 *
 * @internal
 */
export interface AuthWire {
	/** Open a stream and present the token on it. */
	present(token: Uint8Array): Promise<Stream>;
	/** The next reply on a presented token's stream, or undefined once the acceptor finished it. */
	read(stream: Stream): Promise<WireReply | undefined>;
	/** The token the peer presents on a stream it opened. */
	accept(stream: Stream): Promise<Uint8Array>;
	/** Tell the presenter a grant. Throws {@link Unsupported} when this wire cannot represent it. */
	grant(stream: Stream, grant: WireGrant): Promise<void>;
	/** Refuse or revoke the presenter's token. */
	refuse(stream: Stream, code: SessionCode, reason: string): Promise<void>;
	/** End the stream telling the presenter its token or grant cannot be answered here. */
	unsupported(stream: Stream, reason: string): Promise<void>;
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
	#wire: AuthWire;
	#writes = Promise.resolve();
	#done = false;

	constructor(stream: Stream, wire: AuthWire) {
		this.#stream = stream;
		this.#wire = wire;
		// The presenter withdraws by closing or cancelling its side.
		this.closed = stream.reader.closed.then(
			() => null,
			(err: unknown) => (err instanceof StreamError && err.code === StreamCode.Cancel ? null : error(err)),
		);
	}

	#write(write: () => Promise<void>) {
		this.#writes = this.#writes.then(write).catch((err: unknown) => {
			// The peer already closed the stream: nothing left to tell it.
			if (err instanceof StreamError) return;
			// A grant the wire cannot carry is withheld, never widened, and so is any other
			// reply that fails to encode.
			if (!(err instanceof Unsupported)) console.warn("auth reply not sent", err);
			this.#done = true;
			return this.#wire.unsupported(this.#stream, error(err).message).catch(() => void 0);
		});
	}

	update(grant: Grant): void {
		if (this.#done) return;
		const expires = grant.expires === undefined ? undefined : grant.expires - Date.now();
		this.#write(() =>
			this.#wire.grant(this.#stream, { publish: grant.publish, subscribe: grant.subscribe, expires }),
		);
	}

	revoke(code: SessionCode, reason: string): void {
		if (this.#done) return;
		this.#write(() => this.#wire.refuse(this.#stream, code, reason));
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
	/** How this session's wire carries the exchange, or undefined when it carries none. */
	wire?: AuthWire;
	/** What the default acceptor grants the peer's connection credential. */
	peerGrant: Grant;
}

/**
 * A session's tokens and grants: presents ours, one stream each, and answers the peer's.
 * The wire decides only the encoding; see {@link AuthWire}.
 *
 * @internal
 */
export class AuthSession implements AuthApi {
	#wire?: AuthWire;
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

	constructor({ wire, peerGrant }: AuthSessionProps) {
		this.#wire = wire;
		this.#peerGrant = peerGrant;

		// Present the connection's own credential right away, so both sides learn their
		// grant without waiting on the app.
		if (wire) this.#present(wire, new Uint8Array(), true);
	}

	get grant(): Getter<Grant | undefined> {
		return this.#union;
	}

	/** Whether this session exchanges tokens at all. */
	get negotiated(): boolean {
		return this.#wire !== undefined;
	}

	async add(token: string | Uint8Array): Promise<Token> {
		if (!this.#wire || this.#closed) throw new Unsupported();
		const bytes = typeof token === "string" ? new TextEncoder().encode(token) : token;
		const presented = this.#present(this.#wire, bytes, false);
		await presented.answered;
		return presented;
	}

	requests(): Requests {
		if (this.#acceptor !== "undecided") throw new Error("auth requests already taken or answered by default");
		const queue = new RequestQueue();
		if (!this.#wire) queue.close();
		this.#acceptor = queue;
		return queue;
	}

	/** Resolves once every token the session presented at setup has its first reply. */
	async setupAnswered(): Promise<void> {
		while (this.#setupPending.peek() > 0) await this.#setupPending.changed();
	}

	/** Answer one of the peer's token streams, for the life of its token. */
	async serve(stream: Stream): Promise<void> {
		const wire = this.#wire;
		if (!wire) throw new Error("auth not negotiated");

		const token = await wire.accept(stream);
		await this.#decided;
		if (this.#acceptor === "undecided") this.#acceptor = "default";

		const issued = new IssuedGrant(stream, wire);
		if (this.#acceptor instanceof RequestQueue) {
			const request = new PeerRequest(token, issued);
			if (!this.#acceptor.push(request)) request.reject(SessionCode.Unauthorized, "not accepting tokens");
		} else if (token.byteLength > 0) {
			// Only the connection's own credential has a default answer; a token needs
			// someone to verify it.
			await wire.unsupported(stream, "no acceptor for tokens");
			return;
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

	#present(wire: AuthWire, token: Uint8Array, setup: boolean): Presented {
		const presented = new Presented(token, setup);
		this.#tokens.add(presented);
		if (setup) this.#setupPending.update((n) => n + 1);
		void this.#run(wire, presented);
		return presented;
	}

	async #run(wire: AuthWire, token: Presented) {
		let result: Error | null = null;
		try {
			const stream = await wire.present(token.token);
			token.stream = stream;
			if (token.withdrawn) throw cancel();

			for (;;) {
				const reply = await wire.read(stream);
				if (!reply) {
					// The peer ended the grant without revoking it, or closed without ever
					// answering.
					result = token.isAnswered ? null : new Unsupported();
					break;
				}
				if ("grant" in reply) {
					const expires = reply.grant.expires === undefined ? undefined : Date.now() + reply.grant.expires;
					token.grant.set({ publish: reply.grant.publish, subscribe: reply.grant.subscribe, expires });
					this.#replied = true;
					this.#answered(token);
					this.#recompute();
					continue;
				}
				console.warn("auth token refused", reply.refused);
				// A refused setup token leaves an empty union, not an unknown (unrestricted)
				// one. A grant the acceptor could not tell is unknown, not empty.
				if (!(reply.refused instanceof Unsupported)) this.#replied = true;
				result = reply.refused;
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

/**
 * Close the session when our origin publishes a broadcast our grant does not cover,
 * instead of leaving it to wait for a subscription that never comes.
 *
 * Starts once the tokens the session presented at setup are answered, then checks each
 * broadcast when it first appears. A grant that later shrinks withdraws what it no longer
 * covers without closing anything: the grant is read before the table, so a revocation is
 * never mistaken for a new unauthorized publication. Only the dialing side enforces: a
 * server's publish origin is everything the peer may read, not what it intends to push.
 *
 * @internal
 */
export async function enforceGrant({
	quic,
	advertised,
	grant,
	setupAnswered,
}: {
	quic: WebTransport;
	/** The broadcasts this side publishes, undefined once the origin ends. */
	advertised: Getter<ReadonlyMap<Path.Valid, unknown> | undefined>;
	grant: Getter<Grant | undefined>;
	setupAnswered: Promise<void>;
}): Promise<void> {
	const closed = quic.closed.then(
		() => "closed" as const,
		() => "closed" as const,
	);
	if ((await Promise.race([setupAnswered.then(() => "ready" as const), closed])) === "closed") return;

	// Every broadcast admitted so far that is still published.
	const live = new Set<Path.Valid>();
	for (;;) {
		let dispose: Dispose = () => {};
		const woke = new Promise<"changed">((resolve) => {
			const table = advertised.changed(() => resolve("changed"));
			const granted = grant.changed(() => resolve("changed"));
			dispose = () => {
				table();
				granted();
			};
		});

		const current = grant.peek();
		const table = advertised.peek();
		if (!table) {
			dispose();
			return;
		}
		if (current) {
			for (const path of live) {
				if (!table.has(path)) live.delete(path);
			}
			for (const path of table.keys()) {
				if (live.has(path)) continue;
				if (!current.publish.matches(path)) {
					console.error(`publishing outside our grant; closing the session: broadcast=${path}`);
					quic.close({
						closeCode: SessionCode.Unauthorized,
						reason: closeReason(`unauthorized: ${path}`),
					});
					dispose();
					return;
				}
				live.add(path);
			}
		}

		const why = await Promise.race([woke, closed]);
		dispose();
		if (why === "closed") return;
	}
}
