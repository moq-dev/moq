import { Milli } from "@moq/clock";
import { type Dispose, type GetPromise, type Getter, race, Signal } from "@moq/signals";
import * as announce from "../announced.ts";
import type { Grant } from "../auth.ts";
import type { AuthSession } from "../auth_session.ts";
import * as broadcast from "../broadcast.ts";
import type { Drain } from "../connection/goaway.ts";
import { BroadcastCache } from "../consume.ts";
import * as DatagramStream from "../datagram_stream.ts";
import type * as Epoch from "../epoch.ts";
import {
	closeError,
	error,
	ProtocolViolation,
	reason,
	SessionCode,
	StreamCode,
	Stream as StreamError,
	sessionCause,
	unauthorized,
} from "../error.ts";
import type * as netGroup from "../group.ts";
import { Cost, type Route, routesEqual, UNKNOWN_HOP } from "../hop.ts";
import {
	finalSequence,
	groupBounds,
	hiddenBelow,
	hooks,
	scopeCaptures,
	scopeHead,
	scopeOverlaps,
} from "../internal.ts";
import type { Lister } from "../listing.ts";
import * as Path from "../path.ts";
import { type Cursor, Reader, type Stream } from "../stream.ts";
import { Tail } from "../tail.ts";
import type { Timescale, Timestamp } from "../time.ts";
import type * as track from "../track.ts";
import { untilAborted } from "../util/abort.ts";
import { closeUnwanted } from "../util/announce.ts";
import { Deadline, requestOptions } from "../util/deadline.ts";
import { abandoned as demandAbandoned, isAbandoned, unwanted } from "../util/demand.ts";
import { LIMITS, type Limits, SlotMap, Slots } from "../util/limits.ts";
import { overrideBroadcastWire, wireOf } from "../wire.ts";
import { DuplicateTrackAlias, RetiredTrackAlias, TrackAliases } from "./aliases.ts";
import * as Cluster from "./cluster.ts";
import { ObjectDatagram } from "./datagram.ts";
import { requestReason } from "./error.ts";
import { Fetch, FetchHeader, FetchOk } from "./fetch.ts";
import { isDraft22 } from "./filter.ts";
import { Join } from "./join.ts";
import * as Message from "./message.ts";
import * as Namespace from "./namespace.ts";
import {
	decodeFetchObject,
	decodeObjectTime,
	EndOfTrackProperties,
	Frame,
	type Group as GroupMessage,
	ObjectIdGap,
} from "./object.ts";
import { Parameters } from "./parameters.ts";
import { DEFAULT_WIRE, fromWire, toWire } from "./priority.ts";
import { type Publish, PublishDone, PublishDoneStatus, publishDoneClean } from "./publish.ts";
import { type AdvertParams, type PublishNamespace, PublishNamespaceUpdate } from "./publish_namespace.ts";
import { RequestError, RequestOk } from "./request.ts";
import { type Framed, RequestStream, refuseReserved, refuseUpdate } from "./request_stream.ts";
import type { Session } from "./session.ts";
import { joinFilter, Subscribe, SubscribeOk, SubscribeUpdate } from "./subscribe.ts";
import { SubscribeNamespace, SubscribeNamespaceEntry, SubscribeNamespaceEntryDone } from "./subscribe_namespace.ts";
import { MalformedAuthToken } from "./token.ts";

// Wire ceiling (2^62-1). A draining session stamps it on every live route so any other
// candidate outranks it, while the route stays selectable as the last path. Matches Rust
// Cost::DRAIN: cost is the whole mechanism, not a separate state.
const DRAIN_COST: Cost = 2n ** 62n - 1n;

/** A fetch stream the head cannot be stitched from, which costs only that head. */
class JoinUnsupported extends Error {
	constructor(message: string) {
		super(message);
		this.name = "JoinUnsupported";
	}
}

// A live subscription, as the track alias its data streams name resolves to.
type Subscription = {
	// The write side incoming group streams are routed into.
	track: track.Producer;
	// The group streams received, so the subscription can wait for the ones PUBLISH_DONE
	// says are still owed.
	tail: Tail;
	// The track's exclusive end, once an END_OF_TRACK declares it.
	end?: number;
	// The join of SUBSCRIBE_OK's Largest group, whose head a draft-18 joining FETCH or a
	// draft-22 fill brings.
	join?: Join;
	// The track's TIMESCALE, which the head's fetch objects are stamped in too.
	timescale?: Timescale;
	// The request IDs the head's fetch stream may name, registered until the subscription ends.
	fetchIds: bigint[];
};

// Out-parameter for #openSubscribe: lets the caller observe partial progress
// (stream opened, trackAlias registered) so it can clean up on timeout even
// before the setup promise settles.
type SubscribeSetupState = {
	stream?: RequestStream;
	// The accepted subscription, which a registered alias names.
	subscription?: Subscription;
	registeredAlias?: bigint;
	/**
	 * Whether the caller has given up. Setup checks this once the stream exists so a request
	 * opened after the timeout still unwinds: nothing else settles setup if the peer has
	 * gone quiet, and the cleanup that fired first saw no stream to tear down.
	 */
	cancelled?: boolean;
	/**
	 * Whether the SUBSCRIBE reached the wire. From that moment the publisher may be serving,
	 * even before it answers, so abandoning owes it a cancellation regardless of whether we
	 * ever saw the SUBSCRIBE_OK.
	 */
	sent?: boolean;
	/**
	 * Whether the publisher answered with an error. It has torn the request down already, so
	 * there is nothing left to cancel and naming the dead request id at it risks being read
	 * as a protocol violation.
	 */
	rejected?: boolean;
	/** Hands a draft-22 fill stream that overtook SUBSCRIBE_OK the subscription it heads, or nothing. */
	fill?: (subscription: Subscription | undefined) => void;
};

/** An announced path: each live advertisement referencing it, oldest first, and the route it holds. */
type Announced = { adverts: { id: number; route: Route }[]; route: Route };

/** A local announce reader's filter: its scope, the prefix it asked for, and its hidden opt-in. */
type Filter = { scope: Path.Pattern; prefix: Path.Valid; hidden: boolean; covering: boolean };

/** A local announce reader: its filter, and what settles once its request is off the wire. */
type Asking = Filter & { gone?: Promise<void> };

/** Whether a reader with `filter` sees an announcement at `path`. */
function sees(filter: Filter, path: Path.Valid): boolean {
	return (
		(filter.covering ? scopeOverlaps(filter.scope, path) : filter.scope.matches(path)) &&
		(filter.hidden || !hiddenBelow(filter.prefix, path))
	);
}

/**
 * Handles subscribing to broadcasts using moq-transport protocol.
 * Each request rides its own bidi stream.
 *
 * @internal
 */
export class Subscriber {
	#session: Session;

	// The transport, so a request cut off by the session's close ends with the session's
	// error. Optional for tests that drive a bare session.
	#quic?: WebTransport;

	// The Hop IDs this session declared; see {@link Cluster}. What the peer declared is what
	// says whether an advertisement carries a hop path, and ours is what a path looping back
	// to us contains.
	#cluster?: Cluster.Hops;

	// Whether both SETUPs offered MoQ Epoch, so advertisements may name their instance.
	#epoch: boolean;

	// Publisher-chosen aliases used by incoming group streams.
	#aliases = new TrackAliases<Subscription>();

	// Units for each track's object Timestamps, from the TIMESCALE Track Property in
	// SUBSCRIBE_OK. A track missing from this map declared no timeline, so its frames
	// arrive untimed.
	#timescales = new Map<bigint, Timescale>();

	// The requests a fetch stream may answer, mapped to the subscription whose head it brings:
	// a draft-18 joining FETCH's own request ID, or a draft-22 SUBSCRIBE's, whose fill can
	// overtake SUBSCRIBE_OK and so waits on it. A fetch stream needs no track alias.
	#fetches = new Map<bigint, Promise<Subscription | undefined>>();

	// Dedup consumed broadcasts per path: repeat consume() calls share one subscription.
	#consumes = new BroadcastCache();

	// Every announced path, with the live advertisements that reference it.
	//
	// A peer may advertise one namespace twice on a session: an unsolicited
	// PUBLISH_NAMESPACE and an inline NAMESPACE answering our own SUBSCRIBE_NAMESPACE are
	// two messages about one source, which the MoQ Solicit draft requires us to tolerate.
	// Tracking each is what keeps the second from duplicating the announce and the first
	// to end from retracting what the other still holds. A publisher replacing its instance
	// (MoQ Epoch) withdraws one advertisement and makes another, on separate streams that
	// can arrive in either order, so the newest stands until it goes and an update to an
	// older one changes nothing. Each live wire advertisement holds an announce slot,
	// even while its route loops back through us and is absent from this derived map.
	#announced = new Map<Path.Valid, Announced>();
	#announceSlots: Slots;

	// Names each advertisement the peer makes, so a later update or withdrawal finds its own.
	#nextAdvert = 0;

	// Any consumers that want each new announcement, keyed by their local filter, until
	// their request is off the wire.
	#announcedConsumers = new Map<announce.Producer, Asking>();

	// Whether the peer understands the HIDDEN parameter (MoQ Hidden).
	#hidden: boolean;
	#maxAge: boolean;

	// Whether a SUBSCRIBE_NAMESPACE's REQUEST_OK counts the NAMESPACEs that follow it (MoQ Active Count).
	#activeCount: boolean;

