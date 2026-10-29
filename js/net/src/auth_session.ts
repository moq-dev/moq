import { Instant, Milli, Nano, now, sleep, wall, yieldNow } from "@moq/clock";
import { type Dispose, type Getter, Once, race, Signal } from "@moq/signals";
import type { Credential, Grant, Issued, Request, Requests } from "./auth.ts";
import { closeReason, error, ProtocolViolation, SessionCode, SessionError, StreamCode, StreamError } from "./error.ts";
import * as Path from "./path.ts";
import type { Reader, Stream, Writer } from "./stream.ts";

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

/** Whether two grants allow the same paths and lapse together. */
function grantsEqual(a: Grant | undefined, b: Grant | undefined): boolean {
	if (a === undefined || b === undefined) return a === b;
	return a.publish.equals(b.publish) && a.subscribe.equals(b.subscribe) && a.expires === b.expires;
}

/** The session or peer carries no AUTH exchange, or takes no tokens in band. */
export class Unsupported extends Error {
	/** The moq-lite session code for this condition. */
	readonly code = SessionCode.Unsupported;
	constructor(message = "auth unsupported") {
		super(message);
		this.name = "Unsupported";
	}
}

/** A handle already backs a session that is open and not draining, or its requests were already taken. */
export class Duplicate extends Error {
	constructor(message = "auth handle already backs an active session") {
		super(message);
		this.name = "Duplicate";
	}
}

// How long a token refused for a reason that may pass waits before its session presents it
// again, doubling on each refusal up to RETRY_MAX.
const RETRY_INITIAL = Milli(1000);
const RETRY_MAX = Milli(30_000);

// Whether a refusal is the acceptor failing to decide rather than its verdict on the token,
// such as an unreachable auth server or a full verification queue.
function transient(err: Error | null): boolean {
	return (
		err instanceof SessionError &&
		(err.code === SessionCode.Internal ||
			err.code === SessionCode.TooManyRequests ||
			err.code === SessionCode.Timeout)
	);
}

