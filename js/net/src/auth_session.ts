import { Instant, Nano, now, wall } from "@moq/clock";
import { type Dispose, type Getter, Once, Signal } from "@moq/signals";
import {
	type Auth,
	type Grant,
	grantsEqual,
	type Handle,
	type Issued,
	type Request,
	type Requests,
	Unsupported,
} from "./auth.ts";
import { closeReason, error, ProtocolViolation, SessionCode, SessionError, StreamCode, StreamError } from "./error.ts";
import * as Path from "./path.ts";
import type { Reader, Stream, Writer } from "./stream.ts";
import { nextTask } from "./util/yield.ts";

/** Write a grant's patterns as a count and each one's canonical text, the encoding both wires' AUTH_OK share. @internal */
export async function encodePatterns(w: Writer, patterns: Path.Patterns) {
	await w.u53(patterns.size);
	for (const pattern of patterns) await w.string(pattern.text);
}

/** Read a grant's patterns, refusing any that is invalid or not canonical. @internal */
export async function decodePatterns(r: Reader): Promise<Path.Patterns> {
	const count = await r.u53();
	const patterns: Path.Pattern[] = [];
	for (let i = 0; i < count; i++) {
		const text = await r.string();
		const pattern = Path.Pattern.parse(text);
		// Only the canonical spelling is valid, so each pattern has one encoding.
		if (pattern.text !== text) throw new Error(`non-canonical pattern: ${text}`);
		patterns.push(pattern);
	}
	// Reduce once, so a peer's count never turns into quadratic work.
	return new Path.Patterns(patterns);
}

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
	/** Wait for the presenter to withdraw while validating follow-up messages. */
	withdrawn(stream: Stream): Promise<void>;
	/** Tell the presenter a grant. Throws {@link Unsupported} when this wire cannot represent it. */
	grant(stream: Stream, grant: WireGrant): Promise<void>;
	/** Refuse or revoke the presenter's token. */
	refuse(stream: Stream, code: SessionCode, reason: string): Promise<void>;
	/** End the stream telling the presenter its token or grant cannot be answered here. */
	unsupported(stream: Stream, reason: string): Promise<void>;
}

/** The paths any of `grants` allows. Reduced once, so it costs about the grants' total size. */
function union(grants: Iterable<Grant>): Grant {
	const publish: Path.Pattern[] = [];
	const subscribe: Path.Pattern[] = [];
	let expires: number | undefined;
	for (const grant of grants) {
		for (const pattern of grant.publish) publish.push(pattern);
		for (const pattern of grant.subscribe) subscribe.push(pattern);
		// The earliest expiry is when the union next shrinks.
		if (grant.expires !== undefined) expires = Math.min(expires ?? grant.expires, grant.expires);
	}
	return { publish: new Path.Patterns(publish), subscribe: new Path.Patterns(subscribe), expires };
}

// The transport closed under the stream, which ends the whole session rather than one token.
function transportClosed(err: unknown): boolean {
	return typeof err === "object" && err !== null && "source" in err && err.source === "session";
}

function cancel(): StreamError {
	return new StreamError(StreamCode.Cancel, { message: "cancel" });
}

/** One token this side presented. */
class Presented implements Auth {
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
		// One without a stream to come (carried only by requests) ends here.
		if (this.stream) this.stream.abort(cancel());
		else if (this.isAnswered) this.end(null);
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

	get closed(): Promise<Error | null> {
		return this.#issued.closed;
	}