	// Settles when the peer sends GOAWAY, repricing this session's routes to the drain cost.
	#goaway?: GetPromise<Drain>;
	// Our grant (MoQ Auth): a subscription it stops covering is cancelled. Undefined until
	// the peer answers, which allows everything.
	#grant?: Getter<Grant | undefined>;
	#auth?: AuthSession;

	/**
	 * Creates a new Subscriber instance.
	 *
	 * @internal
	 */
	constructor({
		session,
		quic,
		cluster,
		epoch = false,
		hidden = false,
		maxAge = false,
		activeCount = false,
		goaway,
		grant,
		auth,
		limits = LIMITS,
	}: {
		/** The session abstraction for bidi streams and request IDs. */
		session: Session;
		/** The transport the session runs on. */
		quic?: WebTransport;
		/** The Hop IDs the SETUP exchange settled (MoQ Cluster). */
		cluster?: Cluster.Hops;
		/** Whether both SETUPs offered MoQ Epoch. */
		epoch?: boolean;
		/** Whether the peer understands the HIDDEN parameter (MoQ Hidden). */
		hidden?: boolean;
		/** Whether the peer accepts subscriber max age. */
		maxAge?: boolean;
		/** Whether both SETUPs declared MoQ Active Count. */
		activeCount?: boolean;
		/** Settles when the peer sends GOAWAY. */
		goaway?: GetPromise<Drain>;
		/** The union of our tokens' grants (MoQ Auth), which bounds what we subscribe to. */
		grant?: Getter<Grant | undefined>;
		auth?: AuthSession;
		/** How many live requests the peer may hold. */
		limits?: Limits;
	}) {
		this.#session = session;
		this.#quic = quic;
		this.#announceSlots = new Slots(limits.announces, { quic, reason: "too many announcements" });
		this.#cluster = cluster;
		this.#epoch = epoch;
		this.#hidden = hidden;
		this.#maxAge = maxAge;
		this.#activeCount = activeCount;
		this.#goaway = goaway;
		this.#grant = grant;
		this.#auth = auth;
		// A draining peer usually stops publishing namespaces, so reprice from the signal
		// itself. Waiting for another message would leave the route primary until close.
		void goaway?.then(() => this.#drainAnnounced());
	}

	// Whether the peer has sent GOAWAY. Requests keep opening here until a replacement
	// session's route outranks this one, deliberately past draft-22 section 9.2's SHOULD
	// NOT: refusing them would fail requests that land before the replacement is up.
	#goingAway(): boolean {
		return this.#goaway?.peek() !== undefined;
	}

	// What a route costs once the peer has asked us to leave.
	#priced(route: Route): Route {
		if (!this.#goingAway()) return route;
		if (route.cost === DRAIN_COST) return route;
		return { ...route, cost: DRAIN_COST };
	}

