/**
 * Point-to-point `track` sessions between server and client endpoints: each half is a
 * track, and each message a group.
 *
 * An endpoint's first dial announces its hidden client broadcast, `<prefix>/.client/<id>`,
 * and its first accept its hidden server broadcast, `<prefix>/.server/<id>`, so ordinary
 * grants decide which roles it may take. Sessions use the existing MoQ wire and need lite-07 (currently an explicit
 * `moq-lite-07-wip` offer) or moq-transport on every hop. While the endpoint's origin
 * has a moq-lite client session before 07, dials and accepts fail as unsupported.
 * Their track names are secrets and are never listed. See the socket concept page.
 *
 * @module
 */
import { type Dispose, type GetPromise, type Getter, Once, Signal } from "@moq/signals";
import { NotFound, StreamCode, StreamError } from "./error.ts";
import { hooks } from "./internal.ts";
import type * as Origin from "./origin.ts";
import * as Path from "./path.ts";
import type * as Track from "./track.ts";
import { untilAborted } from "./util/abort.ts";
import { isUnrequested as abandoned } from "./util/demand.ts";
import { fromBytes } from "./util/hex.ts";
import { wireOf } from "./wire.ts";

/** The protocol segment of a session's track name. */
const PROTOCOL = "track";
/** A server accepts dials, and a client dials. */
type Role = "server" | "client";
const SEGMENT: Record<Role, string> = { server: ".server", client: ".client" };
// Every message, oldest first, from group 0, so a burst written before the answer is kept.
const reliable = (): Track.Subscription => ({
	order: "ascending",
	delay: { max: null },
	ranges: [{ start: { absolute: 0n } }],
});
const cancelled = () => new StreamError(StreamCode.Cancel, { message: "socket operation cancelled" });
const ended = () => new StreamError(StreamCode.SessionClosed, { message: "socket endpoint closed" });
// Unsupported has no registered stream code, as in Rust: it maps to Internal.
const unreachable = () => new Error("socket endpoint is unreachable behind a moq-lite hop before 07");

function segment(id: string): void {
	if (!id || id.includes("/") || id.includes("\\")) throw new TypeError("socket id must be one plain segment");
}

function endpointPath(prefix: Path.Valid, role: Role, id: string): Path.Valid {
	segment(id);
	return Path.encode(Path.from(prefix, SEGMENT[role], id));
}

/**
 * Whether `path` is an endpoint's broadcast, which only routes sessions and carries no
 * media, so a worker that acts on every announced broadcast can skip it.
 */
export function isEndpoint(path: Path.Valid): boolean {
	const parent = path.split("/").at(-2);
	return Object.values(SEGMENT).some((role) => parent === role);
}

function parse(name: string): { protocol: string; dialer: string; acceptor: string; session: string } | undefined {
	const parts = name.split("/");
	if (parts.length !== 4 || parts.some((part) => !part || part.includes("\\"))) return;
	const [protocol, dialer, acceptor, session] = parts;
	return { protocol, dialer, acceptor, session };
}

// A cancellable signal wait removes its subscriptions even if the value never changes.
function waitFor<T>(value: Getter<T>, ready: (value: T) => boolean, signal: AbortSignal): Promise<T> {
	return new Promise((resolve, reject) => {
		let dispose: Dispose = () => {};
		const finish = () => {
			dispose();
			signal.removeEventListener("abort", abort);
		};
		const abort = () => {
			finish();
			reject(signal.reason);
		};
		const check = () => {
			const current = value.peek();
			if (!ready(current)) return;
			finish();
			resolve(current);
		};
		if (signal.aborted) return abort();
		dispose = value.subscribe(check);
		signal.addEventListener("abort", abort, { once: true });
		check();
	});
}

/** Cancellation for a pending dial or accept; aborting after it resolves has no effect. */
export interface Options {
	/** Abandon this operation with the signal's reason. */
	signal?: AbortSignal;
}

