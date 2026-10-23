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
import type { Drain } from "./connection/goaway.ts";
import type * as Epoch from "./epoch.ts";
import type { Consumer as GroupConsumer } from "./group.ts";
import type { Route } from "./hop.ts";
import type { Lister, Tracks } from "./listing.ts";
import type * as origin from "./origin.ts";
import type * as Path from "./path.ts";
import type * as track from "./track.ts";

/** The protocol-facing operations behind a broadcast handle. */
export interface Broadcast {
	// pending: /quest/m1/public-model/serve.md
	subscribe(name: string, options?: track.Subscription): track.Subscriber;
	// pending: /quest/m1/public-model/serve.md
	/** The track's properties; `hold` keeps the track wanted until it aborts, the way a held TRACK stream does, and aborting it before the answer abandons the lookup. */
	resolveTrackInfo(name: string, hold?: AbortSignal): Promise<track.Info>;
	// pending: /quest/m1/public-model/serve.md
	/** The newest frame the track's cache holds, read without subscribing; `undefined` while it holds none. */
	largest(name: string): track.Location | undefined;
	// pending: /quest/m1/public-model/serve.md
	fetchGroup(name: string, sequence: number, options?: track.FetchGroupOptions): Promise<GroupConsumer>;
	// pending: /quest/m1/public-model/answer.md
	requested(): Promise<track.Request | undefined>;
	// private: capability gap
	// Sessions dedup a peer's listings per broadcast; a custom session has no public owner yet.
	/** List the broadcast's tracks for `owner`, ending its older listing of this broadcast with CANCELLED. */
	tracks(owner?: object): Tracks;
	// pending: /quest/m1/public-model/answer.md
	/** Answer the broadcast's listings, as a session does with its peer's; throws while another answers. */
	lister(): Lister;
	// pending: /quest/m1/public-model/answer.md
	/**
	 * A session delivered this broadcast. An originated route refuses to serve it:
	 * publishing it would name this origin's hop for upstream content.
	 */
	fromSession?: boolean;
}

/** Receives one change to the watched prefixes: `prefix` is now (or no longer) wanted. */
export type InterestListener = (prefix: Path.Valid, wanted: boolean) => void;

/** The protocol-facing operations behind an origin producer. */
export interface OriginProducer {
	// pending: /quest/m1/public-model/answer.md
	/**
	 * Hear the root-relative prefixes readers of this origin are watching, narrowed to its
	 * scope: each one currently wanted first, then every change. A prefix may be reported
	 * wanted more than once, by overlapping watches, and is released as many times. Sessions
	 * ask their peer for announcements under exactly these. Returns the unsubscribe.
	 */
	interests(listener: InterestListener): Dispose;
	// pending: /quest/m1/public-model/answer.md
	/** Whether an advertised prefix overlaps the handle's allowed paths. */
	accepts(prefix: Path.Valid): boolean;
	// pending: /quest/m1/public-model/answer.md
	receive(
		prefix: Path.Valid,
		route?: Route | { epoch?: Route["epoch"]; hops?: Route["hops"]; cost?: Route["cost"]; auth?: Route["auth"] },
	): origin.Dynamic;
	// pending: /quest/m1/public-model/answer.md
	attach(discovery: boolean): Dispose;
	// pending: /quest/m1/public-model/answer.md
	expect(): Dispose;
	// pending: /quest/m1/public-model/answer.md
	/**
	 * Hold the origin's announcement streams overlapping `prefix` unsynced until
	 * the returned dispose: a session holds one while its peer's initial set is in flight.
	 */
	snapshot(prefix: Path.Valid): Dispose;
	// pending: /quest/m1/public-model/answer.md
	readonly requests: Getter<ReadonlyMap<Path.Valid, origin.RequestSlot> | undefined>;
	// pending: /quest/m1/public-model/answer.md
	changed(): GetPromise<unknown>;
	// pending: /quest/m1/public-model/answer.md
	/** Whether sessions should answer the request with a blind subscription right now. */
	blind(slot: origin.RequestSlot): boolean;
	// pending: /quest/m1/public-model/answer.md
	answer(path: Path.Valid, front: broadcast.Consumer): Dispose | undefined;
	// pending: /quest/m1/public-model/answer.md
	routes(path: Path.Valid): boolean;
}

