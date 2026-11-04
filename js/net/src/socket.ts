/**
 * Point-to-point sessions over one hidden broadcast per endpoint.
 *
 * Sessions use the existing MoQ wire and need lite-07 (currently an explicit
 * `moq-lite-07-wip` offer) or moq-transport on every hop.
 * Their track names are secrets and are never listed. See the socket concept page.
 *
 * @module
 */
import { type Dispose, type GetPromise, type Getter, Once, Signal } from "@moq/signals";
import { NotFound, ProtocolViolation, StreamCode, StreamError } from "./error.ts";
import * as Group from "./group.ts";
import { hooks, SOCKET } from "./internal.ts";
import type * as Origin from "./origin.ts";
import * as Path from "./path.ts";
import type * as Track from "./track.ts";
import { untilAborted } from "./util/abort.ts";
import { wireOf } from "./wire.ts";

type Protocol = "tcp" | "track";
type Stream = TcpStream | TrackStream;
const reliable: Track.Subscription = {
	order: "ascending",
	delay: { max: null },
	ranges: [{ start: { absolute: 0n } }],
};
const cancelled = () => new StreamError(StreamCode.Cancel, { message: "socket operation cancelled" });
const ended = () => new StreamError(StreamCode.SessionClosed, { message: "socket endpoint closed" });

function abandoned(request: Track.Request): boolean {
	return !hooks.requestWanted(request).peek() || hooks.demandReleased(request.demand()).peek() !== undefined;
}

function segment(id: string): void {
	if (!id || id.includes("/") || id.includes("\\")) throw new TypeError("socket id must be one plain segment");
}