let makeTrack: (parts: Parts) => TrackStream;
let makeIncoming: (endpoint: Endpoint, request: Track.Request) => Incoming;
let accept: (
	endpoint: Endpoint,
	request: Track.Request,
	info: Track.InfoInit,
	options?: Options,
) => Promise<TrackStream>;

/** A track session whose ascending reader skips unavailable groups, with no default publisher max age. */
export class TrackStream {
	/** This side's track; write groups as independent messages. */
	readonly producer: Track.Producer;
	/** The peer's subscribed track; ordered() skips groups that are refused, dropped, or evicted. */
	readonly consumer: Track.Subscriber;
	/** The peer endpoint's id, verified by accepting the dial. */
	readonly peer: string;
	/** The fresh, secret session id chosen by the dialer. */
	readonly session: string;

	private constructor(parts: Parts) {
		this.producer = parts.producer;
		this.consumer = parts.consumer;
		this.peer = parts.peer;
		this.session = parts.session;
	}
	static {
		makeTrack = (parts) => new TrackStream(parts);
	}
}

interface Parts {
	producer: Track.Producer;
	consumer: Track.Subscriber;
	peer: string;
	session: string;
}

/** An unverified incoming request to accept (checking its original dial is still open) or reject. */
export class Incoming {
	/** The id the dialer claims; accept verifies that endpoint really dialed this session. */
	readonly peer: string;
	/** The claimed session id; only accept verifies that its original dial is still open. */
	readonly session: string;
	#endpoint: Endpoint;
	#request?: Track.Request;

	private constructor(endpoint: Endpoint, request: Track.Request) {
		const name = parse(request.name);
		if (!name) throw new Error("invalid incoming socket name");
		this.peer = name.dialer;
		this.session = name.session;
		this.#endpoint = endpoint;
		this.#request = request;
	}
	static {
		makeIncoming = (endpoint, request) => new Incoming(endpoint, request);
	}

	/** Accept while the original dial stays open; omitted info keeps every message with no publisher max age. */
	accept(info: Track.InfoInit = { maxAge: null }, options?: Options): Promise<TrackStream> {
		const request = this.#request;
		this.#request = undefined;
		if (!request) return Promise.reject(cancelled());
		return accept(this.#endpoint, request, info, options);
	}

	/** Refuse this dial as not found; idempotent. */
	reject(): void {
		this.#request?.reject(new NotFound("socket session"));
		this.#request = undefined;
	}
}

class Queue {
	dials = new Signal(new Map<Track.Request, Dispose>());
	close(): void {
		for (const [request, dispose] of this.dials.peek()) {
			dispose();
			request.reject(ended());
		}
		this.dials.mutate((dials) => dials.clear());
	}
}

/**
 * A server, a client, or both: dials servers under the same prefix, and accepts their dials.
 *
 * Its first dial announces its client broadcast, which holds its half of each session it dials,
 * and its first accept announces its server broadcast, where peers dial it. A pure client never
 * announces a server broadcast, and until the first accept a dial to it waits as for a peer not
 * online yet.
 *
 * Every hop between two endpoints must carry hidden broadcasts: moq-lite-07 or moq-transport.
 * While the origin has a moq-lite client session before 07, whose peer never hears this
 * endpoint, dials and accepts fail as unsupported. Nothing tells a dialer that its peer sits
 * behind such a hop, so a dial to it waits as for a peer not announced yet.
 */
export class Endpoint {
	/** This endpoint's id, a single plain segment. */
	readonly id: string;
	/** The shared prefix containing the endpoints. */
	readonly prefix: Path.Valid;
	/** Settles when the endpoint closes. */
	get closed(): GetPromise<Error | null> {
		return this.#closed;
	}
	#closed = new Once<Error | null>();
	#abort = new AbortController();
	#origin: Origin.Producer;
	#routes: Partial<Record<Role, Origin.Route>> = {};
	#queue = new Queue();
	#sessions = new Set<Session>();
	#unclose: Dispose[] = [];
	#withheld: Getter<number>;