/** The protocol-facing operations behind an origin consumer. */
export interface OriginConsumer {
	// pending: /quest/m1/public-model/answer.md
	routes(path: Path.Valid): boolean;
	// pending: /quest/m1/public-model/answer.md
	readonly broadcasts: Getter<ReadonlyMap<Path.Valid, broadcast.Consumer> | undefined>;
	// pending: /quest/m1/public-model/answer.md
	readonly advertised: Getter<Advertisements | undefined>;
	// pending: /quest/m1/public-model/answer.md
	/** The announced local broadcast at `path`, when it is the route peers are offered there.
	 * A request naming an `epoch` only matches a route serving it. */
	local(path: Path.Valid, epoch?: Epoch.Valid): broadcast.Consumer | undefined;
	// pending: /quest/m1/public-model/answer.md
	/** Resolve `path` for serving; throws `Unroutable` when the route serves another `epoch`. */
	demand(path: Path.Valid, epoch?: Epoch.Valid, signal?: AbortSignal): Promise<broadcast.Consumer | undefined>;
}

// pending: /quest/m1/public-model/answer.md
/** One originated advertisement exposed to the publishing wire. */
export interface Advertised {
	readonly identity: object;
	readonly route: Route;
	/** The paths a scoped route may serve beneath its prefix, relative like its key; unset for the whole subtree. */
	readonly claim?: Path.Patterns;
}

/** What names a publisher instance: an advertisement's identity and epoch. */
export interface Instance {
	readonly identity: object;
	readonly route: Pick<Route, "epoch">;
}

// pending: /quest/m1/public-model/answer.md
/**
 * Whether two advertisements serve one publisher instance: the same epoch, or, without one,
 * the same advertisement, since nothing says another serves the same bytes.
 */
export function sameInstance(a: Instance, b: Instance): boolean {
	if (a.route.epoch !== undefined || b.route.epoch !== undefined) return a.route.epoch === b.route.epoch;
	return a.identity === b.identity;
}

// pending: /quest/m1/public-model/answer.md
/**
 * Every originated advertisement per prefix, most preferred first. A reader takes the first
 * one its scope admits, so a cheaper route it cannot use never hides one it can.
 */
export type Advertisements = ReadonlyMap<Path.Valid, readonly Advertised[]>;

/** The protocol-facing operations behind an established session. */
export interface Established {
	// private: plumbing
	consume(path: Path.Valid): broadcast.Consumer;
	// private: plumbing
	/** Settles with the peer's GOAWAY; the session keeps serving until it closes. */
	readonly goaway: GetPromise<Drain>;
}

type View = Broadcast | OriginProducer | OriginConsumer | Established;
const views = new WeakMap<object, View>();

// private: plumbing
/** Register the package-private view for a handle. */
export function registerWire(handle: object, view: View): void {
	views.set(handle, view);
}

// pending: /quest/m1/public-model/deferred.md
/**
 * Replace selected operations on a broadcast's package-private view.
 *
 * Only a session-delivered broadcast does this, so the replacement is marked.
 * An origin refuses to serve that broadcast on a route it originated.
 */
export function overrideBroadcastWire(
	handle: broadcast.Consumer,
	overrides: Partial<Pick<Broadcast, "resolveTrackInfo" | "fetchGroup">>,
): void {
	const view = views.get(handle);
	if (!view) throw new Error("broadcast has no wire view");
	views.set(handle, { ...(view as Broadcast), ...overrides, fromSession: true });
}

// private: plumbing
export function wireOf(handle: broadcast.Producer | broadcast.Consumer): Broadcast;
export function wireOf(handle: origin.Producer): OriginProducer;
export function wireOf(handle: origin.Consumer): OriginConsumer;
export function wireOf(handle: import("./connection/established.ts").Established): Established;
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
	return makeTrack(name, wireOf(source));
}
