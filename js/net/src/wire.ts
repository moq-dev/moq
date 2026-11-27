/**
 * Package-private capabilities used by the protocol implementations.
 *
 * Public handles register their wire view when they are constructed. Keeping the view in a
 * WeakMap means applications cannot call transport-facing operations on the handles they own,
 * while the protocol layers still share one implementation.
 *
 * @module
 */
import type { Dispose, GetPromise, Getter } from "@moq/signals";
import type * as broadcast from "./broadcast.ts";
import type * as datagram from "./datagram.ts";
import type { Consumer as GroupConsumer } from "./group.ts";
import type { WireRoute } from "./hop.ts";
import type * as Identity from "./identity.ts";
import type { Lister, Tracks } from "./listing.ts";
import type * as origin from "./origin.ts";
import type * as Path from "./path.ts";
import type { Timescale } from "./time.ts";
import type * as track from "./track.ts";
import { checkName } from "./util/utf8.ts";

/** The protocol-facing operations behind a broadcast handle. */
export interface Broadcast {
	/** Whether datagram metadata comes from an application rather than a peer's kindless SUBSCRIBE. */
	localDatagrams: boolean;
	datagrams(name: string): datagram.Consumer;
	liveDatagrams(name: string): datagram.Consumer | undefined;
	/** Create a datagram track, listed unless `listed` is false, as a session's track riding a stream track's subscription. */
	createDatagrams(name: string, info: Partial<datagram.Info>, listed?: boolean): datagram.Producer | undefined;
	requestedDatagrams(): Promise<datagram.Request | undefined>;
	subscribe(name: string, options?: track.Subscription): track.Subscriber;
	/** The track's properties; `hold` keeps the track wanted until it aborts, the way a held TRACK stream does, and aborting it before the answer abandons the lookup. */
	resolveTrackInfo(name: string, hold?: AbortSignal): Promise<track.Info>;
	/**
	 * The newest frame the track's cache holds, read without subscribing; `undefined` while it holds
	 * none. With `timescale`, its `frame` is that frame's moq-timestamp Object ID at that scale.
	 */
	largest(name: string, timescale?: Timescale): track.Position | undefined;
	/** Fetch one group; absent when the session's version has no FETCH, so callers never ask. */
	fetchGroup?(name: string, key: bigint, options?: track.FetchGroupOptions): Promise<GroupConsumer>;
	requested(): Promise<track.Request | undefined>;
	// Sessions dedup a peer's listings per broadcast; a custom session has no public owner yet.
	/** List the broadcast's tracks for `owner`, ending its older listing of this broadcast with CANCELLED. */
	tracks(owner?: object): Tracks;
	/** Answer the broadcast's listings, as a session does with its peer's; throws while another answers. */
	lister(): Lister;
	/**
	 * The operations a metadata reader resolves names through, each name's metadata copy, where the
	 * handle keeps those apart from the payload copies; undefined elsewhere. See {@link metaWireOf}.
	 */
	meta?(): Broadcast | undefined;
}

/** Receives one change to the watched prefixes: `prefix` is now (or no longer) wanted. */
export type InterestListener = (prefix: Path.Valid, wanted: boolean) => void;

/** An origin's open request for a path, as a serving session sees it. */
export interface RequestSlot {
	/** The blind answer a session already gave it, if any. */
	readonly answer?: broadcast.Consumer;
}

/** The protocol-facing operations behind an origin producer. */
export interface OriginProducer {
	/**
	 * Hear the root-relative prefixes readers of this origin are watching, narrowed to its
	 * scope: each one currently wanted first, then every change. A prefix may be reported
	 * wanted more than once, by overlapping watches, and is released as many times. Sessions
	 * ask their peer for announcements under exactly these. Returns the unsubscribe.
	 */
	interests(listener: InterestListener): Dispose;
	/** Whether an advertised prefix overlaps the handle's allowed paths. */
	accepts(prefix: Path.Valid): boolean;
	/** Land a route a peer announced, taken as given and never forwarded back to a peer. */
	receive(prefix: Path.Valid, route?: Partial<WireRoute>): origin.Claim;
	attach(discovery: boolean): Dispose;
	/**
	 * Hold the origin's announcement streams overlapping `prefix` unsynced until
	 * the returned dispose: a session holds one while its peer's initial set is in flight.
	 */
	snapshot(prefix: Path.Valid): Dispose;
	readonly requests: Getter<ReadonlyMap<Path.Valid, RequestSlot>>;
	changed(): GetPromise<unknown>;
	/** Whether sessions should answer the request with a blind subscription right now. */
	blind(slot: RequestSlot): boolean;
	answer(path: Path.Valid, front: broadcast.Consumer): Dispose | undefined;
	routes(path: Path.Valid): boolean;
	/** The rank of the link a session given this handle lands its routes through. */
	readonly priority: number;
}

/** The protocol-facing operations behind an origin route or claim. */
export interface Route {
	/** The metadata the route is advertised with, or undefined while hidden. */
	readonly info: WireRoute | undefined;
	/**
	 * Advertise the route with `info` as given, as a session's update does: a named identity
	 * replaces the route's, which restarts its readers; otherwise it keeps the route's own.
	 * Announces a hidden route.
	 */
	update(info: Partial<WireRoute>): void;
	/**
	 * Close the route as its announcer withdrawing it, so readers see it end. Closing the handle
	 * of a route a session received instead means the session lost it, and readers see it go
	 * offline.
	 */
	end(): void;
}