	/** An endpoint on origin, which must also reach its peers; nothing is announced until it dials or accepts. */
	constructor(origin: Origin.Producer, prefix: Path.Valid, id: string) {
		this.id = id;
		this.prefix = Path.from(prefix);
		for (const role of ["server", "client"] as const) endpointPath(this.prefix, role, id);
		this.#origin = origin;
		this.#withheld = hooks.hiddenWithheld(origin);
		// Before the first dial or accept no route watches the origin, so watch its close here.
		this.#unclose.push(origin.closed.subscribe(() => this.close()));
		if (origin.closed.peek() !== undefined) this.close();
	}

	// This endpoint's broadcast in role, announced on first use.
	#route(role: Role): Origin.Route {
		const existing = this.#routes[role];
		if (existing) return existing;
		const route = this.#origin.route(endpointPath(this.prefix, role, this.id));
		this.#routes[role] = route;
		this.#unclose.push(hooks.onClosed(route.broadcast, () => this.close()));
		return route;
	}

	static {
		accept = (endpoint, request, info, options) => endpoint.#accept(request, info, options);
	}

	/**
	 * Dial the server peer in a track session, waiting for its announcement and acceptance until
	 * cancelled; omitted info keeps every message with no publisher max age.
	 */
	dialTrack(peer: string, info: Track.InfoInit = { maxAge: null }, options?: Options): Promise<TrackStream> {
		return this.#dial(peer, info, options);
	}
	/** Take the next track dial; the first call starts queueing dials until close, and announces the server. */
	async acceptTrack(options?: Options): Promise<Incoming> {
		const abort = new AbortController();
		const signal = AbortSignal.any([this.#signal(options), abort.signal]);
		const unwatch = this.#reachable(abort);
		try {
			signal.throwIfAborted();
			const queue = this.#queue;
			if (!this.#routes.server) void this.#serve(this.#route("server"));
			for (;;) {
				await waitFor(queue.dials, (dials) => dials.size > 0, signal);
				signal.throwIfAborted();
				const first = queue.dials.peek().entries().next().value;
				if (!first) continue;
				const [request, dispose] = first;
				dispose();
				queue.dials.mutate((dials) => dials.delete(request));
				if (!abandoned(request)) return makeIncoming(this, request);
				request.reject(cancelled());
			}
		} finally {
			unwatch();
		}
	}

	// Abort with unsupported while a session withholds this endpoint from its peer, until disposed.
	#reachable(abort: AbortController): Dispose {
		const check = () => {
			if (this.#withheld.peek() > 0) abort.abort(unreachable());
		};
		const unwatch = this.#withheld.subscribe(check);
		check();
		return unwatch;
	}

	async #serve(route: Origin.Route): Promise<void> {
		try {
			for (;;) {
				const request = await wireOf(route.broadcast).requested();
				if (!request) return;
				const name = parse(request.name);
				if (!name || name.acceptor !== this.id || name.dialer === this.id) {
					request.reject(new NotFound("socket session"));
					continue;
				}
				const queue = this.#queue;
				if (name.protocol !== PROTOCOL) {
					// Unsupported has no registered stream code, as in Rust: it maps to Internal.
					request.reject(new Error("socket protocol is unsupported"));
					continue;
				}
				const demand = request.demand();
				const prune = () => {
					if (!abandoned(request)) return;
					queue.dials.peek().get(request)?.();
					queue.dials.mutate((dials) => dials.delete(request));
					request.reject(cancelled());
				};
				const stop = hooks.requestWanted(request).subscribe(prune);
				const release = hooks.demandReleased(demand).subscribe(prune);
				queue.dials.mutate((dials) =>
					dials.set(request, () => {
						stop();
						release();
					}),
				);
				prune();
			}
		} catch (error) {
			this.#end(error instanceof Error ? error : new Error(String(error)));
		}
	}

	#signal(options?: Options): AbortSignal {
		return options?.signal ? AbortSignal.any([this.#abort.signal, options.signal]) : this.#abort.signal;
	}

	// Subscribe to the session's half in the peer's broadcast, its server or client one by role.
	async #remote(
		role: Role,
		peer: string,
		name: string,
		signal: AbortSignal,
		hold: (following: Origin.Following) => void,
	): Promise<{ consumer: Track.Subscriber; peer: GetPromise<Error | null> }> {
		const following = this.#origin.follow(endpointPath(this.prefix, role, peer));
		hold(following);
		const abort = new AbortController();
		const stop = following.closed.subscribe(() => abort.abort(following.closed.peek() ?? ended()));
		if (following.closed.peek() !== undefined) abort.abort(following.closed.peek() ?? ended());
		const pending = AbortSignal.any([signal, abort.signal]);
		try {
			const broadcast = await waitFor(following.active, (active) => active !== undefined, pending);
			if (!broadcast) throw ended();
			return {
				consumer: broadcast.track(name).subscribe(reliable()),
				peer: broadcast.closed,
			};
		} finally {
			stop();
		}
	}

	async #dial(peer: string, info: Track.InfoInit, options?: Options): Promise<TrackStream> {
		segment(peer);
		if (peer === this.id) throw new Error("dialing the same socket endpoint is unsupported");
		const abort = new AbortController();
		const signal = AbortSignal.any([this.#signal(options), abort.signal]);
		signal.throwIfAborted();
		const session = fromBytes(crypto.getRandomValues(new Uint8Array(16)));
		const name = `${PROTOCOL}/${this.id}/${peer}/${session}`;
		const answer = hooks.reserveUnlistedTrack(this.#route("client").broadcast, name);
		let following: Origin.Following | undefined;
		let consumer: Track.Subscriber | undefined;
		let producer: Track.Producer | undefined;
		let stop: Dispose = () => {};
		const unreached = this.#reachable(abort);
		try {
			const remote = await this.#remote("server", peer, name, signal, (value) => {
				following = value;
			});
			consumer = remote.consumer;
			stop = hooks.watchSubscriberError(consumer, (error) => abort.abort(error));
			const serving = waitFor(hooks.requestWanted(answer), (wanted) => wanted, signal).then(() => {
				producer = answer.accept(info);
			});
			await untilAborted(Promise.all([serving, consumer.info()]), signal);
			signal.throwIfAborted();
			if (!producer || !following) throw cancelled();
			return this.#open({ producer, consumer, peer, session }, following, remote.peer);
		} catch (error) {
			answer.reject(error instanceof Error ? error : cancelled());
			consumer?.close();
			following?.close();
			throw error;
		} finally {
			stop();
			unreached();
			abort.abort(cancelled());
		}
	}

	async #accept(request: Track.Request, info: Track.InfoInit, options?: Options): Promise<TrackStream> {
		const name = parse(request.name);
		if (!name) throw cancelled();
		const abort = new AbortController();
		const signal = AbortSignal.any([this.#signal(options), abort.signal]);
		const demand = request.demand();
		const check = () => {
			if (abandoned(request)) abort.abort(cancelled());
		};
		const stop = hooks.requestWanted(request).subscribe(check);
		const release = hooks.demandReleased(demand).subscribe(check);
		const unreached = this.#reachable(abort);
		let following: Origin.Following | undefined;
		let consumer: Track.Subscriber | undefined;
		let stopError: Dispose = () => {};
		try {
			check();
			signal.throwIfAborted();
			const remote = await this.#remote("client", name.dialer, request.name, signal, (value) => {
				following = value;
			});
			consumer = remote.consumer;
			stopError = hooks.watchSubscriberError(consumer, (error) => abort.abort(error));
			await untilAborted(consumer.info(), signal);
			signal.throwIfAborted();
			if (!following) throw cancelled();
			hooks.keepRequest(request);
			return this.#open(
				{ producer: request.accept(info), consumer, peer: name.dialer, session: name.session },
				following,
				remote.peer,
			);
		} catch (error) {
			request.reject(error instanceof Error ? error : cancelled());
			consumer?.close();
			following?.close();
			throw error;
		} finally {
			stop();
			release();
			unreached();
			stopError();
		}
	}