// Whether a token's stream ending with `result` is the peer's verdict on it: a refusal that will
// not pass, Unsupported, or a clean close after a grant. Anything else left the token undecided.
function isVerdict(refused: boolean, granted: boolean, result: Error | null): boolean {
	return (refused && !transient(result)) || result instanceof Unsupported || (result === null && granted);
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
/** One token as one session presents it. */
class Presented {
	readonly token: Uint8Array;
	/** The session's own credential, presented at setup on this session alone. */
	readonly setup: boolean;
	/** The handle's token this presents, or undefined for the session's own credential. */
	readonly owner?: Token;

	/** The latest grant: undefined before the first, during a retry, and once it ends here. */
	grant?: Grant;
	stream?: Stream;
	/** Wakes a retry backoff early once the presentation is no longer wanted. */
	retry?: AbortController;
	withdrawn = false;
	isAnswered = false;
	ended = false;

	constructor(token: Uint8Array, setup: boolean, owner?: Token) {
		this.token = token;
		this.setup = setup;
		this.owner = owner;
	}
}

/** A token the application added to a {@link Handle}, presented on every session of it. */
class Token implements Credential {
	readonly grant = new Signal<Grant | undefined>(undefined);
	readonly closed: Promise<Error | null>;
	readonly answered: Promise<void>;
	readonly bytes: Uint8Array;

	isAnswered = false;
	ended = false;

	#withdraw: (token: Token) => void;
	#close!: (err: Error | null) => void;
	#answer!: () => void;
	#refuse!: (err: Error) => void;

	constructor(bytes: Uint8Array, withdraw: (token: Token) => void) {
		this.bytes = bytes;
		this.#withdraw = withdraw;
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
		this.#withdraw(this);
	}
}

/** The state behind one {@link Handle}: the tokens it presents, and every session it backs. */
class HandleState {
	/** The live session's union. */
	readonly grant = new Signal<Grant | undefined>(undefined);
	/** Created by a session for itself, so it ends with that session. */
	own = false;
	/** Set once the handle ends. */
	closed?: Error;
	/** Every token the application added that has not ended. */
	readonly tokens = new Set<Token>();
	/** Every open session built on this handle, oldest first. */
	readonly sessions: AuthSession[] = [];
	acceptor: "undecided" | "default" | RequestQueue = "undecided";

	/** The session the handle backs now: the newest one still open. */
	get live(): AuthSession | undefined {
		return this.sessions.at(-1);
	}

	/** Fail unless a new session can be built on the handle. */
	vacant() {
		if (this.closed) throw this.closed;
		if (this.sessions.some((session) => session.active)) throw new Duplicate();
	}

	attach(session: AuthSession) {
		this.vacant();
		this.sessions.push(session);
		// The successor is live now, so its predecessor's grants no longer apply.
		this.refresh();
	}

	/** A session closed: it presents nothing anymore. A handle of its own ends with it. */
	detach(session: AuthSession, err: Error) {
		const index = this.sessions.indexOf(session);
		if (index >= 0) this.sessions.splice(index, 1);
		if (this.own) this.abort(err);
		this.refresh();
	}

	/** Point the handle's grant, and each token's, at the live session. */
	refresh() {
		const live = this.live;
		const union = live?.grant.peek();
		if (!grantsEqual(union, this.grant.peek())) this.grant.set(union);
		for (const token of this.tokens) {
			const grant = live?.grantOf(token);
			if (!grantsEqual(grant, token.grant.peek())) token.grant.set(grant);
		}
	}

	add(bytes: Uint8Array): Token {
		if (this.closed) throw this.closed;
		// A draining session takes no new tokens, and a handle of its own has no successor.
		if (this.own && !this.sessions.some((session) => session.active)) {
			throw new StreamError(StreamCode.GoingAway);
		}
		const token = new Token(bytes, (token) => this.withdraw(token));
		this.tokens.add(token);
		for (const session of [...this.sessions]) session.offer(token);
		return token;
	}

	/** The application dropped the token: every session stops presenting it. */
	withdraw(token: Token) {
		if (!this.tokens.delete(token)) return;
		for (const session of this.sessions) session.withdraw(token);
		token.end(null);
	}

	/** The peer's verdict, or the handle's end: the token ends on every session. */
	verdict(token: Token, err: Error | null) {
		if (!this.tokens.delete(token)) return;
		token.end(err);
		for (const session of this.sessions) session.withdraw(token);
	}

	/** Decide who answers the peer's tokens, once: the application if it took them. */
	decide(): RequestQueue | undefined {
		if (this.acceptor === "undecided") this.acceptor = "default";
		return this.acceptor instanceof RequestQueue ? this.acceptor : undefined;
	}

	abort(err: Error) {
		if (this.closed) return;
		this.closed = err;
		for (const token of [...this.tokens]) this.verdict(token, err);
		if (this.acceptor instanceof RequestQueue) this.acceptor.close();
	}
}

const states = new WeakMap<Handle, HandleState>();

function stateOf(handle: Handle): HandleState {
	const state = states.get(handle);
	if (!state) throw new TypeError("not an auth handle");
	return state;
}

/**
 * Fail unless a session can be built on `handle`: it ended, or it backs an active session.
 *
 * @internal
 */
export function vacant(handle: Handle) {
	stateOf(handle).vacant();
}

/**
 * Why `handle` ended, or undefined while it can still back a session.
 *
 * @internal
 */
export function ended(handle: Handle): Error | undefined {
	return stateOf(handle).closed;
}

/**
 * The tokens this side presents and the grant they earn, across every session built on it.
 *
 * A session made without one creates its own, returned by `Established.auth`, which ends with
 * that session. Pass one to `connect` as `auth` instead and it outlives its sessions: each
 * presents every token the handle holds, so a {@link Credential} it returned keeps riding a route's
 * announcements across a reconnect. It backs one active session at a time, plus the ones
 * draining after a GOAWAY or a `close()`. Mirrors the Rust `moq_net::auth::Handle`.
 */
export class Handle {
	/** A handle backing no session yet, for `connect`'s `auth` option. */
	constructor() {
		states.set(this, new HandleState());
	}

	/**
	 * The union of every grant this side holds on the live session: undefined until its peer
	 * first answers a token, while no session is live, and forever on a version without AUTH.
	 */
	get grant(): Getter<Grant | undefined> {
		return stateOf(this).grant;
	}

	/**
	 * Present another token on every session of this handle, resolving once a peer answers it.
	 *
	 * With no session live it waits for the next one. A refusal that may pass, such as an
	 * unreachable auth server, is presented again after a backoff on moq-lite, and so is a token
	 * whose stream the peer drops without a verdict. Rejects with
	 * {@link Unsupported} when a session neither carries AUTH nor lets a request carry a token
	 * (moq-lite without AUTH), with the peer's refusal as an `Error.Session`, or with the
	 * handle's end. On moq-transport without AUTH it resolves at once with no grant: only a
	 * request whose route names it carries it.
	 */
	async add(token: string | Uint8Array): Promise<Credential> {
		// A copy, so the caller reusing its buffer never changes what a request carries.
		const bytes = typeof token === "string" ? new TextEncoder().encode(token) : token.slice();
		const added = stateOf(this).add(bytes);
		await added.answered;
		return added;
	}

	/**
	 * Answer every token the peer presents, instead of the default: granting what this side
	 * publishes and consumes for the connection's own credential and refusing any other.
	 *
	 * Call it as soon as connect or accept resolves, in the same task: whoever answers is
	 * decided once, right after. Throws {@link Duplicate} once that has happened.
	 *
	 * The peer's stream credit bounds how many tokens wait unanswered, since each holds its
	 * stream open until answered or withdrawn. A withdrawn token leaves the queue.
	 *
	 * @internal See {@link Request}.
	 */
	requests(): Requests {
		const state = stateOf(this);
		if (state.acceptor !== "undecided") throw new Duplicate("auth requests already taken or answered by default");
		const queue = new RequestQueue();
		// A session of its own that carries no AUTH never receives a token.
		if (state.closed || (state.own && !state.live?.negotiated)) queue.close();
		state.acceptor = queue;
		return queue;
	}

	/**
	 * End the handle with `err`: every token ends, its sessions stop presenting them, and the
	 * peer's queued tokens are refused. A handle a session created for itself ends with that
	 * session.
	 */
	abort(err: Error = new Error("auth handle closed")): void {
		stateOf(this).abort(err);
	}
}

/** A handle of a session's own, which ends with it. */
function ownHandle(): Handle {
	const handle = new Handle();
	stateOf(handle).own = true;
	return handle;
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
	 * Whether a request can carry a token (moq-transport), so a token the wire cannot present
	 * is kept for the announcements that name it.
	 */
	requests?: boolean;
	/** What the default acceptor grants the peer's connection credential. */
	peerGrant: Grant;
	/** The handle the session is built on, or undefined for one of its own that ends with it. */
	handle?: Handle;
	/** Whether the session is draining after a GOAWAY or a local close, which frees its handle for a successor. */
	draining?: () => boolean;
}

/**
 * One session's side of a {@link Handle}: presents the handle's tokens and its own
 * credential, one stream each, and answers the peer's. The wire decides only the encoding;
 * see {@link AuthWire}.
 *
 * @internal
 */
export class AuthSession {
	/** The peer sent a malformed AUTH message; the owning connection closes the session. */
	readonly violated = new Once<ProtocolViolation>();
	/** The handle this session is built on. */
	readonly handle: Handle;
	#state: HandleState;
	#wire?: AuthWire;
	#requests: boolean;
	#peerGrant: Grant;
	#draining: () => boolean;
	#ended = new Signal(0);

	#union = new Signal<Grant | undefined>(undefined);
	// The union until the session ends, which is not a revocation: what the session covered
	// ends with the session's own error, not as unauthorized.
	#scope = new Signal<Grant | undefined>(undefined);
	// The peer replied to some token, so the union is known even when empty.
	#replied = false;
	#presented = new Set<Presented>();
	// The handle's tokens as this session presents them, so a request carries only those.
	#byToken = new Map<Token, Presented>();
	#setupPending = new Signal(0);
	#pending = new Signal(0);
	#closed = false;

	// Whoever answers the peer's tokens is decided once, after the task that established
	// the session: an app that calls requests() as soon as connect/accept resolves always
	// wins, however quickly the peer's first token arrives.
	#decided = yieldNow();

	/** Throws {@link Duplicate} while `handle` backs another active session, or the handle's end. */
	constructor({ wire, requests, peerGrant, handle, draining }: AuthSessionProps) {
		this.handle = handle ?? ownHandle();
		this.#state = stateOf(this.handle);
		this.#wire = wire;
		this.#requests = requests ?? false;
		this.#peerGrant = peerGrant;
		this.#draining = draining ?? (() => false);
		this.#state.attach(this);

		// Present the connection's own credential right away, so both sides learn their
		// grant without waiting on the app.
		if (wire) this.#present(wire, new Presented(new Uint8Array(), true));
		for (const token of [...this.#state.tokens]) this.offer(token);
	}

	/** Whether the session still takes new work: open, neither closing nor past a GOAWAY. */
	get active(): boolean {
		return !this.#closed && !this.#draining();
	}

	/** The union of this session's grants, undefined until its peer first answers. */
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

	/** The handle's `token` grant on this session. */
	grantOf(token: Token): Grant | undefined {
		return this.#byToken.get(token)?.grant;
	}

	/** Whether this session still presents the handle's `token`. */
	presents(token: Token): boolean {
		const presented = this.#byToken.get(token);
		return presented !== undefined && !presented.ended && !presented.withdrawn;
	}

	/**
	 * Present one of the handle's tokens on this session. A draining session takes no new
	 * tokens, so its verdict never ends one its successor carries.
	 */
	offer(token: Token) {
		if (!this.active || this.#byToken.has(token)) return;
		if (!this.#wire && !this.#requests) {
			this.#state.verdict(token, new Unsupported());
			return;
		}
		const presented = new Presented(token.bytes, false, token);
		this.#byToken.set(token, presented);
		if (this.#wire) {
			this.#present(this.#wire, presented);
			return;
		}
		// Only a request carries it, and the peer judges it there: no grant to wait for.
		presented.isAnswered = true;
		this.#presented.add(presented);
		token.answer();
	}

	/** Stop presenting one of the handle's tokens: withdrawn, or ended for good elsewhere. */
	withdraw(token: Token) {
		const presented = this.#byToken.get(token);
		if (!presented || presented.withdrawn) return;
		presented.withdrawn = true;
		presented.grant = undefined;
		// The stream's loop notices and ends the presentation; one still opening checks on
		// arrival, and one waiting to retry wakes. One without a stream to come (carried only by
		// requests) ends here.
		if (presented.stream) presented.stream.abort(cancel());
		else if (presented.retry) presented.retry.abort();
		else if (!this.#wire) {
			presented.ended = true;
			this.#presented.delete(presented);
			this.#byToken.delete(token);
			this.#ended.update((n) => n + 1);
		}
		this.#recompute();
	}

	/**
	 * The bytes a request carries for `token`: undefined for none, for one another handle
	 * issued, or for one that ended here.
	 */
	carried(token: Credential | undefined): Uint8Array | undefined {
		if (!this.#requests || !(token instanceof Token)) return undefined;
		const presented = this.#byToken.get(token);
		if (!presented || presented.ended || presented.withdrawn) return undefined;
		return presented.token;
	}

	/** Bumps whenever one of this session's tokens ends, so a request carrying it drops it. */
	get ended(): Getter<number> {
		return this.#ended;
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
		const acceptor = this.#state.decide();

		const issued = new IssuedGrant(stream, wire);
		if (acceptor) {
			const request = new PeerRequest(token, issued);
			acceptor.push(request);
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

	/**
	 * End the session: fail every pending token here, leaving the handle's tokens for the next
	 * session. A handle of the session's own ends with it, closing the requests.
	 */
	close(err: Error = new Error("session closed")) {
		if (this.#closed) return;
		this.#closed = true;
		this.#pending.set(0);
		this.#setupPending.set(0);
		for (const presented of this.#presented) {
			presented.ended = true;
			presented.isAnswered = true;
			presented.grant = undefined;
			presented.retry?.abort();
		}
		this.#presented.clear();
		this.#byToken.clear();
		this.#recompute();
		this.#state.detach(this, err);
	}

	#present(wire: AuthWire, presented: Presented) {
		this.#presented.add(presented);
		if (presented.setup) this.#setupPending.update((n) => n + 1);
		if (presented.token.byteLength > 0) this.#pending.update((n) => n + 1);
		void this.#run(wire, presented);
	}

	async #run(wire: AuthWire, presented: Presented) {
		let result: Error | null = null;
		let closed = false;
		// The peer refused the last attempt with a reply, rather than losing its stream.
		let refused = false;
		// The peer granted the token, so a clean close ends the grant. A retry clears it.
		let granted = false;
		// The peer replied to an earlier attempt, so it knows AUTH.
		let replied = false;
		let delay: Milli | undefined;
		for (;;) {
			refused = false;
			// Whether the peer answered this token, so a close is not the peer predating AUTH:
			// a grant on this stream, or a reply to an earlier one that this retry follows.
			let answered = replied;
			try {
				const stream = await wire.present(presented.token);
				presented.stream = stream;
				if (presented.withdrawn || presented.ended) throw cancel();

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
						result = answered ? null : new Unsupported();
						stream.close();
						break;
					}
					if ("grant" in reply) {
						const expires = reply.grant.expires === undefined ? undefined : wall() + reply.grant.expires;
						presented.grant = { publish: reply.grant.publish, subscribe: reply.grant.subscribe, expires };
						answered = granted = replied = true;
						delay = undefined;
						this.#replied = true;
						this.#answered(presented);
						this.#recompute();
						continue;
					}
					console.warn("auth token refused", reply.refused);
					// A refused setup token leaves an empty union, not an unknown (unrestricted)
					// one. A grant the acceptor could not tell, or never judged, is unknown, not empty.
					const goingAway =
						reply.refused instanceof StreamError && reply.refused.code === StreamCode.GoingAway;
					if (!goingAway && !(reply.refused instanceof Unsupported)) this.#replied = true;
					result = reply.refused;
					replied = true;
					// A peer going away judged nothing, so the successor presents the token instead.
					refused = !goingAway;
					stream.close();
					break;
				}
			} catch (err: unknown) {
				// End both halves: a peer that reset only its own leaves ours open, and a retry that
				// abandoned it would hold the stream's credit until the session ends.
				presented.stream?.abort(cancel());
				closed = transportClosed(err);
				if (presented.withdrawn || presented.ended) {
					result = null;
				} else if (!answered && err instanceof StreamError) {
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
			presented.stream = undefined;

			// Present the token again after a backoff while it is still wanted on an active
			// session: after a refusal that may pass, such as an unreachable auth server (only
			// moq-lite's AUTH_ERROR carries such a code), or, on a shared handle, after its stream
			// was lost without a verdict, so the session never runs on without it. A handle of the
			// session's own ends a lost token instead, which tells the application.
			if (
				isVerdict(refused, granted, result) ||
				closed ||
				!this.active ||
				presented.withdrawn ||
				presented.ended ||
				result instanceof ProtocolViolation ||
				(!refused && this.#state.own)
			)
				break;
			presented.grant = undefined;
			granted = false;
			// Unanswered again, as on its first presentation: a request or publication the token
			// might cover waits for the retry's verdict rather than being judged without it.
			this.#unsettle(presented);
			this.#recompute();
			delay = delay === undefined ? RETRY_INITIAL : Milli(Math.min(delay * 2, RETRY_MAX));
			presented.retry = new AbortController();
			await sleep(Nano.fromMilli(delay), { signal: presented.retry.signal }).catch(() => void 0);
			presented.retry = undefined;
			if (this.#closed || presented.withdrawn || presented.ended) {
				refused = false;
				result = null;
				break;
			}
			// After a GOAWAY the peer must not see new streams: the successor presents it.
			if (this.#draining()) {
				refused = false;
				result = new StreamError(StreamCode.GoingAway);
				break;
			}
		}

		// Already ended by the session's close.
		if (presented.ended) return;
		presented.ended = true;
		presented.grant = undefined;
		// A token that ends unanswered counts as answered for enforcement: its refusal is
		// the reply.
		this.#settle(presented);
		this.#presented.delete(presented);
		const owner = presented.owner;
		if (owner && this.#byToken.get(owner) === presented) this.#byToken.delete(owner);
		this.#ended.update((n) => n + 1);
		// Only the peer's verdict ends one of the handle's tokens on every session: a refusal,
		// Unsupported, or a clean close after a grant. Anything else leaves it for the next
		// session, since an active one presents it again instead.
		// A draining session's verdict is stale while an active session still presents the
		// token: that session's own answer decides, even mid-retry with no grant right now.
		const superseded =
			!this.active &&
			owner !== undefined &&
			this.#state.sessions.some((session) => session !== this && session.active && session.presents(owner));
		// A handle of the session's own has no next session to present it, so any end is final.
		const settled =
			!this.#closed &&
			!presented.withdrawn &&
			!superseded &&
			(this.#state.own || isVerdict(refused, granted, result));
		if (owner && settled) this.#state.verdict(owner, result);
		// Closed before the union shrinks, so the scope never reads it as a revocation.
		if (closed) this.close();
		this.#recompute();
	}

	/** The peer granted `presented`: it counts as answered, and so does the handle's token. */
	#answered(presented: Presented) {
		this.#settle(presented);
		presented.owner?.answer();
	}

	/** Stop counting `presented` as unanswered, for enforcement and the setup wait. */
	#settle(presented: Presented) {
		if (presented.isAnswered) return;
		presented.isAnswered = true;
		if (presented.setup) this.#setupPending.update((n) => n - 1);
		if (presented.token.byteLength > 0 && !this.#closed) this.#pending.update((n) => n - 1);
	}

	/** Count `presented` as unanswered again, as a retry presents it anew. */
	#unsettle(presented: Presented) {
		if (!presented.isAnswered) return;
		presented.isAnswered = false;
		if (presented.setup) this.#setupPending.update((n) => n + 1);
		if (presented.token.byteLength > 0) this.#pending.update((n) => n + 1);
	}

	#recompute() {
		const granted: Grant[] = [];
		for (const presented of this.#presented) {
			if (presented.grant) granted.push(presented.grant);
		}
		// Undefined until the first reply; an empty union afterwards grants nothing.
		if (granted.length > 0 || this.#replied) {
			const next = union(granted);
			if (!grantsEqual(next, this.#union.peek())) this.#union.set(next);
			if (!this.#closed && !grantsEqual(next, this.#scope.peek())) this.#scope.set(next);
		}
		this.#state.refresh();
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
	// Ends an authorization wait with the session, without adding a reaction on `closed` per wait.
	const ended = new AbortController();
	void closed.then(() => ended.abort());

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
					try {
						await auth?.ready(path, "publish", { signal: ended.signal });
					} catch (err) {
						dispose();
						if (ended.signal.aborted) return;
						throw err;
					}
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

		// `race` drops its reaction on `closed` with the rest, which lives for the session.
		const why = await race([woke, closed]);
		dispose();
		if (why === "closed") return;
	}
}