/** The protocol-facing operations behind an origin consumer. */
export interface OriginConsumer {
	routes(path: Path.Valid): boolean;
	readonly advertised: Getter<Advertisements | undefined>;
	/** The announced local broadcast at `path`, when it is the route peers are offered there.
	 * A request naming an `identity` only matches a route serving it. */
	local(path: Path.Valid, identity?: Identity.Valid): broadcast.Consumer | undefined;
	/** Resolve `path` for serving; throws `Unroutable` when the route serves another `identity`. */
	demand(path: Path.Valid, identity?: Identity.Valid, signal?: AbortSignal): Promise<broadcast.Consumer | undefined>;
	/** Whether the handle's scope admits everything beneath the root-relative `prefix`. */
	covers(prefix: Path.Valid): boolean;
	/** The root-relative prefixes the handle's scope admits; unset for every path. */
	readonly allowed?: Path.Prefixes;
	/** The absolute path the handle's root-relative paths hang from. */
	readonly root: Path.Valid;
}

/** One originated advertisement exposed to the publishing wire. */
export interface Advertised {
	readonly ref: object;
	readonly route: WireRoute;
	/** The absolute prefix a claim raised to its handle's root is filed at; unset when filed at its row. */
	readonly filed?: Path.Valid;
	/** Whether it is a broadcast published here, which serves only the path it is filed at. */
	readonly exact?: boolean;
	/** Its place, newest first, among the claims filed with it; unset when that is its place in its row. */
	readonly order?: number;
}

/** What names a stream: an advertisement, and the identity its route serves. */
export interface Served {
	readonly ref: object;
	readonly route: Pick<WireRoute, "identity" | "epoch">;
}

/**
 * Whether two advertisements serve one stream: the same route identity, or, for a route not
 * yet named one, the same advertisement.
 */
export function sameIdentity(a: Served, b: Served): boolean {
	if (a.route.identity !== undefined || b.route.identity !== undefined) return a.route.identity === b.route.identity;
	return a.ref === b.ref;
}

/**
 * Every originated advertisement per prefix, most preferred first. A reader ranks only what its
 * scope admits, so a cheaper route it cannot use never hides one it can, and ranks the claims
 * covering its root as one tier wherever each is filed.
 */
export type Advertisements = ReadonlyMap<Path.Valid, readonly Advertised[]>;

/** The protocol-facing operations behind an established session. */
export interface Established {
	/**
	 * Consume `path` from the session: the route it ranks best, or with `identity`, the best
	 * route of that identity alone, as a request pinned to it resolves.
	 */
	consume(path: Path.Valid, identity?: Identity.Valid): broadcast.Consumer;
	/**
	 * Whether the peer refuses an announce request overlapping one it still holds, so
	 * discovery never holds two where one covers the other.
	 */
	readonly exclusive: boolean;
	/**
	 * Tell the peer this side is moving, with a GOAWAY naming no URI where the version carries
	 * one, and stop taking new tokens, so the auth handle backs a replacement while this session
	 * keeps serving.
	 */
	leave(): void;
	// private: plumbing
	/**
	 * This session's side of its auth handle.
	 *
	 * @internal
	 */
	readonly auth: import("./auth_session.ts").AuthSession;
}

/** A produced broadcast's view, which always fetches from its own cache. */
export type Produced = Broadcast & Required<Pick<Broadcast, "fetchGroup">>;

type View = Broadcast | OriginProducer | OriginConsumer | Route | Established;
const views = new WeakMap<object, View>();

/** Register the package-private view for a handle. */
export function registerWire(handle: object, view: View): void {
	views.set(handle, view);
}

/** Replace selected operations on a broadcast's package-private view. */
export function overrideBroadcastWire(
	handle: broadcast.Consumer,
	overrides: Partial<Pick<Broadcast, "resolveTrackInfo" | "fetchGroup">>,
): void {
	const view = views.get(handle);
	if (!view) throw new Error("broadcast has no wire view");
	views.set(handle, { ...(view as Broadcast), ...overrides });
}

export function wireOf(handle: broadcast.Producer): Produced;
export function wireOf(handle: broadcast.Producer | broadcast.Consumer): Broadcast;
export function wireOf(handle: origin.Producer): OriginProducer;
export function wireOf(handle: origin.Consumer): OriginConsumer;
export function wireOf(handle: origin.Route | origin.Claim): Route;
export function wireOf(handle: import("./connection/session.ts").Session): Established;
/** Return the package-private view for a handle. */
export function wireOf(handle: object): View {
	const view = views.get(handle);
	if (!view) throw new Error("handle has no wire view");
	return view;
}

let makeTrack: ((name: string, source: Broadcast) => track.Consumer) | undefined;

/** Install the private Track.Consumer constructor. */
export function registerTrackConsumer(factory: (name: string, source: Broadcast) => track.Consumer): void {
	makeTrack = factory;
}

/** Create a track handle backed by a broadcast's private wire view. */
export function trackOf(name: string, source: broadcast.Producer | broadcast.Consumer): track.Consumer {
	if (!makeTrack) throw new Error("track module is not loaded");
	checkName(name);
	return makeTrack(name, wireOf(source));
}

/**
 * The view a metadata reader resolves names through: the metadata copies where the handle keeps
 * them apart (a lite-07 session's broadcast, or an origin front), and otherwise the payload
 * copies, whose headers it takes. Mirrors the Rust `broadcast::Consumer::track_meta`.
 */
export function metaWireOf(source: broadcast.Producer | broadcast.Consumer): Broadcast {
	const view = wireOf(source);
	return view.meta?.() ?? view;
}

/** {@link trackOf} for a metadata reader, resolving the copy {@link metaWireOf} picks. */
export function trackMetaOf(name: string, source: broadcast.Producer | broadcast.Consumer): track.Consumer {
	if (!makeTrack) throw new Error("track module is not loaded");
	checkName(name);
	return makeTrack(name, metaWireOf(source));
}