	get ended(): boolean {
		return this.#issued.ended;
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
	ended = false;
	readonly closed: Promise<Error | null>;
	#stream: Stream;
	#wire: AuthWire;
	#writes = Promise.resolve();
	#done = false;

	constructor(stream: Stream, wire: AuthWire) {
		this.#stream = stream;
		this.#wire = wire;
		// The presenter withdraws by closing or cancelling its side.
		this.closed = wire
			.withdrawn(stream)
			.then(
				() => null,
				(err: unknown) => (err instanceof StreamError && err.code === StreamCode.Cancel ? null : error(err)),
			)
			.finally(() => {
				this.ended = true;
			});
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
		const expires = grant.expires === undefined ? undefined : grant.expires - wall();
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
	// How many requests survived the last prune: pruning again only once the queue doubles
	// keeps a push amortized O(1).
	#survivors = 0;
	#waiters: ((request: PeerRequest | undefined) => void)[] = [];
	#closed = false;

	/** Queue the token for the application, or refuse it when closed. */
	push(request: PeerRequest): void {
		if (this.#closed) {
			request.reject(SessionCode.Unauthorized, "not accepting tokens");
			return;
		}
		// A withdrawn token gave its stream back, so it must not stay queued while the
		// application is busy: that would let a peer that keeps withdrawing grow the queue.
		if (this.#queue.length >= 2 * Math.max(this.#survivors, 1)) {
			this.#queue = this.#queue.filter((queued) => !queued.ended);
			this.#survivors = this.#queue.length;
		}
		const waiter = this.#waiters.shift();
		if (waiter) waiter(request);
		else this.#queue.push(request);
	}

	next(): Promise<Request | undefined> {
		let next = this.#queue.shift();
		while (next?.ended) next = this.#queue.shift();
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
	/**
	 * Whether a request can carry a token (moq-transport), so {@link AuthSession.add} keeps one
	 * the wire cannot present, for the announcements that name it.
	 */
	requests?: boolean;
	/** What the default acceptor grants the peer's connection credential. */
	peerGrant: Grant;
}

/**
 * A session's tokens and grants: presents ours, one stream each, and answers the peer's.
 * The wire decides only the encoding; see {@link AuthWire}.
 *
 * @internal
 */
export class AuthSession implements Handle {
	/** The peer sent a malformed AUTH message; the owning connection closes the session. */
	readonly violated = new Once<ProtocolViolation>();
	#wire?: AuthWire;
	#requests: boolean;
	#peerGrant: Grant;
	// Every token this session issued, so a request carries only its own.
	#issued = new WeakSet<Presented>();
	#ended = new Signal(0);

	#union = new Signal<Grant | undefined>(undefined);
	// The union until the session ends, which is not a revocation: what the session covered
	// ends with the session's own error, not as unauthorized.
	#scope = new Signal<Grant | undefined>(undefined);
	// The peer replied to some token, so the union is known even when empty.
	#replied = false;
	#tokens = new Set<Presented>();
	#setupPending = new Signal(0);
	#pending = new Signal(0);
	#acceptor: "undecided" | "default" | RequestQueue = "undecided";
	#closed = false;

	// Whoever answers the peer's tokens is decided once, after the task that established
	// the session: an app that calls requests() as soon as connect/accept resolves always
	// wins, however quickly the peer's first token arrives.
	#decided = nextTask();

	constructor({ wire, requests, peerGrant }: AuthSessionProps) {
		this.#wire = wire;
		this.#requests = requests ?? false;
		this.#peerGrant = peerGrant;

		// Present the connection's own credential right away, so both sides learn their
		// grant without waiting on the app.
		if (wire) this.#present(wire, new Uint8Array(), true);
	}

	get grant(): Getter<Grant | undefined> {
		return this.#union;
	}

	/** The grant this session enforces: {@link grant}, kept as it was once the session ends. */
	get scope(): Getter<Grant | undefined> {
		return this.#scope;
	}

	/** Whether this session exchanges tokens at all. */
	get negotiated(): boolean {
		return this.#wire !== undefined;
	}

	async add(token: string | Uint8Array): Promise<Auth> {
		if (this.#closed || (!this.#wire && !this.#requests)) throw new Unsupported();
		// A copy, so the caller reusing its buffer never changes what a request carries.
		const bytes = typeof token === "string" ? new TextEncoder().encode(token) : token.slice();
		if (!this.#wire) {
			// Only a request carries it, and the peer judges it there: no grant to wait for.
			const presented = new Presented(bytes, false);
			presented.answer();
			this.#track(presented);
			return presented;
		}
		const presented = this.#present(this.#wire, bytes, false);
		await presented.answered;
		return presented;
	}

	/**
	 * The bytes a request carries for `token`: undefined for none, for one another session
	 * issued, or for one that ended.
	 */
	carried(token: Auth | undefined): Uint8Array | undefined {
		if (!this.#requests || !(token instanceof Presented) || !this.#issued.has(token) || token.ended)
			return undefined;
		return token.token;
	}

	/** Bumps whenever one of this session's tokens ends, so a request carrying it drops it. */
	get ended(): Getter<number> {
		return this.#ended;
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

	/**
	 * Wait for presented tokens that might admit this path, without delaying a covered path.
	 * Resolves false once `until`, the request's own lifetime, settles first, so an abandoned
	 * request never waits on a token that may never be answered. `until` is called only once
	 * the request has to wait.
	 */
	async ready(
		path: Path.Valid,
		direction: "publish" | "subscribe",
		{ signal, until }: { signal?: AbortSignal; until?: () => PromiseLike<unknown> } = {},
	): Promise<boolean> {
		let started: Instant | undefined;
		let ended = false;
		let wake: (() => void) | undefined;
		const end = () => {
			ended = true;
			wake?.();
		};
		try {
			while (this.#pending.peek() > 0 && !this.#union.peek()?.[direction].matches(path)) {
				if (ended) return false;
				if (started === undefined) {
					started = now();
					until?.().then(end, end);
				}
				signal?.throwIfAborted();
				await new Promise<void>((resolve, reject) => {
					const cleanup = () => {
						pending();
						grant();
						wake = undefined;
						signal?.removeEventListener("abort", abort);
					};
					const done = () => {
						cleanup();
						resolve();
					};
					const abort = () => {
						cleanup();
						reject(signal?.reason);
					};
					const pending = this.#pending.changed(done);
					const grant = this.#union.changed(done);
					wake = done;
					signal?.addEventListener("abort", abort, { once: true });
				});
			}
			return true;
		} finally {
			if (started !== undefined) {
				console.debug(
					`request waited for authorization: path=${path} waited=${Nano.toMilli(Instant.durationSince(now(), started))}ms`,
				);
			}
		}
	}

	/** Answer one of the peer's token streams, for the life of its token. */
	async serve(stream: Stream): Promise<void> {
		const wire = this.#wire;
		if (!wire) throw new Error("auth not negotiated");

		let token: Uint8Array;
		try {
			token = await wire.accept(stream);
		} catch (err) {
			if (
				err instanceof StreamError ||
				err instanceof SessionError ||
				err instanceof Unsupported ||
				transportClosed(err)
			)
				throw err;
			throw new ProtocolViolation("malformed AUTH", { cause: err });
		}
		await this.#decided;
		if (this.#acceptor === "undecided") this.#acceptor = "default";

		const issued = new IssuedGrant(stream, wire);
		if (this.#acceptor instanceof RequestQueue) {
			const request = new PeerRequest(token, issued);
			this.#acceptor.push(request);
		} else if (token.byteLength > 0) {
			// Only the connection's own credential has a default answer; a token needs
			// someone to verify it.
			await wire.unsupported(stream, "no acceptor for tokens");
			return;
		} else {
			issued.update(this.#peerGrant);
		}

		const ended = await issued.closed;
		issued.close();
		if (ended instanceof ProtocolViolation) throw ended;
	}

	/** End the session: fail every pending token and close the requests. */
	close() {
		if (this.#closed) return;
		this.#closed = true;
		this.#pending.set(0);
		for (const token of this.#tokens) token.end(new Error("session closed"));
		this.#tokens.clear();
		this.#recompute();
		if (this.#acceptor instanceof RequestQueue) this.#acceptor.close();
	}

	#present(wire: AuthWire, token: Uint8Array, setup: boolean): Presented {
		const presented = new Presented(token, setup);
		this.#track(presented);
		if (setup) this.#setupPending.update((n) => n + 1);
		if (token.byteLength > 0) this.#pending.update((n) => n + 1);
		void this.#run(wire, presented);
		return presented;
	}

	/** Hold `token` until it ends, so the session's close ends it too. */
	#track(token: Presented) {
		this.#tokens.add(token);
		this.#issued.add(token);
		void token.closed.then(() => {
			this.#tokens.delete(token);
			this.#ended.update((n) => n + 1);
		});
	}

	async #run(wire: AuthWire, token: Presented) {
		let result: Error | null = null;
		let closed = false;
		try {
			const stream = await wire.present(token.token);
			token.stream = stream;
			if (token.withdrawn) throw cancel();

			for (;;) {
				let reply: WireReply | undefined;
				try {
					reply = await wire.read(stream);
				} catch (err) {
					if (
						err instanceof StreamError ||
						err instanceof SessionError ||
						err instanceof Unsupported ||
						transportClosed(err)
					)
						throw err;
					throw new ProtocolViolation("malformed AUTH reply", { cause: err });
				}
				if (!reply) {
					// The peer ended the grant without revoking it, or closed without ever
					// answering. Close our side too, which is what settles its Issued.closed.
					result = token.isAnswered ? null : new Unsupported();
					stream.close();
					break;
				}
				if ("grant" in reply) {
					const expires = reply.grant.expires === undefined ? undefined : wall() + reply.grant.expires;
					token.grant.set({ publish: reply.grant.publish, subscribe: reply.grant.subscribe, expires });
					this.#replied = true;
					this.#answered(token);
					this.#recompute();
					continue;
				}
				console.warn("auth token refused", reply.refused);
				// A refused setup token leaves an empty union, not an unknown (unrestricted)
				// one. A grant the acceptor could not tell, or never judged, is unknown, not empty.
				const unjudged =
					reply.refused instanceof Unsupported ||
					(reply.refused instanceof StreamError && reply.refused.code === StreamCode.GoingAway);
				if (!unjudged) this.#replied = true;
				result = reply.refused;
				stream.close();
				break;
			}
		} catch (err: unknown) {
			closed = transportClosed(err);
			if (token.withdrawn) {
				result = null;
			} else if (!token.isAnswered && err instanceof StreamError) {
				// A peer that predates AUTH resets a stream type it does not know.
				result = new Unsupported();
			} else {
				result = error(err);
				if (result instanceof ProtocolViolation && !this.#closed && this.violated.peek() === undefined) {
					this.violated.set(result);
				}
				if (!this.#closed) console.warn("auth token ended", result);
			}
		}

		// A token that ends unanswered counts as answered for enforcement: its refusal is
		// the reply.
		const unanswered = !token.isAnswered;
		token.end(result);
		if (unanswered && token.setup) this.#setupPending.update((n) => n - 1);
		if (unanswered && token.token.byteLength > 0 && !this.#closed) this.#pending.update((n) => n - 1);
		this.#tokens.delete(token);
		// Closed before the union shrinks, so the scope never reads it as a revocation.
		if (closed) this.close();
		this.#recompute();
	}

	#answered(token: Presented) {
		if (token.isAnswered) return;
		token.answer();
		if (token.setup) this.#setupPending.update((n) => n - 1);
		if (token.token.byteLength > 0 && !this.#closed) this.#pending.update((n) => n - 1);
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
		if (!this.#closed && !grantsEqual(next, this.#scope.peek())) this.#scope.set(next);
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
	auth,
}: {
	quic: WebTransport;
	/** The broadcasts this side publishes, undefined once the origin ends. */
	advertised: Getter<ReadonlyMap<Path.Valid, unknown> | undefined>;
	grant: Getter<Grant | undefined>;
	setupAnswered: Promise<void>;
	auth?: AuthSession;
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
					await auth?.ready(path, "publish");
					// A broadcast withdrawn during the wait is no longer ours to judge.
					if (!advertised.peek()?.has(path)) continue;
					if (grant.peek()?.publish.matches(path)) {
						live.add(path);
						continue;
					}
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
