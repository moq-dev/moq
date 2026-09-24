/**
 * Package-private capabilities used by the protocol implementations.
 *
 * Public handles register their wire view when they are constructed. Keeping the view in a
 * WeakMap means applications cannot call transport-facing operations on the handles they own,
 * while the protocol layers still share one implementation.
 *
 * @module
 */
import type { Dispose, Getter } from "@moq/signals";
import type * as broadcast from "./broadcast.ts";
import type { Consumer as GroupConsumer } from "./group.ts";
import type { Route } from "./hop.ts";
import type * as origin from "./origin.ts";
import type * as Path from "./path.ts";
import type * as track from "./track.ts";

/** The protocol-facing operations behind a broadcast handle. */
export interface Broadcast {
	subscribe(name: string, options?: track.Subscription): track.Subscriber;
	resolveTrackInfo(name: string): Promise<track.Info>;
	fetchGroup(name: string, sequence: number, options?: track.FetchGroupOptions): Promise<GroupConsumer>;
	requested(): Promise<track.Request | undefined>;
}

/** The protocol-facing operations behind an origin producer. */
export interface OriginProducer {
	receive(
		prefix: Path.Valid,
		route?: Route | { hops?: Route["hops"]; cost?: Route["cost"] | bigint },
	): origin.Dynamic;
	attach(discovery: boolean): Dispose;
	expect(): Dispose;
	readonly requests: Getter<ReadonlyMap<Path.Valid, origin.RequestSlot> | undefined>;
	changed(): Promise<unknown>;
	answer(path: Path.Valid, front: broadcast.Consumer): Dispose | undefined;
	routes(path: Path.Valid): boolean;
}

/** The protocol-facing operations behind an origin consumer. */
export interface OriginConsumer {
	routes(path: Path.Valid): boolean;
	readonly broadcasts: Getter<ReadonlyMap<Path.Valid, broadcast.Consumer> | undefined>;
	readonly advertised: Getter<ReadonlyMap<Path.Valid, Advertised> | undefined>;
	/** The announced local broadcast at `path`, when it is the route peers are offered there. */
	local(path: Path.Valid): broadcast.Consumer | undefined;
	demand(path: Path.Valid): Promise<broadcast.Consumer | undefined>;
}

/** One originated advertisement exposed to the publishing wire. */
export interface Advertised {
	readonly identity: object;
	readonly route: Route;
}

/** The protocol-facing operation behind an established session. */
export interface Established {
	consume(path: Path.Valid): broadcast.Consumer;
}

type View = Broadcast | OriginProducer | OriginConsumer | Established;
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