function endpointPath(prefix: Path.Valid, id: string): Path.Valid {
	segment(id);
	return Path.encode(Path.from(prefix, SOCKET, id));
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

let makeTcp: (parts: Parts, producer: Group.Producer, consumer: Group.Consumer) => TcpStream;
let makeTrack: (parts: Parts) => TrackStream;
let makeIncoming: <S extends Stream>(endpoint: Endpoint, protocol: Protocol, request: Track.Request) => Incoming<S>;
let accept: (
	endpoint: Endpoint,
	protocol: Protocol,
	request: Track.Request,
	info: Track.InfoInit,
	options?: Options,
) => Promise<Stream>;

/** A TCP session's one group each way; finish is FIN, and abort is RST. */
export class TcpStream {
	/** This side's writes, as frames in group zero. */
	readonly producer: Group.Producer;
	/** The peer's writes in order; a reader falling behind the unpaced JS tail fails with eviction. */
	readonly consumer: Group.Consumer;
	/** The peer endpoint's id. */
	readonly peer: string;
	/** The fresh, secret session id chosen by the dialer. */
	readonly session: string;

	private constructor(parts: Parts, producer: Group.Producer, consumer: Group.Consumer) {
		this.producer = producer;
		this.consumer = consumer;
		this.peer = parts.peer;
		this.session = parts.session;
	}
	static {
		makeTcp = (parts, producer, consumer) => new TcpStream(parts, producer, consumer);
	}
}

/** A track session with an ascending, reliable subscription and no default publisher max age. */
export class TrackStream {
	/** This side's track; write groups as independent messages. */
	readonly producer: Track.Producer;
	/** The peer's subscribed track; update its subscription or read through ordered(). */
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
export class Incoming<S extends TcpStream | TrackStream> {
	/** The id the dialer claims; accept verifies that endpoint really dialed this session. */
	readonly peer: string;
	/** The claimed session id; only accept verifies that its original dial is still open. */
	readonly session: string;
	#endpoint: Endpoint;
	#protocol: Protocol;
	#request?: Track.Request;

	private constructor(endpoint: Endpoint, protocol: Protocol, request: Track.Request) {
		const name = parse(request.name);
		if (!name) throw new Error("invalid incoming socket name");
		this.peer = name.dialer;
		this.session = name.session;
		this.#endpoint = endpoint;
		this.#protocol = protocol;
		this.#request = request;
	}
	static {
		makeIncoming = (endpoint, protocol, request) => new Incoming(endpoint, protocol, request);
	}

	/** Accept while the original dial stays open; track accepts may describe their outgoing half. */
	accept(
		...args: S extends TcpStream ? [options?: Options] : [info?: Track.InfoInit, options?: Options]
	): Promise<S> {
		const request = this.#request;
		this.#request = undefined;
		if (!request) return Promise.reject(cancelled());
		const [info, options] =
			this.#protocol === "tcp"
				? [{}, args[0] as Options | undefined]
				: [(args[0] as Track.InfoInit) ?? { maxAge: null }, args[1]];
		return accept(this.#endpoint, this.#protocol, request, info, options) as Promise<S>;
	}

	/** Refuse this dial as not found; idempotent. */
	reject(): void {
		this.#request?.reject(new NotFound("socket session"));
		this.#request = undefined;
	}
}

class Queue {
	listening = false;
	dials = new Signal(new Map<Track.Request, Dispose>());
	close(): void {
		for (const [request, dispose] of this.dials.peek()) {
			dispose();
			request.reject(ended());
		}
		this.dials.mutate((dials) => dials.clear());
	}
}

/** Announces one endpoint, dials peers under the same prefix, and accepts each protocol separately. */
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
	#publication: Origin.Publication;
	#queues = { tcp: new Queue(), track: new Queue() };
	#serving = false;
	#sessions = new Set<Session>();
	#unclose: Dispose;

	/** Publish this endpoint on origin, which must also reach its peers. */
	constructor(origin: Origin.Producer, prefix: Path.Valid, id: string) {
		this.id = id;
		this.prefix = Path.from(prefix);
		this.#origin = origin;
		this.#publication = origin.createBroadcast(endpointPath(this.prefix, id));
		this.#unclose = hooks.onClosed(this.#publication.broadcast, () => this.close());
		this.#publication.announce();
	}

	static {
		accept = (endpoint, protocol, request, info, options) => endpoint.#accept(protocol, request, info, options);
	}

	/** Dial a TCP session; waits for the peer's announcement and acceptance until cancelled. */
	dialTcp(peer: string, options?: Options): Promise<TcpStream> {
		return this.#dial("tcp", peer, {}, options) as Promise<TcpStream>;
	}
	/** Dial a track session; omitted info keeps every message with no publisher max age. */
	dialTrack(peer: string, info: Track.InfoInit = { maxAge: null }, options?: Options): Promise<TrackStream> {
		return this.#dial("track", peer, info, options) as Promise<TrackStream>;
	}
	/** Take the next TCP dial; the first call starts queueing TCP dials until close. */
	acceptTcp(options?: Options): Promise<Incoming<TcpStream>> {
		return this.#next<TcpStream>("tcp", options);
	}
	/** Take the next track dial; its queue is independent of TCP's. */
	acceptTrack(options?: Options): Promise<Incoming<TrackStream>> {
		return this.#next<TrackStream>("track", options);
	}

	async #next<S extends Stream>(protocol: Protocol, options?: Options): Promise<Incoming<S>> {
		const signal = this.#signal(options);
		signal.throwIfAborted();
		const queue = this.#queues[protocol];
		queue.listening = true;
		if (!this.#serving) {
			this.#serving = true;
			void this.#serve();
		}
		for (;;) {
			await waitFor(queue.dials, (dials) => dials.size > 0, signal);
			signal.throwIfAborted();
			const first = queue.dials.peek().entries().next().value;
			if (!first) continue;
			const [request, dispose] = first;
			dispose();
			queue.dials.mutate((dials) => dials.delete(request));
			if (!abandoned(request)) return makeIncoming<S>(this, protocol, request);
			request.reject(cancelled());
		}
	}

	async #serve(): Promise<void> {
		try {
			for (;;) {
				const request = await wireOf(this.#publication.broadcast).requested();
				if (!request) return;
				const name = parse(request.name);
				if (!name || name.acceptor !== this.id || name.dialer === this.id) {
					request.reject(new NotFound("socket session"));
					continue;
				}
				if ((name.protocol !== "tcp" && name.protocol !== "track") || !this.#queues[name.protocol].listening) {
					// Unsupported has no registered stream code, as in Rust: it maps to Internal.
					request.reject(new Error("socket protocol is unsupported"));
					continue;
				}
				const queue = this.#queues[name.protocol];
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

	async #remote(
		peer: string,
		name: string,
		signal: AbortSignal,
		hold: (following: Origin.Following) => void,
	): Promise<{ consumer: Track.Subscriber; peer: GetPromise<Error | null> }> {
		const following = this.#origin.follow(endpointPath(this.prefix, peer));
		hold(following);
		const abort = new AbortController();
		const stop = following.closed.subscribe(() => abort.abort(following.closed.peek() ?? ended()));
		if (following.closed.peek() !== undefined) abort.abort(following.closed.peek() ?? ended());
		const pending = AbortSignal.any([signal, abort.signal]);
		try {
			const broadcast = await waitFor(following.active, (active) => active !== undefined, pending);
			if (!broadcast) throw ended();
			return { consumer: broadcast.track(name).subscribe(reliable), peer: broadcast.closed };
		} finally {
			stop();
		}
	}

	async #dial(protocol: Protocol, peer: string, info: Track.InfoInit, options?: Options): Promise<Stream> {
		segment(peer);
		if (peer === this.id) throw new Error("dialing the same socket endpoint is unsupported");
		const abort = new AbortController();
		const signal = AbortSignal.any([this.#signal(options), abort.signal]);
		signal.throwIfAborted();
		const session = Array.from(crypto.getRandomValues(new Uint8Array(16)), (byte) =>
			byte.toString(16).padStart(2, "0"),
		).join("");
		const name = `${protocol}/${this.id}/${peer}/${session}`;
		const answer = hooks.reserveUnlistedTrack(this.#publication.broadcast, name);
		let following: Origin.Following | undefined;
		let consumer: Track.Subscriber | undefined;
		let producer: Track.Producer | undefined;
		let stop: Dispose = () => {};
		try {
			const remote = await this.#remote(peer, name, signal, (value) => {
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
			return this.#open(protocol, { producer, consumer, peer, session }, following, remote.peer);
		} catch (error) {
			answer.reject(error instanceof Error ? error : cancelled());
			consumer?.close();
			following?.close();
			throw error;
		} finally {
			stop();
			abort.abort(cancelled());
		}
	}

	async #accept(
		protocol: Protocol,
		request: Track.Request,
		info: Track.InfoInit,
		options?: Options,
	): Promise<Stream> {
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
		let following: Origin.Following | undefined;
		let consumer: Track.Subscriber | undefined;
		let stopError: Dispose = () => {};
		try {
			check();
			signal.throwIfAborted();
			const remote = await this.#remote(name.dialer, request.name, signal, (value) => {
				following = value;
			});
			consumer = remote.consumer;
			stopError = hooks.watchSubscriberError(consumer, (error) => abort.abort(error));
			await untilAborted(consumer.info(), signal);
			signal.throwIfAborted();
			if (!following) throw cancelled();
			hooks.keepRequest(request);
			return this.#open(
				protocol,
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
			stopError();
		}
	}

	#open(protocol: Protocol, parts: Parts, following: Origin.Following, peer: GetPromise<Error | null>): Stream {
		this.#abort.signal.throwIfAborted();
		if (following.active.peek()?.closed !== peer) throw ended();
		const session = new Session(parts, following, peer, () => this.#sessions.delete(session));
		this.#sessions.add(session);
		try {
			return protocol === "tcp" ? session.tcp() : session.track();
		} catch (error) {
			session.reset(error instanceof Error ? error : cancelled());
			throw error;
		}
	}

	/** Reset every held session, cancel pending operations, and retract the endpoint; idempotent. */
	close(): void {
		this.#end(null);
	}

	#end(error: Error | null): void {
		if (this.#closed.peek() !== undefined) return;
		this.#closed.set(error);
		this.#unclose();
		this.#abort.abort(error ?? ended());
		for (const queue of Object.values(this.#queues)) queue.close();
		for (const session of this.#sessions) session.reset(ended());
		this.#publication.broadcast.close();
	}
}

// Owns the routing hold, monitors both halves, and ends the TCP copy on protocol violations.
class Session {
	#parts: Parts;
	#following: Origin.Following;
	#peer: GetPromise<Error | null>;
	#done: Dispose;
	#abort = new AbortController();
	#stops: Dispose[] = [];
	#local?: Group.Producer;
	#outgoing?: Group.Producer;
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

	tcp(): TcpStream {
		const { producer, consumer } = this.#parts;
		const outgoing = producer.createGroup(0n);
		const local = new Group.Producer(0n);
		const incoming = local.consume();
		this.#local = local;
		this.#outgoing = outgoing;
		this.#monitor();
		this.#watch(outgoing.closed, () => {
			producer.finish();
			this.#sent = true;
		});
		this.#watch(producer.closed, () => {});
		this.#watch(consumer.closed, () => {});
		const check = () => {
			if (!local.demand().wanted.peek() && !this.#received) this.reset(cancelled());
		};
		this.#hold(local.demand().wanted.subscribe(check));
		void this.#copy().catch((error) => this.reset(error instanceof Error ? error : cancelled()));
		void this.#groups().catch((error) => this.reset(error instanceof Error ? error : cancelled()));
		return makeTcp(this.#parts, outgoing, incoming);
	}

	async #copy(): Promise<void> {
		const ordered = this.#parts.consumer.ordered();
		const group = await ordered.nextGroup();
		if (group) {
			try {
				if (group.sequence !== 0n) throw new ProtocolViolation("second group on a TCP session");
				for (;;) {
					const frame = await group.readFrame();
					if (!frame) break;
					this.#local?.writeFrame({ payload: frame.payload });
				}
			} finally {
				group.close();
			}
		}
		if (this.#abort.signal.aborted) return;
		this.#received = true;
		this.#local?.finish();
		this.#complete();
	}

	async #groups(): Promise<void> {
		for (;;) {
			const largest = await this.#parts.consumer.largest();
			if (!largest) return;
			if (largest.group > 0n) throw new ProtocolViolation("second group on a TCP session");
		}
	}

	#complete(): void {
		if (this.#sent && this.#received) this.#close(false);
	}

	reset(error: Error): void {
		if (this.#abort.signal.aborted) return;
		this.#outgoing?.abort(error);
		this.#local?.abort(error);
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
		if (reset || this.#local) this.#parts.consumer.close();
		this.#following.close();
		this.#done();
	}
}