	#open(parts: Parts, following: Origin.Following, peer: GetPromise<Error | null>): TrackStream {
		this.#abort.signal.throwIfAborted();
		if (following.active.peek()?.closed !== peer) throw ended();
		const session = new Session(parts, following, peer, () => this.#sessions.delete(session));
		this.#sessions.add(session);
		try {
			return session.track();
		} catch (error) {
			session.reset(error instanceof Error ? error : cancelled());
			throw error;
		}
	}

	/** Reset every held session, cancel pending operations, and retract the endpoint's broadcasts; idempotent. */
	close(): void {
		this.#end(null);
	}

	#end(error: Error | null): void {
		if (this.#closed.peek() !== undefined) return;
		this.#closed.set(error);
		for (const unclose of this.#unclose) unclose();
		this.#abort.abort(error ?? ended());
		this.#queue.close();
		for (const session of this.#sessions) session.reset(ended());
		for (const route of Object.values(this.#routes)) route.broadcast.close();
	}
}

// Owns the routing hold and monitors both halves, releasing the session once both end.
class Session {
	#parts: Parts;
	#following: Origin.Following;
	#peer: GetPromise<Error | null>;
	#done: Dispose;
	#abort = new AbortController();
	#stops: Dispose[] = [];
	#sent = false;
	#received = false;
	constructor(parts: Parts, following: Origin.Following, peer: GetPromise<Error | null>, done: Dispose) {
		this.#parts = parts;
		this.#following = following;
		this.#peer = peer;
		this.#done = done;
	}

	#hold(stop: Dispose): void {
		if (this.#abort.signal.aborted) stop();
		else this.#stops.push(stop);
	}

	#monitor(): void {
		// A socket reset ends the session even when a generic routed reader would recover.
		this.#hold(hooks.watchSubscriberError(this.#parts.consumer, (error) => this.reset(error)));
		const check = () => {
			if (this.#following.active.peek()?.closed !== this.#peer) this.reset(ended());
		};
		this.#hold(this.#following.active.subscribe(check));
		check();
	}

	#watch(value: GetPromise<Error | null>, done: () => void): void {
		const check = (error: Error | null | undefined) => {
			if (error === undefined) return;
			if (error) this.reset(error);
			else {
				done();
				this.#complete();
			}
		};
		this.#hold(value.subscribe(check));
		check(value.peek());
	}

	track(): TrackStream {
		this.#monitor();
		this.#watch(this.#parts.producer.closed, () => {
			this.#sent = true;
		});
		this.#watch(this.#parts.consumer.closed, () => {
			this.#received = true;
		});
		return makeTrack(this.#parts);
	}

	#complete(): void {
		if (this.#sent && this.#received) this.#close(false);
	}

	reset(error: Error): void {
		if (this.#abort.signal.aborted) return;
		this.#parts.producer.abort(error);
		this.#parts.consumer.close(error);
		this.#close(true);
	}

	#close(reset: boolean): void {
		if (this.#abort.signal.aborted) return;
		this.#abort.abort();
		for (const stop of this.#stops) stop();
		this.#stops.length = 0;
		// A settled end retains its cache for existing readers, while releasing the
		// broadcast's unlisted reservation instead of accumulating completed sessions.
		this.#parts.producer.close();
		if (reset) this.#parts.consumer.close();
		this.#following.close();
		this.#done();
	}
}