	// Reprice every live advertisement. Idempotent, since the signal stays set.
	#drainAnnounced(): void {
		for (const [path, info] of this.#announced) {
			this.#settleAnnounce(path, info);
		}
	}

	// Whether our grant no longer lets us subscribe to `broadcast`. No grant yet allows it.
	#denied(broadcast: Path.Valid): boolean {
		const grant = this.#grant?.peek();
		return grant !== undefined && !grant.subscribe.matches(broadcast);
	}

	/**
	 * Whether an advertisement is ours coming back: its hop path already ran through us, so
	 * subscribing via it would route us back to ourselves.
	 *
	 * A conforming peer withholds these (it knows our Hop ID), so this is the backstop that
	 * keeps a mesh working when one member does not. A session that negotiated nothing
	 * carries no path, and there is nothing to check.
	 */
	#reflected(advert: Cluster.Advert | undefined): boolean {
		return advert !== undefined && this.#cluster !== undefined && Cluster.loops(advert, this.#cluster.self);
	}

	/**
	 * The route an advertisement carries; one without a path is anonymous and free. Its
	 * epoch is taken as given, and absent when the advertisement names none.
	 */
	#route({ cluster, epoch }: AdvertParams): Route {
		const route = cluster === undefined ? { hops: [UNKNOWN_HOP], cost: Cost.zero } : { ...cluster };
		return epoch === undefined ? route : { ...route, epoch };
	}

	/**
	 * Gets an announced reader matching `scope`. Paths are relative to the session,
	 * not the scope.
	 *
	 * The peer is asked with SUBSCRIBE_NAMESPACE regardless of what it declared, and an
	 * unsolicited PUBLISH_NAMESPACE lands here too, so a peer that only tells and one
	 * that only answers are both discovered.
	 *
	 * Hidden routes (a `.`-prefixed segment below the scope's head) are left out unless
	 * `options.hidden` opts in. The opt-in rides the SUBSCRIBE_NAMESPACE when the peer
	 * understands it (MoQ Hidden); the rule is also applied here, since an unsolicited
	 * PUBLISH_NAMESPACE or a peer that never heard of it hides nothing.
	 *
	 * Only routes whose literal name matches `scope` are reported, unless `options.covering`
	 * also asks for those whose subtree overlaps it.
	 *
	 * The request goes out once every overlapping request already closed here is off the
	 * wire, so a peer that refuses overlapping requests never sees this one beside them.
	 */
	announced(scope: Path.Pattern = Path.Pattern.all(), options?: announce.Options): announce.Consumer {
		// The wire speaks announce interest by prefix.
		const prefix = scopeHead(scope);
		const filter: Asking = {
			scope,
			prefix,
			hidden: options?.hidden ?? false,
			covering: options?.covering ?? false,
		};

		// The overlapping requests on their way out, such as the narrower ones an exclusive
		// discovery just closed for this one. Those still open overlap it only where the
		// caller holds both on purpose. One whose last reader just closed is leaving even
		// before its producer closes.
		const blockers: Promise<void>[] = [];
		for (const [other, { prefix: asked, gone }] of this.#announcedConsumers) {
			if (gone === undefined || (other.wanted.peek() && other.closed.peek() === undefined)) continue;
			if (Path.hasPrefix(asked, prefix) || Path.hasPrefix(prefix, asked)) blockers.push(gone);
		}

		const announced = new announce.Producer();
		for (const [active, info] of this.#announced) {
			if (!sees(filter, active)) continue;
			announced.append({
				prefix: active,
				captures: scopeCaptures(scope, active),
				kind: "start",
				route: info.route,
			});
		}
		this.#announcedConsumers.set(announced, filter);
		const consumer = announced.consume();
		// The last reader leaving ends the stream, which cancels the request.
		closeUnwanted(announced);

		filter.gone = this.#runAnnounced(announced, prefix, filter.hidden && this.#hidden, blockers).finally(() => {
			this.#announcedConsumers.delete(announced);
			announced.close();
		});

		return consumer;
	}

	/**
	 * Record one more advertisement for a path, returning the id that names it. Consumers
	 * hear `start` for the first; a later one stands as the newest (see `#settleAnnounce`).
	 */
	#attachAnnounce(path: Path.Valid, route: Route): number {
		const id = this.#nextAdvert++;
		const existing = this.#announced.get(path);
		if (existing) {
			existing.adverts.push({ id, route });
			this.#settleAnnounce(path, existing);
			return id;
		}
		route = this.#priced(route);
		const entry: Announced = { adverts: [{ id, route }], route };
		this.#announced.set(path, entry);
		this.#consumes.announce(path, entry, route);

		console.debug(`announced: broadcast=${path} active=true epoch=${route.epoch}`);
		for (const [consumer, filter] of this.#announcedConsumers) {
			// A consumer that closed stays registered until its request is off the wire.
			if (!sees(filter, path) || consumer.closed.peek() !== undefined) continue;
			const scope = filter.scope;
			consumer.append({ prefix: path, captures: scopeCaptures(scope, path), kind: "start", route });
		}
		return id;
	}

	/** Replace the route of advertisement `id`, which moves the path only while it is the newest. */
	#updateAnnounce(path: Path.Valid, id: number, route: Route) {
		const existing = this.#announced.get(path);
		const advert = existing?.adverts.find((advert) => advert.id === id);
		if (existing === undefined || advert === undefined) return;
		// A repeat under another epoch is a newer advertisement, so it becomes the newest;
		// one of the same instance stays where it is.
		if (advert.route.epoch !== route.epoch) {
			existing.adverts.splice(existing.adverts.indexOf(advert), 1);
			existing.adverts.push(advert);
		}
		advert.route = route;
		this.#settleAnnounce(path, existing);
	}

	/**
	 * Route a path by its newest live advertisement. A no-op when that changed nothing.
	 * Otherwise consumers hear `update` for new hops or cost, so a forwarder can reprice
	 * without retracting and the shared consume stays, or `restart` for another publisher
	 * instance (MoQ Epoch), so the next consume subscribes afresh.
	 */
	#settleAnnounce(path: Path.Valid, existing: Announced) {
		const newest = existing.adverts.at(-1);
		if (newest === undefined) return;
		const route = this.#priced(newest.route);
		if (routesEqual(existing.route, route)) return;
		const restart = existing.route.epoch !== route.epoch;
		existing.route = route;
		this.#consumes.announce(path, existing, route, restart);
		console.debug(`announced: broadcast=${path} ${restart ? `restart=true epoch=${route.epoch}` : "rerouted"}`);
		const kind = restart ? "restart" : "update";
		for (const [consumer, filter] of this.#announcedConsumers) {
			// A consumer that closed stays registered until its request is off the wire.
			if (!sees(filter, path) || consumer.closed.peek() !== undefined) continue;
			const scope = filter.scope;
			consumer.append({ prefix: path, captures: scopeCaptures(scope, path), kind, route });
		}
	}

	/**
	 * Drop advertisement `id` for a path, retracting it only once the last one goes.
	 */
	#detachAnnounce(path: Path.Valid, id: number) {
		const existing = this.#announced.get(path);
		const index = existing?.adverts.findIndex((advert) => advert.id === id) ?? -1;
		if (existing === undefined || index === -1) return;
		existing.adverts.splice(index, 1);
		if (existing.adverts.length > 0) {
			// The newest went while an older one stands: that one is what remains.
			this.#settleAnnounce(path, existing);
			return;
		}

		this.#announced.delete(path);

		// The path is gone, so stop sharing its broadcast: a holder outliving the publisher
		// would otherwise hand the dead generation to whoever consumes the path next.
		this.#consumes.withdraw(path, existing);
		console.debug(`announced: broadcast=${path} active=false`);

		for (const [consumer, filter] of this.#announcedConsumers) {
			// A consumer that closed stays registered until its request is off the wire.
			if (!sees(filter, path) || consumer.closed.peek() !== undefined) continue;
			const scope = filter.scope;
			consumer.append({ prefix: path, captures: scopeCaptures(scope, path), kind: "end", route: existing.route });
		}
	}

	// Close with this code before the session's own close, which would send a clean one, as
	// the connection does for its own violations.
	#fail(code: SessionCode, reason: string) {
		try {
			this.#quic?.close({ closeCode: code, reason });
		} catch {
			// Already closed.
		}
		this.#session.close();
	}

	async #runAnnounced(
		announced: announce.Producer,
		prefix: Path.Valid,
		hidden: boolean,
		blockers: Promise<void>[],
	): Promise<void> {
		const version = this.#session.version;

		// Suffixes live on this stream, with the advertisement each holds and the epoch it named,
		// so a repeat is recognized as an update to it rather than a second one, which would leak.
		// An advertisement stays live on the wire until its NAMESPACE_DONE, even while its
		// route is detached (`id` unset) for looping back through us.
		const live = new SlotMap<Path.Valid, { id?: number; epoch?: Epoch.Valid }>(this.#announceSlots);

		// Set once `release` has given back everything this stream held. The read loop is not
		// awaited when the local consumer closes first, and the stream keeps delivering what
		// the peer sends until it ends its half, so an entry can still decode afterwards.
		// Attaching one then would take a reference nobody is left to release, pinning the
		// path for the session.
		let released = false;

		// The stream owns every advertisement it carried, so give them back however it ends:
		// as the request starts leaving, a decode error, or the peer resetting it. Without
		// this each namespace keeps its count and the source never detaches, which would pin
		// the path for the session even after the other source withdrew.
		const release = () => {
			released = true;
			for (const [path, held] of live) {
				if (held.id !== undefined) this.#detachAnnounce(path, held.id);
			}
			live.clear();
		};

		// A peer that refuses overlapping requests would refuse this one while it still holds
		// a request this overlaps, so wait for those to leave the wire. A consumer that leaves
		// meanwhile sends nothing.
		if (blockers.length > 0) {
			await race([Promise.allSettled(blockers), announced.closed]);
			if (announced.closed.peek() !== undefined) return;
		}

		// A consumer that leaves while waiting on request-ID credit takes no ID, so a granted one
		// never opens a request nobody wants.
		const left = new AbortController();
		void announced.closed.then(() => left.abort());
		let requestId: bigint | undefined;
		try {
			requestId = await this.#session.nextRequestId(left.signal);
		} catch (err) {
			if (left.signal.aborted) return;
			throw err;
		}
		if (requestId === undefined) return;

		try {
			const deadline = new Deadline();
			const request = new RequestStream(
				await deadline.wait(deadline.open(this.#session.openBi(requestOptions))),
				this.#session,
				this.#quic,
			);

			const run = async () => {
				const msg = new SubscribeNamespace({ namespace: prefix, requestId, hidden });
				await Message.write(request.writer, SubscribeNamespace.id, msg, version);
				console.debug(`subscribe_namespace written: requestId=${requestId}`);

				// End the request, settling once it is off the wire. `reading` is whatever reads
				// the stream now, which has to finish before anything else may.
				const end = async (reading: Promise<unknown>) => {
					release();
					// A request on another stream can overtake the cancel, so this one is off the
					// wire only once the peer ends its half. What it sends meanwhile is dropped.
					request.cancel();
					await reading.catch(() => undefined);
					try {
						while ((await request.next()) !== undefined) {}
					} catch {
						// Reset, or the session ended.
					}
					request.close();
				};

				// Resolves to how many NAMESPACEs make up the initial set (MoQ Active Count).
				const response = (async () => {
					const framed = await request.next();
					if (framed === undefined) throw new Error("namespace stream ended before its response");
					if (framed.type === RequestOk.id) {
						const ok = await RequestOk.decode(framed.reader, version);
						// A count the negotiation did not promise, or a missing one it did, is
						// the peer breaking the extension. Without it, the answer is the best
						// boundary there is.
						const count = ok.parameters.activeCount;
						if (this.#activeCount !== (count !== undefined)) {
							throw new ProtocolViolation("ACTIVE_COUNT does not match the negotiation");
						}
						return count ?? 0;
					}
					// A well-formed refusal already ended the request, so end ours cleanly before
					// failing it. Anything else leaves the stream for the reset.
					if (framed.type === RequestError.id) {
						await RequestError.decode(framed.reader, version);
						request.close();
					}
					throw new Error(`SubscribeNamespace rejected: typeId=0x${framed.type.toString(16)}`);
				})();
				// A consumer that leaves before the peer answers ends the request now, not on the answer.
				const answered = await race([response, announced.closed.then(() => undefined)]);
				if (answered === undefined) {
					await end(response);
					return;
				}
				let initial = answered;

				// Loop reading Namespace/NamespaceDone entries
				const readLoop = (async () => {
					for (;;) {
						if (initial === 0) announced.synced();

						const framed = await request.next();
						// Ending: `end` drains the rest.
						if (released) break;
						if (framed === undefined) {
							if (initial > 0)
								throw new ProtocolViolation("namespace stream ended inside the initial set");
							break;
						}
						const msgType = framed.type;

						// The counted initial set is NAMESPACEs only, so anything else cuts it short.
						if (initial > 0 && msgType !== SubscribeNamespaceEntry.id) {
							throw new ProtocolViolation(`message 0x${msgType.toString(16)} inside the initial set`);
						}
						if (msgType === SubscribeNamespaceEntry.id) {
							const entry = await SubscribeNamespaceEntry.decode(framed.reader, version, {
								cluster: Cluster.negotiated(this.#cluster),
								epoch: this.#epoch,
							});
							// Counted even when discarded below.
							if (initial > 0) initial -= 1;
							if (released) break;

							const path = Path.join(prefix, entry.suffix);
							const params = entry.params ?? {};
							const held = live.get(path);

							// A repeat under another epoch replaces the instance in place, but one
							// that gains or loses an epoch skipped the NAMESPACE_DONE it needs.
							if (held !== undefined && (held.epoch === undefined) !== (params.epoch === undefined)) {
								throw new ProtocolViolation(`NAMESPACE repeated gaining or losing an epoch: ${path}`);
							}

							// The wire still holds a reflected advertisement until DONE, so charge
							// it before deciding whether its route can be used. Repeats keep the slot.
							if (held === undefined) live.set(path, { epoch: params.epoch });

							// A repeat updates the advertisement in place, so one that now
							// loops back through us has taken a route we can't subscribe
							// over: the path is gone even though the message says active.
							if (this.#reflected(params.cluster)) {
								console.debug(`dropping reflected namespace: broadcast=${path}`);
								if (held?.id !== undefined) this.#detachAnnounce(path, held.id);
								live.set(path, { epoch: params.epoch });
								continue;
							}

							// A repeat replaces the advertisement in place: HOP_PATH / ROUTE_COST
							// can change without a NAMESPACE_DONE, and another epoch restarts the
							// path without it ever going absent. Only the first is a new path.
							const route = this.#route(params);
							if (held?.id !== undefined) {
								held.epoch = params.epoch;
								this.#updateAnnounce(path, held.id, route);
							} else {
								live.set(path, { id: this.#attachAnnounce(path, route), epoch: params.epoch });
							}
						} else if (msgType === SubscribeNamespaceEntryDone.id) {
							const entry = await SubscribeNamespaceEntryDone.decode(framed.reader, version);
							if (released) break;
							const path = Path.join(prefix, entry.suffix);

							const held = live.get(path);
							live.delete(path);
							if (held?.id !== undefined) this.#detachAnnounce(path, held.id);
						} else {
							throw new Error(
								`unexpected message on subscribe_namespace stream: 0x${msgType.toString(16)}`,
							);
						}
					}
				})();

				// The race below can end through the consumer's close while a message that
				// already arrived is still decoding, which leaves the loop with nothing
				// awaiting it: a violation decoded after that still closes the session.
				// An advertisement is decoded here rather than in the session dispatch, so
				// this is the only place a malformed one surfaces, and the cluster draft
				// requires closing the session over it: the peer would just repeat it on the
				// next SUBSCRIBE_NAMESPACE.
				readLoop.catch((err: unknown) => request.abort(err));

				// Wait for either the read loop or the announced to close
				await race([readLoop, announced.closed]);
				await end(readLoop);
			};
			await request.serve(run());
		} catch (err: unknown) {
			const e = error(err);
			console.warn(`subscribe_namespace error: ${reason(e)}`);

			// Abort the stream rather than letting the caller's `finally` close it cleanly.
			// A rejected namespace subscription is a failure, and a consumer that can't tell
			// it from "nothing is published under this prefix" waits forever on a broadcast
			// that will never be announced. Matches the lite subscriber.
			announced.close(e);
		} finally {
			release();
		}
	}

	/**
	 * Consumes a broadcast from the connection.
	 *
	 * Deduplicated per serving instance: repeat calls for the same path share one reference-counted
	 * broadcast (and one upstream subscription). The shared broadcast closes once every caller has
	 * closed its handle, so callers close normally.
	 */
	consume(path: Path.Valid): broadcast.Consumer {
		const instance = this.#consumes.instance(path);
		return this.#consumes.get(path, instance) ?? this.#createConsume(path, instance);
	}

	#createConsume(path: Path.Valid, instance: ReturnType<BroadcastCache["instance"]>): broadcast.Consumer {
		// moq-transport resolves metadata through SUBSCRIBE and has no one-shot group fetch here.
		const consumer = new ConsumeBroadcast();
		// Registered before anyone can list the broadcast, so no reader syncs on an empty answer.
		void this.#refuseListing(wireOf(consumer).lister());
		void (async () => {
			for (;;) {
				const request = await wireOf(consumer).requested();
				if (!request) break;
				// Every request names the instance the consume resolved, so the peer refuses
				// rather than answers from another.
				void this.#runSubscribe(path, request, instance.validity?.signal, instance.route.epoch);
			}
		})();
		return this.#consumes.insert(path, instance, consumer);
	}

	// moq-transport lists no tracks here: refuse each listing until the broadcast closes, since an
	// empty answer would claim the peer lists nothing.
	async #refuseListing(lister: Lister): Promise<void> {
		try {
			for (;;) {
				while (!lister.used.peek()) {
					if (lister.closed.peek() !== undefined) return;
					await race([Signal.race(lister.used, lister.closed)]);
				}
				lister.abort(new Error("track listing requires moq-lite-07 or newer"));
			}
		} finally {
			lister.close();
		}
	}

	// The session is gone. If the transport has already closed, that close is the
	// error. A still-open transport, such as a GOAWAY drain, has no peer code yet,
	// so this does not wait for it. Either way the route failed, so a resumed track moves on.
	async #closedSession(): Promise<Error> {
		const closed = () => new StreamError(StreamCode.SessionClosed, { message: "session closed" });
		const quic = this.#quic;
		if (!quic) return closed();
		return Promise.race([
			closeError(quic),
			new Promise<Error>((resolve) => queueMicrotask(() => resolve(closed()))),
		]);
	}

	async #runSubscribe(broadcast: Path.Valid, request: track.Request, valid?: AbortSignal, epoch?: Epoch.Valid) {
		const refused = unauthorized(broadcast);
		try {
			for (;;) {
				const ready = await this.#auth?.ready(broadcast, "subscribe", {
					signal: valid,
					until: () => demandAbandoned(request.demand()),
				});
				if (ready !== false) break;
				// Demand returning before we commit is not abandonment, matching the serving loop.
				if (!isAbandoned(request.demand())) continue;
				request.reject(new Error("subscribe abandoned before it was authorized"));
				return;
			}
		} catch (err) {
			request.reject(error(err));
			return;
		}
		if (this.#denied(broadcast)) {
			request.reject(refused);
			return;
		}

		let requestId: bigint | undefined;
		try {
			valid?.throwIfAborted();
			// The allocator cancels its own wait.
			requestId = await this.#session.nextRequestId(valid);
		} catch (err) {
			request.reject(error(err));
			return;
		}
		if (requestId === undefined) {
			request.reject(await this.#closedSession());
			return;
		}

		console.debug(`subscribe start: id=${requestId} broadcast=${broadcast} track=${request.name}`);

		// Keep the request pending until SUBSCRIBE_OK supplies immutable track metadata.
		// Group streams already wait on the alias, so early data stays behind this response.
		// Open the stream and wait for SUBSCRIBE_OK under a timeout. State
		// flows back via `state` so the timeout path can clean up the stream
		// and any registration if setup eventually finishes.
		const state: SubscribeSetupState = {};
		const deadline = new Deadline();
		const setup = this.#openSubscribe(state, broadcast, request, requestId, deadline, epoch);

		// The publisher can be serving before it answers, so waiting only on the response
		// would miss the local side going away and leave it serving a track nobody reads.
		// Demand returning before we commit is not abandonment, matching the serving loop.
		const demand = request.demand();
		const waitAbandoned = async (): Promise<null> => {
			await demandAbandoned(demand);
			return null;
		};

		let stream: RequestStream;
		let subscription: Subscription;
		let trackAlias: bigint;
		// The SUBSCRIBER_PRIORITY the request went out with.
		let priority: number;
		let maxAge: number | undefined;
		const abandoned = new Error("subscribe abandoned before it was accepted");
		try {
			// Returning demand keeps the same setup and its original deadline.
			const accepted = deadline.wait(setup);
			for (;;) {
				const result = await untilAborted(race([accepted, waitAbandoned()]), valid);
				if (result !== null) {
					stream = result.stream;
					subscription = result.subscription;
					trackAlias = result.alias;
					priority = result.priority;
					maxAge = result.maxAge;
					break;
				}
				if (hooks.demandReleased(demand).peek() === undefined && demand.wanted.peek() !== undefined) continue;
				throw abandoned;
			}
			console.debug(`subscribe ok: id=${requestId} broadcast=${broadcast} track=${request.name}`);
		} catch (err) {
			// A violation on the stream already closed the session through it.
			// A control request that timed out is not late content, so it carries its own code.
			// Local abandonment must commit without yielding after the demand check.
			const e = err === abandoned ? abandoned : await sessionCause(this.#quic, err);
			request.reject(e);
			console.warn(
				`subscribe error: id=${requestId} broadcast=${broadcast} track=${request.name} error=${reason(e)}`,
			);
			// Runs now for whatever is already open, and again if `setup` settles late.
			// Retirement is repeated because a late SUBSCRIBE_OK can register an alias after
			// the first pass; the stream is torn down once.
			let torn = false;
			const cleanup = async (afterSetup: boolean) => {
				state.cancelled = true;

				// A fill stream waiting on SUBSCRIBE_OK gets nothing, and a head already started goes.
				state.fill?.(undefined);
				this.#fetches.delete(requestId);
				state.subscription?.join?.cancel(e);

				if (
					state.registeredAlias !== undefined &&
					state.subscription &&
					this.#aliases.retire(state.registeredAlias, state.subscription)
				) {
					this.#timescales.delete(state.registeredAlias);
				}

				if (!state.stream || torn) return;

				// A SUBSCRIBE still being written must not be torn down from here. Aborting a
				// Web Stream waits for the in-flight write, so the request can still reach the
				// peer while the abort destroys the stream we would have cancelled on. Setup
				// unwinds on the cancelled flag once that write lands, and its pass below has
				// the finished picture.
				if (!state.sent && !afterSetup) return;
				torn = true;

				// A refusal already ended the request, so end ours cleanly: a reset would tell the
				// publisher we failed.
				if (state.rejected) state.stream.close();
				else state.stream.abort(e);
			};

			// Tear down what is already open rather than waiting on `setup`. A peer that
			// opened the stream and then went quiet never settles it, and deferring would
			// leave the request outstanding for the life of the session -- which is the
			// timeout case, not a rare one.
			void cleanup(false);
			setup.then(
				() => cleanup(true),
				() => cleanup(true),
			);
			return;
		}

		const producer = subscription.track;
		let disposeGrant: Dispose | undefined;
		// Ends the joining FETCH with the subscription.
		const joining = new AbortController();
		try {
			if (subscription.join && !isDraft22(this.#session.version)) {
				void this.#runJoiningFetch(subscription, requestId, priority, joining.signal);
			}

			// Which terminal fired decides how the track closes, so tag them rather than
			// racing bare promises.
			const publisherEnded = Symbol("publisher");
			const localEnded = Symbol("local");
			const revokedEnded = Symbol("revoked");
			const idle = Symbol("idle");

			// Losing the grant ends the subscription, leaving the session alone.
			let revoke!: () => void;
			const revoked = new Promise<typeof revokedEnded>((resolve) => {
				revoke = () => resolve(revokedEnded);
			});
			disposeGrant = this.#grant?.subscribe(() => {
				if (this.#denied(broadcast)) revoke();
			});
			// The grant may have shrunk during setup, before this watcher existed.
			if (this.#denied(broadcast)) revoke();

			// Terminal conditions settle at most once (PublishDone, track close = local
			// unsubscribe, a revoked grant); race them once so the demand loop doesn't
			// re-subscribe each pass.
			const unanswered = new Signal(0);
			const done = race([
				this.#runPublishDone(stream, subscription, broadcast, unanswered).then(() => publisherEnded),
				hooks.trackReleased(producer).then(() => localEnded),
				revoked,
			]);
			const updates = this.#runUpdates(stream, requestId, producer, priority, maxAge, unanswered, done);

			// Serve until a terminal condition fires or the last local subscriber leaves. The unused
			// wake is level-triggered: re-check demand so a subscriber that returns before we tear
			// down resumes on the same stream.
			let terminal = localEnded;
			const demand = producer.demand();
			for (;;) {
				const reason = await race([done, unwanted(demand).then(() => idle)]);
				if (
					reason === idle &&
					hooks.demandReleased(demand).peek() === undefined &&
					demand.wanted.peek() !== undefined
				)
					continue;
				terminal = reason;
				break;
			}

			// Close before the cancellation is written, not after: awaiting the write first
			// reopens the window the demand re-check above just closed, and a subscriber that
			// returned during it would be closed by this line. The lite subscriber closes
			// straight out of its loop for the same reason.
			if (terminal === revokedEnded) {
				console.info(`subscription no longer authorized: broadcast=${broadcast} track=${request.name}`);
				producer.abort(refused);
			} else {
				// Only a received end declares one; a copy nobody wants anymore is released, not finished.
				hooks.releaseTrack(producer);
			}

			// Closing settled `done`, so an update still being written finishes and nothing starts
			// another. Its bytes must not interleave with the close.
			await updates;

			stream.close();
			console.debug(`subscribe close: id=${requestId} broadcast=${broadcast} track=${request.name}`);
		} catch (err) {
			stream.abort(err);
			const e = await sessionCause(this.#quic, err);
			producer.abort(e);
			console.warn(
				`subscribe error: id=${requestId} broadcast=${broadcast} track=${request.name} error=${reason(e)}`,
			);
		} finally {
			joining.abort();
			// A track end already settled the head; anything else leaves it for a later join.
			subscription.join?.cancel(new Error("subscription ended"));
			for (const id of subscription.fetchIds) this.#fetches.delete(id);
			disposeGrant?.();
			// Only the owner tears down the alias metadata: a later subscription may have
			// reclaimed the alias and installed its own timescale.
			if (this.#aliases.retire(trackAlias, subscription)) this.#timescales.delete(trackAlias);
		}
	}

	/**
	 * Read the PUBLISH_DONE that ends a subscription, then wait for the data streams it counts.
	 *
	 * An error status aborts the track with it. A clean one leaves streams in flight, since
	 * QUIC does not order them, so wait until the Stream Count many have been read, or a
	 * bounded grace for the ones that never arrive (the draft says to use a timeout). The count
	 * is only a hint: a peer may send 0 regardless, so 0 waits out the grace. A request
	 * stream that FINs without PUBLISH_DONE is a protocol violation.
	 */
	async #runPublishDone(
		stream: RequestStream,
		subscription: Subscription,
		broadcast: Path.Valid,
		unanswered: Signal<number>,
	): Promise<void> {
		const version = this.#session.version;
		// A refused update is followed by PUBLISH_DONE, which ends the subscription, so no
		// further answer is owed.
		let refused = false;
		let framed: Framed | undefined;
		for (;;) {
			framed = await stream.next();
			if (framed === undefined) throw new ProtocolViolation("subscribe stream ended without PUBLISH_DONE");
			const { type } = framed;
			if (type === PublishDone.id) break;
			const answer = (type === RequestOk.id || type === RequestError.id) && !refused && unanswered.peek() > 0;
			if (!answer) throw new ProtocolViolation(`unexpected message on a subscription: 0x${type.toString(16)}`);
			if (type === RequestOk.id) {
				await RequestOk.decode(framed.reader, version);
				unanswered.update((count) => count - 1);
			} else {
				const err = await RequestError.decode(framed.reader, version);
				console.debug(`subscription update refused: ${err.reasonPhrase}`);
				refused = true;
			}
		}
		const done = await PublishDone.decode(framed.reader, version);
		// A revocation reads the same as one from a moq-lite peer.
		if (done.statusCode === PublishDoneStatus.UNAUTHORIZED) throw unauthorized(broadcast);
		if (!publishDoneClean(done.statusCode, version)) {
			throw new Error(`publish done: status=0x${done.statusCode.toString(16)} reason=${done.reasonPhrase}`);
		}
		const count = done.streamCount;

		const { tail, track, join } = subscription;
		const complete = () => count > 0n && BigInt(tail.streams) >= count;
		await tail.settle(complete, hooks.trackReleased(track));
		// SUBSCRIPTION_ENDED ends just this subscription, so the copy fails rather than declare an
		// end the track may run past: a later reader subscribes afresh, and its groups continue.
		// A head nothing claimed may be run past the same way, so it is cancelled, not finished.
		if (done.statusCode !== PublishDoneStatus.TRACK_ENDED) {
			throw new Error(`publish done: subscription ended reason=${done.reasonPhrase}`);
		}
		// The track ended and every stream is accounted for, so no tail is coming: a head nothing
		// claimed is its whole group. A draft-18 joining FETCH is not in the Stream Count; its
		// head held the settle.
		if (hooks.trackReleased(track).peek() === undefined) join?.end();
	}

	/**
	 * Follow the aggregate subscriber priority upstream until `done` settles with REQUEST_UPDATE,
	 * each under a Request ID of its own. The answer comes back on this stream, so one update is
	 * in flight at a time and changes meanwhile coalesce. No draft lets an update change the
	 * group order, so it stays as asked.
	 */
	async #runUpdates(
		stream: RequestStream,
		requestId: bigint,
		producer: track.Producer,
		sent: number,
		sentMaxAge: number | undefined,
		unanswered: Signal<number>,
		done: Promise<unknown>,
	): Promise<void> {
		const version = this.#session.version;
		const stopped = done.then(
			() => null,
			() => null,
		);
		const stop = new AbortController();
		void stopped.then(() => stop.abort());

		for (;;) {
			// With no subscriber left there is nothing to follow: the subscription is ending, or
			// keeps what it last asked for until one returns, as on lite and in Rust.
			const subscription = producer.subscription.peek();
			const wanted = toWire(subscription?.priority ?? 0);
			const wantedMaxAge = this.#maxAge ? subscription?.maxDelay : undefined;
			if (!subscription || (wanted === sent && wantedMaxAge === sentMaxAge) || unanswered.peek() > 0) {
				const disposes: Dispose[] = [];
				const wake = new Promise<void>((resolve) => {
					disposes.push(
						producer.subscription.changed(() => resolve()),
						unanswered.changed(() => resolve()),
					);
				});
				await race([wake, stopped]);
				for (const dispose of disposes) dispose();
				if (stop.signal.aborted) return;
				continue;
			}

			let own: bigint | undefined;
			try {
				own = await this.#session.nextRequestId(stop.signal);
			} catch {
				return;
			}
			if (own === undefined) return;

			const parameters = new Parameters();
			parameters.subscriberPriority = wanted;
			parameters.maxAge = wantedMaxAge;
			const update = new SubscribeUpdate({ requestId: own, parameters });
			let message: Uint8Array;
			try {
				message = await Message.frame(stream.writer.version, SubscribeUpdate.id, update, version);
			} catch (err) {
				// Nothing reached the wire, but the track can no longer follow its subscribers.
				console.warn(`subscribe update failed: id=${requestId} error=${reason(error(err))}`);
				producer.abort(error(err));
				return;
			}
			// Counted before writing: the answer can be read before the write's promise resolves.
			unanswered.update((count) => count + 1);
			try {
				await stream.writer.write(message);
			} catch {
				// The publisher stopped reading; its responses decide how the track ends.
				return;
			}
			sent = wanted;
			sentMaxAge = wantedMaxAge;
			console.debug(`subscribe update: id=${requestId} priority=${wanted}`);
		}
	}

	/**
	 * Fetch the head of a draft-18 join's group, and keep the request open until the head is
	 * in or the subscription ends.
	 *
	 * It asks for the group SUBSCRIBE_OK names as Largest, from object 0 up to Largest, with a
	 * Relative Joining FETCH (offset 0) on its own request, outside PUBLISH_DONE's Stream Count.
	 * The objects arrive on a fetch stream that {@link handleFetch} writes into the
	 * {@link Join}. As in the Rust subscriber, a refusal or a failed request costs only the
	 * head, and the request has no deadline of its own: it lives as long as the subscription.
	 * A malformed answer closes the session, as on any request.
	 */
	async #runJoiningFetch(subscription: Subscription, subscribeId: bigint, priority: number, signal: AbortSignal) {
		const join = subscription.join;
		if (!join) return;
		const version = this.#session.version;
		let request: RequestStream | undefined;
		try {
			const fetchId = await this.#session.nextRequestId(signal);
			if (fetchId === undefined) throw await this.#closedSession();
			// Registered before the FETCH goes out: its stream can overtake FETCH_OK. It stays
			// until the subscription ends, so a head that lands after a whole group won the
			// sequence is read away rather than refused.
			this.#fetches.set(fetchId, Promise.resolve(subscription));
			subscription.fetchIds.push(fetchId);

			request = new RequestStream(
				await untilAborted(Promise.resolve(this.#session.openBi(requestOptions)), signal),
				this.#session,
				this.#quic,
			);
			const msg = new Fetch({
				requestId: fetchId,
				type: { kind: "relativeJoining", subscription: subscribeId, offset: 0n },
				subscriberPriority: priority,
				groupOrder: "ascending",
			});
			await Message.write(request.writer, Fetch.id, msg, version);
			console.debug(`joining fetch: id=${fetchId} group=${join.sequence}`);

			const response = await untilAborted(request.next(), signal);
			if (response === undefined) throw new Error("joining FETCH ended before its response");
			if (response.type === RequestError.id) {
				const err = await RequestError.decode(response.reader, version);
				const why = requestReason(err.errorCode, err.reasonPhrase, "fetch", version);
				throw new Error(`joining FETCH refused: ${why}`);
			}
			if (response.type !== FetchOk.id) {
				throw new ProtocolViolation(`unexpected answer to FETCH: 0x${response.type.toString(16)}`);
			}
			try {
				await FetchOk.decode(response.reader, version);
			} catch (err) {
				throw new ProtocolViolation(`malformed FETCH_OK: ${reason(error(err))}`);
			}

			// The answer is in; the head arrives on the fetch stream, or never. The request stays
			// read meanwhile: a reset means its fetch stream cannot be trusted to come, while a
			// FIN only ends the publisher's half.
			const ended = request.next().then((framed) => {
				if (framed !== undefined) {
					throw new ProtocolViolation(`unexpected message after FETCH_OK: 0x${framed.type.toString(16)}`);
				}
				return join.settled;
			});
			await untilAborted(Promise.race([join.settled, ended]), signal);
			request.close();
		} catch (err) {
			const e = error(err);
			if (e instanceof ProtocolViolation) {
				request?.abort(e);
				this.#fail(e.code, e.message);
			} else if (signal.aborted) {
				request?.cancel();
				request?.close();
			} else {
				request?.abort(e);
				console.debug(`joining FETCH failed, joining at the next whole group: ${reason(e)}`);
			}
			join.refuse(e);
		}
	}

	// Opens the subscribe stream, sends SUBSCRIBE, and reads the response.
	// `state` is populated as soon as the stream opens and again when the
	// trackAlias is registered, so the caller can clean both up on timeout
	// even before this promise settles.
	async #openSubscribe(
		state: SubscribeSetupState,
		broadcast: Path.Valid,
		request: track.Request,
		requestId: bigint,
		deadline: Deadline,
		epoch: Epoch.Valid | undefined,
	): Promise<{
		stream: RequestStream;
		subscription: Subscription;
		alias: bigint;
		priority: number;
		maxAge?: number;
	}> {
		const version = this.#session.version;

		state.stream = new RequestStream(
			await deadline.open(this.#session.openBi(requestOptions)),
			this.#session,
			this.#quic,
		);

		// The timeout can fire while the open is still in flight, in which case cleanup ran
		// with nothing to release. Bail now that the stream is recorded, so settling this
		// promise hands it back to be torn down.
		if (state.cancelled) throw new Error("subscribe cancelled before it was sent");

		const msg = new Subscribe({
			requestId,
			trackNamespace: broadcast,
			trackName: request.name,
			// Read now, since a subscriber can join while the stream opened. Updates follow it.
			subscriberPriority: toWire(request.priority),
			maxAge: this.#maxAge ? (request.subscription.maxDelay ?? 0) : undefined,
			// Fixed for the subscription's life: no REQUEST_UPDATE parameter carries it.
			groupOrder: request.subscription.order,
			// Joins at the current group, from its first object: the subscription delivers the
			// objects after Largest, and the group's head before it comes on a fetch stream
			// (see {@link Join}). Fixed for the subscription's life.
			...joinFilter(version),
			epoch,
		});
		if (msg.fill) {
			// The fill names this request and can overtake SUBSCRIBE_OK, so it waits on it here.
			const accepted = Promise.withResolvers<Subscription | undefined>();
			this.#fetches.set(requestId, accepted.promise);
			state.fill = accepted.resolve;
		}
		await Message.write(state.stream.writer, Subscribe.id, msg, version);
		state.sent = true;

		// The caller may have given up while that write was in flight. It deliberately left
		// the stream alone, so unwind here and let its cleanup send the cancellation now that
		// the SUBSCRIBE has actually reached the peer.
		if (state.cancelled) throw new Error("subscribe cancelled while it was being sent");
		console.debug(`subscribe written: id=${requestId} broadcast=${broadcast} track=${request.name}`);

		const response = await state.stream.next();
		if (response === undefined) throw new Error("subscription ended before its response");
		if (response.type !== SubscribeOk.id) {
			let reasonPhrase = "unknown error";
			try {
				if (response.type === RequestError.id) {
					const err = await RequestError.decode(response.reader, version);
					reasonPhrase = requestReason(err.errorCode, err.reasonPhrase, "subscribe", version);
					// Only a well-formed refusal ended the request; cleanup resets anything else.
					state.rejected = true;
				}
			} catch {
				// Decoding error response failed, use default message
			}
			throw new Error(`SUBSCRIBE error: ${reasonPhrase}`);
		}

		const ok = await SubscribeOk.decode(response.reader, version);
		if (state.cancelled) throw new Error("subscribe cancelled before acceptance");
		const maxCacheDuration = ok.properties.maxCacheDuration;
		if (maxCacheDuration !== undefined && maxCacheDuration > BigInt(Number.MAX_SAFE_INTEGER)) {
			throw new RangeError("max cache duration exceeds safe milliseconds");
		}
		if (ok.largest && ok.largest.groupId > BigInt(Number.MAX_SAFE_INTEGER)) {
			throw new RangeError("largest group exceeds safe integers");
		}
		// Staged until the answer is written in below, so no reader sees the track before its
		// Largest and start.
		const staged = hooks.stageRequest(request, {
			// No TIMESCALE means no timeline, and the track must not claim one when served onward.
			timescale: ok.properties.timescale,
			priority: fromWire(ok.properties.priority ?? DEFAULT_WIRE),
			maxAge: maxCacheDuration === undefined ? undefined : Milli(Number(maxCacheDuration)),
			// A moq-transport peer may stamp arrival times, and nothing on the wire says it didn't.
			ietf: version,
		});
		const producer = staged.producer;
		const tail = new Tail();
		const subscription: Subscription = { track: producer, tail, timescale: ok.properties.timescale, fetchIds: [] };
		// The subscription starts after Largest, so the joining FETCH or the fill brings its
		// group's head. With no Largest the track is empty and every group whole. A draft-22 fill
		// is one of the subscription's own streams, so the Stream Count already waits for it.
		if (ok.largest) {
			subscription.join = new Join(Number(ok.largest.groupId), tail, { counted: msg.fill !== undefined });
		}
		if (msg.fill) subscription.fetchIds.push(requestId);
		state.subscription = subscription;
		if (ok.largest) {
			// Object IDs can start past zero or skip, while readers see frame ordinals, so only
			// the group carries over: its first frame is the edge a reader can count on. It names
			// no object, so a Next Object join served onward still starts at the group's first.
			hooks.reportedGroup(producer, Number(ok.largest.groupId));
		}
		// The live join starts at the publisher's edge, whatever floor the demand asked for.
		// With no Largest it skips nothing, so every group is owed: a lower one's stream may
		// still arrive after a higher one's.
		const groups = request.subscription.groups;
		hooks.startTrack(producer, {
			start: ok.largest ? Number(ok.largest.groupId) : 0,
			requested: groups?.start === undefined ? undefined : groupBounds(groups).start,
		});
		staged.commit();
		state.fill?.(subscription);

		try {
			this.#aliases.set(ok.trackAlias, subscription, { broadcast, name: request.name, epoch });
			const timescale = ok.properties.timescale;
			if (timescale !== undefined) {
				this.#timescales.set(ok.trackAlias, timescale);
			}
		} catch (err) {
			// Only one alias naming two different tracks is the session's problem. A publisher
			// sharing an alias between subscriptions to one track is allowed to do that, and
			// disconnecting over it would drop every other broadcast on the session.
			if (err instanceof DuplicateTrackAlias) this.#fail(SessionCode.ProtocolViolation, err.message);
			throw err;
		}
		state.registeredAlias = ok.trackAlias;
		return {
			stream: state.stream,
			subscription,
			alias: ok.trackAlias,
			priority: msg.subscriberPriority,
			maxAge: msg.maxAge,
		};
	}

	/**
	 * Handles an incoming PUBLISH_NAMESPACE on a bidi stream.
	 * Tracks announced broadcasts and notifies consumers.
	 *
	 * @internal
	 */
	runPublishNamespace(msg: PublishNamespace, stream: Stream): Promise<void> {
		const request = new RequestStream(stream, this.#session, this.#quic);
		return request.serve(this.#runPublishNamespace(msg, request));
	}

	async #runPublishNamespace(msg: PublishNamespace, request: RequestStream) {
		if (Namespace.reserved(msg.trackNamespace, this.#session.version)) {
			await refuseReserved(request);
			return;
		}
		const version = this.#session.version;
		const path = msg.trackNamespace;

		// A path that already ran through us looped back. Refuse it rather than holding an
		// advertisement we could never subscribe through. The peer knows our Hop ID, so a
		// conforming one never offers it; 0 as the retry interval says not to come back.
		if (this.#reflected(msg.params.cluster)) {
			console.debug(`dropping reflected publish_namespace: broadcast=${path}`);
			await request.refuse("publish_namespace", {
				condition: "uninterested",
				reasonPhrase: "route loops back",
			});
			return;
		}

		// A path this session already knows is not refused: the same namespace can reach us
		// twice, and each advertisement is tracked apart from news. This request owns exactly
		// one of those references, named by its id while attached, and gives it back when the
		// stream ends.
		this.#announceSlots.acquire();
		let attached: number | undefined;
		// Fixed for the advertisement's life: no REQUEST_UPDATE carries one.
		const epoch = msg.params.epoch;

		try {
			// Send OK first. This must complete before notifying consumers,
			// because consumers may trigger Subscribe writes.
			await Message.write(request.writer, RequestOk.id, new RequestOk(), version);

			// Only now is the advertisement ours to announce, for the reason above: a
			// consumer reacting with a SUBSCRIBE must not interleave with that OK.
			attached = this.#attachAnnounce(path, this.#route(msg.params));

			// An advertisement is updated in place with REQUEST_UPDATE on the stream that
			// already carries it, so read until the stream ends rather than waiting on the
			// close. Nothing else would deliver a re-parented route. What the peer holds
			// is kept current, since an update carries only what changed.
			let held = msg.params.cluster;
			for (;;) {
				const framed = await race([request.next(), request.ended.then(() => undefined)]);
				if (framed === undefined) {
					// FIN cancels only on some drafts; on the rest the request lasts until reset.
					await request.ended;
					break;
				}

				// A repeated PUBLISH_NAMESPACE lands here too: a second request on the
				// stream is the base draft's duplicate request ID.
				if (framed.type !== PublishNamespaceUpdate.id) {
					throw new ProtocolViolation(
						`unexpected message on publish_namespace stream: 0x${framed.type.toString(16)}`,
					);
				}

				let update: PublishNamespaceUpdate;
				try {
					update = await PublishNamespaceUpdate.decode(framed.reader, version);
				} catch (err) {
					if (!(err instanceof MalformedAuthToken)) throw err;
					// A refused update's request ID is still checked.
					if (err.requestId !== undefined) this.#session.checkRequestId(err.requestId);
					// Section 8.9 rejects the update, which withdraws the advertisement as any
					// refused update does.
					await refuseUpdate(request.writer, version, "publish_namespace", {
						condition: "malformed_auth_token",
						reasonPhrase: err.message,
					});
					request.close();
					break;
				}
				this.#session.checkRequestId(update.requestId);

				// The parameters exist only on a session that negotiated the extension;
				// anywhere else they are the peer's violation.
				if (held === undefined) {
					if (update.update.hops !== undefined || update.update.cost !== undefined) {
						throw new ProtocolViolation("cluster parameters on a session that negotiated none");
					}
				} else {
					// A different original publisher applies in place too, as it does inline.
					held = Cluster.apply(held, update.update);
				}

				// A path that now runs through us is unusable, so give it back. The update
				// itself is accepted, and reading continues: this stream is the
				// advertisement's only channel, so a later clean path arrives here or
				// nowhere.
				if (this.#reflected(held)) {
					if (attached !== undefined) {
						console.debug(`publish_namespace now loops back, detaching: broadcast=${path}`);
						this.#detachAnnounce(path, attached);
						attached = undefined;
					}
				} else if (attached === undefined) {
					// Re-attach: a clean path replaced the reflected one we detached from.
					attached = this.#attachAnnounce(path, this.#route({ cluster: held, epoch }));
				} else {
					this.#updateAnnounce(path, attached, this.#route({ cluster: held, epoch }));
				}

				// Nothing here can fail to apply, so every update is acknowledged. A leaf
				// routes nothing, so a repricing changes nothing it holds; consumers still
				// hear the new route so a forwarder can reprice.
				await Message.write(request.writer, RequestOk.id, new RequestOk(), version);
			}
		} finally {
			this.#announceSlots.release();

			// Give back exactly what was taken: a request that never got its OK out never
			// referenced the path, and retracting there would drop someone else's count.
			if (attached !== undefined) {
				this.#detachAnnounce(path, attached);
			}
		}
	}

	/**
	 * Handles an incoming PUBLISH on a bidi stream.
	 * We don't support reverse publish, so send error.
	 *
	 * @internal
	 */
	runPublish(msg: Publish, stream: Stream): Promise<void> {
		const request = new RequestStream(stream, this.#session, this.#quic);
		// We decline the method itself rather than this particular track, which would be
		// UNINTERESTED.
		//
		// The alias the message carries is deliberately not recorded. Nothing will ever bind
		// it, and a rejected request has no lifetime of ours to hang the cleanup on.
		return request.serve(
			Namespace.reserved(msg.trackNamespace, this.#session.version)
				? refuseReserved(request)
				: request.refuse("publish", {
						condition: "not_supported",
						reasonPhrase: "publish not supported",
					}),
		);
	}

	/**
	 * Handles an ObjectStream message (group + frames on uni stream).
	 *
	 * @internal
	 */
	async handleGroup(group: GroupMessage, stream: Reader) {
		if (group.subGroupId !== 0) {
			throw new Error("subgroups are not supported");
		}

		let subscription: Subscription;
		try {
			// The control message establishing this alias can arrive after the data stream.
			subscription = await this.#aliases.get(group.trackAlias);
		} catch (err: unknown) {
			if (err instanceof ProtocolViolation) this.#fail(err.code, err.message);
			const e = await sessionCause(this.#quic, err);
			// Ours: we cancelled the subscription and the publisher has not stopped yet.
			// Anything else on this alias is the publisher sending data for a track it never
			// acknowledged, which is worth seeing.
			if (e instanceof RetiredTrackAlias) {
				console.debug(`dropping group for a cancelled subscription: alias=${group.trackAlias}`);
			}
			stream.stop(e);
			return;
		}

		const { track, tail } = subscription;
		// Every data stream counts toward PUBLISH_DONE's Stream Count, even one dropped below.
		const read = tail.open(group.groupId);

		// Created on the first object rather than the header: an END_OF_TRACK at object 0
		// means the group does not exist at all.
		let producer: netGroup.Producer | undefined;
		const open = () => {
			if (!producer) {
				// The publisher contradicted its own end, which no later group can repair.
				if (subscription.end !== undefined && group.groupId >= subscription.end) {
					throw new ProtocolViolation(
						`group ${group.groupId} is at or past the declared end ${subscription.end}`,
					);
				}
				producer = track.createGroup(group.groupId);
			}
			return producer;
		};

		try {
			// The alias binds after SUBSCRIBE_OK commits the track property; an omitted
			// header priority inherits it (draft-22 section 10.4).
			if (!group.flags.hasPriority) group.publisherPriority = toWire((await track.info()).priority);

			const timescale = this.#timescales.get(group.trackAlias);
			// A stream for the group a join is fetching the head of may start partway
			// through it, at the Object ID the head ends before (see Join).
			const join = subscription.join?.sequence === group.groupId ? subscription.join : undefined;
			let first = join?.outstanding === true;
			const decode = (c: Cursor) => Frame.decode(c, group.flags, timescale, first);
			let nextObject = 0;
			for (;;) {
				// Every object already buffered is written without an await, so the reader wakes
				// once per batch rather than once per object. Only the group's own stream ends it:
				// a track that closes first has already closed (or aborted) this group through its
				// cache.
				const frame =
					stream.tryDecode(decode) ??
					(await race([stream.decodeMaybe(decode), producer?.closed ?? hooks.trackReleased(track)]));
				if (!producer && (frame === null || frame instanceof Error)) {
					stream.stop(frame ?? new StreamError(StreamCode.Cancel));
					return;
				}
				if (first && join) {
					first = false;
					const suspend = () => tail.suspend();
					// No producer yet, so a closing group or track already returned above.
					const object = frame instanceof Error ? undefined : (frame ?? undefined);
					// The first object's ID is absolute: where in the group this stream starts. An End
					// of Group there names where the group ends, past the head's last object.
					const start =
						object?.payload !== undefined || object?.endOfTrack || (object?.idDelta ?? 0) > 0
							? object?.idDelta
							: undefined;
					if (start === 0 && object?.payload !== undefined) {
						// The whole group: it stands alone unless the head already took the sequence.
						if (!join.whole()) {
							stream.stop(new Error("the group's head was served twice"));
							return;
						}
					} else if (start === undefined || start > 0) {
						// A tail with no objects ends the group wherever the head did.
						const head = await join.claim(start, suspend);
						if (head) {
							producer = head.producer;
							nextObject = head.next;
							if (object) object.idDelta = 0;
						} else if (start !== undefined && !object?.endOfTrack) {
							console.debug(
								`dropping a group with no head: alias=${group.trackAlias} group=${group.groupId} start=${start}`,
							);
							stream.stop(new Error("no head to stitch a mid-group stream onto"));
							return;
						}
					}
				}
				if (!frame || frame instanceof Error) {
					// FIRST_OBJECT clear on a stream that ends with no object: it starts partway
					// through its group and carries none of it, so drop it like any headless
					// stream.
					if (!producer && !group.flags.firstObject) {
						console.debug(
							`dropping a group with no head: alias=${group.trackAlias} group=${group.groupId}`,
						);
						stream.stop(new Error("a group must start at object 0"));
						return;
					}
					break;
				}

				if (frame.endOfTrack) {
					if (frame.hasProperties) throw new EndOfTrackProperties();
					// No object at or past this location exists: after the group's last object
					// the track ends with it, and at object 0 it ends before it.
					if (producer && frame.idDelta !== 0) throw new ObjectIdGap(frame.idDelta);
					const end = { group: group.groupId, frame: nextObject + frame.idDelta };
					producer?.finish();
					try {
						hooks.endTrack(track, end);
					} catch (err: unknown) {
						throw new ProtocolViolation(`invalid END_OF_TRACK: ${reason(error(err))}`);
					}
					subscription.end ??= finalSequence(end);
					return;
				}
				if (frame.payload === undefined) break;

				// A track that declared TIMESCALE stamps every object, so one without a Timestamp is
				// malformed rather than something to invent a time for.
				if (timescale !== undefined && frame.timestamp === undefined) {
					throw new StreamError(StreamCode.MalformedTrack, {
						message: `object without a Timestamp on a track with TIMESCALE: group=${group.groupId}`,
					});
				}
				open().writeFrame({ payload: frame.payload, timestamp: frame.timestamp });
				nextObject += 1;
			}

			// A group with no objects still exists.
			open().finish();
		} catch (err: unknown) {
			if (err instanceof ProtocolViolation) this.#fail(err.code, err.message);
			const e = await sessionCause(this.#quic, err);
			// The compact form names the subgroup by its first Object ID, so a nonzero one is
			// subgroup N: stop the stream before any group exists for it.
			if (producer === undefined && e instanceof ObjectIdGap && group.flags.hasSubgroupObject) {
				console.warn(`subgroups are not supported: alias=${group.trackAlias} group=${group.groupId}`);
				stream.stop(new Error("subgroups are not supported"));
				return;
			}
			// The producer is still unopened only when the first object failed. With FIRST_OBJECT
			// clear, that non-zero delta is a headless group: drop the stream and leave the
			// subscription up for the next group. Delivering the object would renumber a P-frame
			// as the keyframe the group opens with.
			if (producer === undefined && e instanceof ObjectIdGap && !group.flags.firstObject) {
				console.debug(`dropping a group with no head: alias=${group.trackAlias} group=${group.groupId}`);
				stream.stop(new Error("a group must start at object 0"));
				return;
			}
			if (e instanceof ObjectIdGap) console.warn(`unsupported object gap: ${reason(e)}`);
			if (e instanceof EndOfTrackProperties) {
				// The group before the marker is complete; only the track's end can't be carried.
				producer?.finish();
				track.abort(e);
			} else if (
				e instanceof ProtocolViolation ||
				(e instanceof StreamError && e.code === StreamCode.MalformedTrack)
			) {
				// The publisher broke the track's end or its content, which no later group can repair.
				producer?.abort(e);
				track.abort(e);
			} else {
				// A stream that fails before its first object still names a group, which the
				// reader sees fail rather than silently go missing.
				try {
					open().abort(e);
				} catch {
					// The track has already closed or ended below this group.
				}
			}
			stream.stop(e);
		} finally {
			read();
		}
	}

	/**
	 * Handles a fetch stream, with its type already read: the head of a join, answering a
	 * draft-18 joining FETCH or a draft-22 fill.
	 *
	 * Either asked for one group from object 0, so the first object must spell
	 * `{G, 0}` and every later one follow it in that group. The head is created as soon as its
	 * first object arrives and written as the objects do, so readers see it at once; the live
	 * tail takes the group over through the {@link Join}. Anything the head cannot stitch,
	 * another group or a gap among them, drops it, which costs only that group's head.
	 *
	 * @internal
	 */
	async handleFetch(stream: Reader) {
		const version = this.#session.version;
		const { requestId } = await FetchHeader.decode(stream, version);
		const subscription = await this.#fetches.get(requestId);
		// Nothing of ours asked: the publisher is answering a FETCH or fill we never sent.
		if (!subscription?.join) throw new Error(`unexpected fetch stream: request=${requestId}`);
		const { join, track, tail, timescale } = subscription;
		// A draft-22 fill is one of the subscription's own data streams, which PUBLISH_DONE
		// counts. A draft-18 joining FETCH is a request of its own; the join holds the end for it.
		const read = isDraft22(version) ? tail.open(join.sequence) : undefined;

		let producer: netGroup.Producer | undefined;
		// A whole group took the sequence first, or the join gave up: read the rest away.
		let discarding = false;
		let next = 0;
		try {
			const decode = (c: Cursor) => decodeFetchObject(c, timescale);
			for (let first = true; ; first = false) {
				const object =
					stream.tryDecode(decode) ??
					(await race([stream.decodeMaybe(decode), producer?.closed ?? hooks.trackReleased(track)]));
				if (object === null || object instanceof Error) {
					join.fail(object ?? new Error("the track closed under the head"));
					stream.stop(object ?? new StreamError(StreamCode.Cancel));
					return;
				}
				if (object === undefined) break;
				if (discarding) continue;

				if (object.kind === "endOfRange") {
					throw new JoinUnsupported("a head with an End of Range cannot be stitched");
				}
				const inherits =
					object.group === undefined ||
					object.object === undefined ||
					object.priority === undefined ||
					object.subgroup === "prior" ||
					object.subgroup === "next";
				if (first && inherits) throw new ProtocolViolation("the first fetch object refers to a prior object");
				if (object.subgroup === "datagram") throw new JoinUnsupported("a datagram group is not fetchable");
				if (object.subgroup === "next" || (object.subgroup === "explicit" && object.subgroupId !== 0)) {
					throw new JoinUnsupported("subgroups are not supported");
				}

				// The first object names its group and Object ID outright; a later Group ID names
				// another group, which a head of one group does not have. A later Object ID Delta
				// is added to the prior ID, so only 1 is the next object.
				if (first) {
					if (object.group !== join.sequence) {
						throw new JoinUnsupported(`the head answered group ${object.group}, not ${join.sequence}`);
					}
					if (object.object !== 0) throw new JoinUnsupported("the head does not start at object 0");
				} else if (object.group !== undefined) {
					throw new JoinUnsupported("a head spanning several groups cannot be stitched");
				} else if (object.object !== undefined && object.object !== 1) {
					throw new JoinUnsupported("a gap in the head cannot be stitched");
				}

				if (timescale !== undefined && object.timestamp === undefined) {
					throw new StreamError(StreamCode.MalformedTrack, {
						message: `object without a Timestamp on a track with TIMESCALE: group=${join.sequence}`,
					});
				}

				if (first) {
					producer = join.open(() => track.createGroup(join.sequence));
					if (!producer) {
						discarding = true;
						continue;
					}
				}
				producer?.writeFrame({ payload: object.payload, timestamp: object.timestamp });
				join.wrote();
				next += 1;
			}
			console.debug(`join head: group=${join.sequence} objects=${next}`);
			join.finish();
		} catch (err: unknown) {
			const e = await sessionCause(this.#quic, err);
			if (e instanceof JoinUnsupported) console.warn(`${e.message}, joining at the next whole group`);
			join.fail(e);
			if (e instanceof ProtocolViolation) this.#fail(e.code, e.message);
			// As for a subgroup object: the track is malformed, not just its head.
			if (e instanceof StreamError && e.code === StreamCode.MalformedTrack) track.abort(e);
			stream.stop(e);
		} finally {
			read?.();
		}
	}

	/**
	 * Receive QUIC datagrams, each an OBJECT_DATAGRAM for one of our subscriptions.
	 *
	 * Returns at once on a transport without datagrams, and once the datagram stream ends or
	 * fails. A malformed datagram throws a {@link ProtocolViolation}, which ends the session.
	 *
	 * @internal
	 */
	async runDatagrams(): Promise<void> {
		if (!this.#quic || DatagramStream.maxDatagramSize(this.#quic) === 0) return;
		const reader = DatagramStream.datagramReader(this.#quic);
		if (!reader) return;

		try {
			for (;;) {
				// The stream errors once the session closes, which ends this loop like any other.
				const next = await reader.read().catch(() => undefined);
				if (!next || next.done) return;
				await this.#recvDatagram(next.value);
			}
		} finally {
			reader.releaseLock();
		}
	}

	/**
	 * Deliver one OBJECT_DATAGRAM as a datagram on its subscription's track: a single-frame
	 * group at the Group ID.
	 *
	 * One the model cannot carry is dropped like any lost datagram: an Object past ID 0 (the
	 * group would need a second object), a status other than Normal, an alias that is not
	 * bound yet (the draft lets us drop rather than buffer), or no Timestamp on a track that
	 * declared a timescale.
	 */
	async #recvDatagram(data: Uint8Array): Promise<void> {
		const version = this.#session.version;
		const datagram = await ObjectDatagram.decode(data, version);
		const { trackAlias: alias, groupId: sequence } = datagram;

		if ((datagram.objectId ?? 0) !== 0) {
			console.debug(`dropping a datagram past object 0: alias=${alias} group=${sequence}`);
			return;
		}
		let payload: Uint8Array;
		if ("status" in datagram.body) {
			if (datagram.body.status !== 0) {
				console.debug(
					`dropping a datagram status: alias=${alias} group=${sequence} status=${datagram.body.status}`,
				);
				return;
			}
			payload = new Uint8Array();
		} else {
			payload = datagram.body.payload;
		}

		const subscription = this.#aliases.peek(alias);
		if (!subscription) {
			console.debug(`dropping a datagram for an unbound alias: alias=${alias} group=${sequence}`);
			return;
		}

		// Like a subgroup object: a track that declared no timescale is untimed.
		const timescale = this.#timescales.get(alias);
		let timestamp: Timestamp | undefined;
		if (timescale !== undefined && datagram.properties !== undefined) {
			try {
				timestamp = await new Reader(undefined, datagram.properties, version).decode((c) =>
					decodeObjectTime(c, timescale),
				);
			} catch (err: unknown) {
				throw new ProtocolViolation(`malformed OBJECT_DATAGRAM properties: ${reason(error(err))}`, {
					cause: err,
				});
			}
		}
		if (timescale !== undefined && timestamp === undefined) {
			console.debug(`dropping an unstamped datagram: alias=${alias} group=${sequence}`);
			return;
		}

		try {
			subscription.track.insertDatagram(sequence, timestamp, payload);
		} catch (err: unknown) {
			console.debug(`dropping datagram: alias=${alias} group=${sequence} error=${reason(error(err))}`);
		}
	}
}

/**
 * A broadcast consumed from a moq-transport session. Track info is resolved by the
 * subscribe path (the inherited `resolveTrackInfo`), but this client has no one-shot
 * group fetch, so the wire offers none and `track.Consumer.fetchGroup()` is rejected.
 */
class ConsumeBroadcast extends broadcast.Consumer {
	constructor(state?: never) {
		super(state);
		overrideBroadcastWire(this, { fetchGroup: undefined });
	}

	// Preserve the subclass when the consume cache shares this broadcast across callers.
	override clone(): ConsumeBroadcast {
		return new ConsumeBroadcast(this.shareState());
	}
}
