import { Milli } from "@moq/clock";
import { type Dispose, Effect, type GetPromise, type Getter, race, Signal } from "@moq/signals";
import * as announce from "../announced.ts";
import type { Grant } from "../auth.ts";
import type { AuthSession } from "../auth_session.ts";
import * as broadcast from "../broadcast.ts";
import type { Drain } from "../connection/goaway.ts";
import type { Probe as ProbeStats } from "../connection/stats.ts";
import { BroadcastCache } from "../consume.ts";
import * as DatagramStream from "../datagram_stream.ts";
import type * as Epoch from "../epoch.ts";
import {
	error,
	ProtocolViolation,
	reason,
	SessionCode,
	SessionError,
	StreamCode,
	StreamError,
	sessionCause,
	unauthorized,
} from "../error.ts";
import * as netGroup from "../group.ts";
import { Cost, type Hop, MAX_HOPS, type Route, routesEqual, UNKNOWN_HOP } from "../hop.ts";
import { groupBounds, hiddenBelow, hooks, scopeCaptures, scopeHead, scopeOverlaps } from "../internal.ts";
import { grace } from "../ledger.ts";
import type { Lister } from "../listing.ts";
import * as Path from "../path.ts";
import { sendOrder } from "../priority.ts";
import { type OpenOptions, type Reader, Stream, Writer } from "../stream.ts";
import { Tail } from "../tail.ts";
import * as Time from "../time.ts";
import type * as track from "../track.ts";
import { untilAborted } from "../util/abort.ts";
import { closeUnwanted } from "../util/announce.ts";
import { Deadline, REQUEST_TIMEOUT_MS, requestOptions } from "../util/deadline.ts";
import { abandoned, isAbandoned, unwanted } from "../util/demand.ts";
import { LIMITS, type Limits, SlotMap, Slots } from "../util/limits.ts";
import { Budget } from "../util/yield.ts";
import { overrideBroadcastWire, wireOf } from "../wire.ts";
import {
	AnnounceHistory,
	AnnounceInit,
	AnnounceOk,
	AnnounceRequest,
	decodeAnnounceBroadcastMaybe,
} from "./announce.ts";
import { Datagram as DatagramMessage } from "./datagram.ts";
import { closeViolation } from "./error.ts";
import { Fetch as FetchMessage } from "./fetch.ts";
import { frameDecoder, type Group as GroupMessage, readFrames, type WireFrame } from "./group.ts";
import { Probe } from "./probe.ts";
import { ProbeLevel, type Setup } from "./setup.ts";
import { StreamId } from "./stream.ts";
import {
	decodeSubscribeResponse,
	decodeSubscribeResponseMaybe,
	EMPTY_RANGE,
	emptyRange,
	exclusiveGroupEnd,
	inclusiveGroupEnd,
	Subscribe,
	SubscribeUpdate,
} from "./subscribe.ts";
import { TrackInfo, Track as TrackMessage } from "./track.ts";
import { decodeTracksEventMaybe, Tracks as TracksMessage } from "./tracks.ts";
import {
	hasAnnounceId,
	hasAnnounceOk,
	hasDatagrams,
	hasFetchMaxDelay,
	hasMediaTime,
	hasOrder,
	hasProbeRtt,
	hasRanges,
	hasRefresh,
	hasSetupStream,
	hasSynced,
	hasTrackStream,
	hasTracks,
	resolvesStart,
	updateSupported,
	Version,
	waitsForSubscriberFin,
} from "./version.ts";

// A peer's buffered TRACKS events decode without reaching a task boundary, and a flood of adds
// and removals of one name never reaches the announces cap: yield so it cannot starve the host.
const listingBudget = new Budget();

// Request streams refuse exhausted peer credit, with one deadline covering create and answer.
export const SUBSCRIBE_SETUP_TIMEOUT_MS = REQUEST_TIMEOUT_MS;

// Wire ceiling (2^62-1). A draining session stamps it on every live route so any other
// candidate outranks it, while the route stays selectable as the last path. Matches Rust
// Cost::DRAIN: cost is the whole mechanism, not a separate state.
const DRAIN_COST: Cost = 2n ** 62n - 1n;

// What a subscription has set up so far, so a revocation (or a timeout) can reach it at
// any stage.
interface SubscribeSetup {
	stream?: Stream;
	track?: Stream;
	producer?: track.Producer;
	// Ends a setup still running once the deadline passed or the grant was revoked.
	cancel: AbortController;
	// Set once our grant stops covering the broadcast.
	revoked?: Error;
}

/**
 * What a consumed broadcast captured when it was opened: the epoch it asks for, and its own id.
 * Fetches are shared only within one consume, so a fresh consume for a replaced instance never
 * reads a FETCH the old one opened.
 */
interface Consumed {
	readonly epoch?: Epoch.Valid;
	readonly id?: number;
	readonly valid?: AbortSignal;
	// Aborted once another instance takes the path, which may answer a request without an epoch
	// still unanswered. A withdrawal alone leaves the peer to answer it.
	readonly answering?: AbortSignal;
}

interface SubscribeEntry {
	// The write side: incoming GROUP streams are routed here. The application reads
	// the matching track.Subscriber it got from broadcast.Consumer.subscribe.
	track: track.Producer;
	compress: boolean;
	// Per-frame timestamp scale (0 = none), resolved before this entry is registered.
	timescale: number;
	// The group streams received, so the subscription can wait for the ones still owed
	// after the publisher ends it.
	tail: Tail;
	// The first group the SUBSCRIBE asked for, if any.
	requested?: number;
	// The first group the publisher serves (SUBSCRIBE_START) and the track's exclusive end
	// (SUBSCRIBE_END), once it declares them.
	start?: number;
	end?: number;
	// Set by the subscribe stream's FIN before lite-05, the track's only end there.
	finished?: boolean;
	// Settles once the subscribe stream's responses are read to its FIN or reset, which
	// Subscriber.drain waits on.
	responses?: Promise<void>;
	// Whether the stream ever asked for a bounded range, which a lite-07 publisher FINs without
	// SUBSCRIBE_END once it covers it.
	bounded: boolean;
	// The groups the SUBSCRIBE asked for, which that FIN covers unless the demand moved since
	// in an update the publisher may not have read.
	asked: { start: number; end?: number };
	// The TRACK stream that answered (lite-05+), held open as interest in the track until
	// the SUBSCRIBE has its first response, so the publisher's demand never lapses between
	// the two.
	held?: Stream;
	// Called at the SUBSCRIBE's first response (lite-05+). Until then a SUBSCRIBE without an
	// epoch may be answered by whichever instance won the path, so a replacement refuses it.
	answered?: () => void;
}

/** Subscribe to the peer and own its receive-side protocol tasks.
 * @internal
 */
export class Subscriber {
	#quic: WebTransport;
	// Advertisements the peer holds live across every announce stream on this session.
	#announces: Slots;

	// The version of the connection.
	readonly version: Version;

	// Shared with the Publisher so reflected announces can be dropped on receipt.
	readonly hop: Hop;

	// Registered only after immutable TRACK_INFO or the older drafts' defaults resolve,
	// so every incoming group and datagram already knows its timestamp framing.
	#subscribes = new Map<bigint, SubscribeEntry>();
	#subscribeNext = 0n;

	// Dedup consumed broadcasts per path: repeat consume() calls share one subscription.
	#consumes = new BroadcastCache();
	#consumeNext = 0;

	// Dedup in-flight one-shot fetches, keyed by [consume, broadcast, epoch, track, sequence].
	// Concurrent (or repeat, while still open) fetchGroup() calls for the same group of one consume
	// share one FETCH stream and each get an independent mirror; the entry is evicted once the
	// group closes.
	#fetches = new Map<string, { group: netGroup.Producer; accepted: Promise<void> }>();

	// The peer's PROBE estimates, written as they arrive (Lite03+ only).
	#probe?: Signal<ProbeStats>;

	// The peer's SETUP (lite-05+), undefined until it arrives. Gates opening the PROBE
	// stream on the peer having advertised Probe >= Report.
	#peerSetup?: Signal<Setup | undefined>;

	// Settles when the peer sends GOAWAY, repricing this session's routes to the drain cost.
	#goaway?: GetPromise<Drain>;

	// Distinguishes failures from streams torn down by Subscriber.close().
	#closed = new AbortController();

	// Our grant: a subscription it stops covering is cancelled. Undefined until the peer
	// answers, and forever on a version without AUTH, which allows everything.
	#grant?: Getter<Grant | undefined>;
	#auth?: AuthSession;

	/**
	 * Creates a new Subscriber instance.
	 * @param quic - The WebTransport session to use
	 * @param version - The protocol version
	 * @param origin - Hop id shared with the Publisher
	 * @param probe - Optional sink for the peer's PROBE estimates
	 * @param peerSetup - Optional peer SETUP slot for capability gating (lite-05+)
	 * @param goaway - Settles when the peer sends GOAWAY
	 * @param grant - The union of our tokens' grants, which bounds what we subscribe to
	 * @param limits - How many live requests the peer may hold
	 *
	 * @internal
	 */
	constructor(
		quic: WebTransport,
		version: Version,
		hop: Hop,
		probe?: Signal<ProbeStats>,
		peerSetup?: Signal<Setup | undefined>,
		goaway?: GetPromise<Drain>,
		grant?: Getter<Grant | undefined>,
		limits: Limits = LIMITS,
		auth?: AuthSession,
	) {
		this.#quic = quic;
		this.#announces = new Slots(limits.announces, { quic, reason: "too many announcements" });
		this.version = version;
		this.hop = hop;
		this.#probe = probe;
		this.#peerSetup = peerSetup;
		this.#goaway = goaway;
		this.#grant = grant;
		this.#auth = auth;
	}

	// Whether the peer has sent GOAWAY. Requests keep opening here until a replacement
	// session's route outranks this one.
	#goingAway(): boolean {
		return this.#goaway?.peek() !== undefined;
	}

	// What a route costs once the peer has asked us to leave. A later announce on a
	// draining session must not win selection, however cheap the path it advertises.
	#cost(cost?: Cost): Cost {
		return this.#goingAway() ? DRAIN_COST : (cost ?? Cost.zero);
	}

	// Whether our grant no longer lets us subscribe to `broadcast`.
	#denied(broadcast: Path.Valid): boolean {
		const grant = this.#grant?.peek();
		return grant !== undefined && !grant.subscribe.matches(broadcast);
	}

	/**
	 * Subscribe to broadcast announcements matching `scope`. Paths are relative
	 * to the session, not the scope.
	 *
	 * Reflected announces (those whose hop chain already includes this
	 * connection) are always dropped: moq-lite-06 has none to keep, and older
	 * versions stay consistent with that.
	 *
	 * Hidden routes (a `.`-prefixed segment below the scope's head) are left out unless
	 * `options.hidden` opts in. The opt-in rides the request on lite-07+; an older peer
	 * never hides anything, so the rule is also applied here.
	 *
	 * Only routes whose literal name matches `scope` are reported, unless `options.covering`
	 * also asks for those whose subtree overlaps it.
	 */
	announced(scope: Path.Pattern = Path.Pattern.all(), options?: announce.Options): announce.Consumer {
		const announced = new announce.Producer();
		// The wire speaks announce interest by prefix, and echoes suffixes beneath it.
		const consumer = announced.consume();
		// The last reader leaving ends the stream, which cancels the request.
		closeUnwanted(announced);
		void this.#runAnnounced(
			announced,
			scopeHead(scope),
			scope,
			options?.hidden ?? false,
			options?.covering ?? false,
		);
		return consumer;
	}

	async #runAnnounced(
		announced: announce.Producer,
		prefix: Path.Valid,
		scope: Path.Pattern,
		hidden: boolean,
		covering: boolean,
	): Promise<void> {
		console.debug(`announced: prefix=${prefix}`);
		// Lite04/05: send our own session-level Hop ID so the peer can skip announces
		// whose hop chain already passed through us. Encoding drops it on every other
		// version, where we drop the reflected announce on receipt instead. Matches the
		// Rust subscriber's `exclude_hop: self.self_origin.id` in `run_announce_prefix`.
		const msg = new AnnounceRequest(prefix, this.hop, hidden);
		const visible = (path: Path.Valid) =>
			(covering ? scopeOverlaps(scope, path) : scope.matches(path)) && (hidden || !hiddenBelow(prefix, path));

		// Opened outside the try so the catch can reach it: a protocol violation below has
		// to reset the stream, not just close our side of it.
		let stream: Stream;
		try {
			const deadline = new Deadline();
			stream = await deadline.wait(
				deadline.open(Stream.open(this.#quic, { version: this.version, ...requestOptions })),
			);
		} catch (err: unknown) {
			announced.close(error(err));
			return;
		}
		// The consumer left while the open waited for a slot: give the slot back unused.
		if (announced.closed.peek() !== undefined) {
			stream.close();
			return;
		}

		// Every advertisement the peer currently has live, keyed by suffix (at most one
		// per path is current, and every announce on this stream shares `prefix`).
		//
		// An advertisement skipped locally as a reflected loop is recorded with
		// `live: false`: the peer numbered it and will retract it regardless of what we
		// made of it, so its path is not free. Dropping it from the map instead would let
		// a later announce take the path, and the skipped one's `endedId` would then
		// retract that one's state.
		type Advertisement = {
			live: boolean;
			route: Route;
			captures: Path.Pattern[] | undefined;
		};
		// Each holds one of the session's announce slots, reflected and hidden ones included.
		const advertised = new SlotMap<Path.Valid, Advertisement>(this.#announces);

		let stopDrain: Dispose | undefined;
		try {
			// Send the announce interest.
			await stream.writer.u53(StreamId.Announce);
			await msg.encode(stream.writer, this.version);

			// Lite05+: the publisher reports its own Hop ID before any announces.
			// It no longer stamps itself onto each hop chain, so we append it here to
			// keep the reflected-announce loop check seeing the full chain.
			let responderOrigin: Hop | undefined;
			// The ANNOUNCEs still owed of the peer's initial set (lite-05+). Older versions
			// mark no boundary, so they sync once the request is out, as a best effort.
			let initial = 0;
			let waitingSynced = hasSynced(this.version);
			if (hasAnnounceOk(this.version)) {
				const ok = await race([AnnounceOk.decode(stream.reader, this.version), announced.closed]);
				// A consumer that leaves before the peer answers ends the request now, not on the answer.
				if (!(ok instanceof AnnounceOk)) {
					stream.close();
					return;
				}
				// Keep a withheld 0: it names nobody for loop detection, but it is the
				// anonymous mark and must travel the reconstructed chain. Assigned identities
				// stay off this hop and are never forwarded.
				responderOrigin = ok.hop;
				initial = ok.active;
			}

			switch (this.version) {
				case Version.DRAFT_01:
				case Version.DRAFT_02: {
					// Receive ANNOUNCE_INIT first
					const init = await race([AnnounceInit.decode(stream.reader, this.version), announced.closed]);
					if (!(init instanceof AnnounceInit)) {
						stream.close();
						return;
					}

					// Process initial announcements. These are advertisements like any other, so
					// they go on record and obey the same one-per-path rule: the initial set
					// naming a path twice is the same violation as two ANNOUNCE_STARTs for it,
					// and the record is what catches either. Draft01/02 carry no hop ids and no
					// ANNOUNCE_OK, so nothing names the publisher.
					for (const suffix of init.suffixes) {
						const path = Path.join(prefix, suffix);
						const error = Path.check(path);
						if (error) throw new ProtocolViolation(error);
						if (advertised.has(path)) {
							throw new ProtocolViolation(`duplicate announce for ${path}`);
						}
						const route = { hops: [UNKNOWN_HOP], cost: this.#cost() };
						const live = visible(path);
						const captures = scopeCaptures(scope, path);
						advertised.set(path, { live, route, captures });
						if (!live) continue;
						this.#consumes.announce(path, announced, route);
						console.debug(`announced: broadcast=${path} active=true`);
						announced.append({ prefix: path, captures, kind: "start", route });
					}
					break;
				}
				default:
					// Draft03+: no AnnounceInit, initial state comes via Announce messages.
					break;
			}

			// A draining peer usually stops announcing, so reprice from the GOAWAY itself.
			// Waiting for another message would leave the route primary until the session
			// closed. Idempotent: an unchanged cost emits nothing. A GOAWAY that already
			// arrived needs no listener, since `#cost()` priced every route above.
			const drainAdvertised = () => {
				if (announced.closed.peek() !== undefined) return;
				for (const [path, ad] of advertised) {
					if (!ad.live) continue;
					const route = { ...ad.route, cost: DRAIN_COST };
					if (routesEqual(ad.route, route)) continue;
					advertised.set(path, { ...ad, route });
					this.#consumes.announce(path, announced, route);
					announced.append({ prefix: path, captures: ad.captures, kind: "update", route });
				}
			};
			stopDrain = this.#goaway?.changed(() => drainAdvertised());

			// Lite06+: announce ids. Each received `active` implicitly assigns the next
			// per-stream ordinal, as does each `takeover`; `endedId`/`update`/`restart` reference
			// it, and lite-07 bases copy from it. Tracked even for announces we skip as reflected, since the sender
			// doesn't know we skipped.
			const history = new AnnounceHistory();

			// Receive announce updates (for Draft03+, this includes initial state)
			for (;;) {
				if (initial === 0 && !waitingSynced) announced.synced();

				const announce = await race([
					decodeAnnounceBroadcastMaybe(stream.reader, this.version),
					announced.closed,
				]);
				// undefined: the stream ended. null: the consumer closed cleanly.
				if (announce === undefined && (initial > 0 || waitingSynced)) {
					throw new ProtocolViolation("announce stream ended inside the initial set");
				}
				if (!announce) break;
				if (announce instanceof Error) throw announce;
				if (announce.status === "synced") {
					if (!waitingSynced) throw new ProtocolViolation("duplicate SYNCED");
					waitingSynced = false;
					continue;
				}

				// The counted initial set is ANNOUNCEs only, so anything else cuts it short. An
				// unknown message counts for nothing either way.
				if (initial > 0 && announce.status !== "skipped") {
					if (announce.status !== "active") {
						throw new ProtocolViolation(`${announce.status} inside the initial set`);
					}
					initial -= 1;
				}

				let path: Path.Valid;
				let active: boolean;
				// Present on active/update/restart; ended messages never carry hops worth checking.
				let hops: Hop[] | undefined;
				let cost: Cost | undefined;
				// Present on active and restart; an update never changes it.
				let epoch: Epoch.Valid | undefined;
				// Another publisher instance replaces the advertisement (lite-07).
				let restart = false;

				switch (announce.status) {
					case "active": {
						const resolved = hasAnnounceId(this.version) ? history.start(announce) : announce;
						// The wire names the suffix beneath the interest prefix; the consumer
						// sees the covered path from the session root.
						path = Path.join(prefix, resolved.suffix);
						active = true;
						hops = resolved.hops;
						cost = announce.cost;
						epoch = announce.epoch;
						break;
					}
					case "ended":
						path = Path.join(prefix, announce.suffix);
						active = false;
						break;
					case "endedId":
						// Resolve and retire the id; an unknown or retired id is a protocol violation.
						path = Path.join(prefix, history.end(announce.id));
						active = false;
						break;
					case "update": {
						// Resolve the id; it stays live (the replacement reuses it).
						const resolved = history.update(announce);
						path = Path.join(prefix, resolved.suffix);
						active = true;
						hops = resolved.hops;
						cost = announce.cost;
						break;
					}
					case "restart": {
						// Resolved like an update: the id stays live.
						const resolved = history.update(announce);
						path = Path.join(prefix, resolved.suffix);
						active = true;
						hops = resolved.hops;
						cost = announce.cost;
						epoch = announce.epoch;
						restart = true;
						break;
					}
					case "takeover": {
						// Continuity across the takeover is not kept here: a takeover at a new path
						// surfaces as a start, and one at its predecessor's own path as a restart,
						// as an older wire carries it.
						const resolved = history.takeover(announce);
						if (!Path.hasPrefix(resolved.previous, resolved.suffix)) {
							throw new ProtocolViolation(`takeover of ${resolved.previous} at ${resolved.suffix}`);
						}
						path = Path.join(prefix, resolved.suffix);
						active = true;
						hops = resolved.hops;
						cost = announce.cost;
						epoch = announce.epoch;
						restart = resolved.suffix === resolved.previous;
						break;
					}
					case "skipped":
						continue;
				}

				// Each suffix fits the wire limits alone, but the interest prefix can push the
				// joined path past them, which the draft makes a violation too.
				const error = Path.check(path);
				if (error) throw new ProtocolViolation(error);

				// One current advertisement per path per stream, decided before anything below
				// can skip this announcement. A second ANNOUNCE_START for a path the peer
				// already advertised is a violation whether or not its route would be usable
				// here, and whether or not we kept the first; letting a skip pre-empt it would
				// retract the live route and leave the stream open on a peer already out of
				// spec.
				//
				// lite-05 alone is exempt, where a duplicate ANNOUNCE *is* the replacement
				// idiom. lite-06 gave that its own message and older versions never had one, so
				// a duplicate means the same thing on both sides of it. Mirrors the branch the
				// Rust announce loop takes before `start_announce`.
				const duplicateIsUpdate = updateSupported(this.version) && !hasAnnounceId(this.version);
				const starts = announce.status === "active" || (announce.status === "takeover" && !restart);
				if (starts && !duplicateIsUpdate && advertised.has(path)) {
					throw new ProtocolViolation(`duplicate announce for ${path}`);
				}

				// Retract this interest's advertisement so new consumes resolve the remaining
				// serving instance, and tell the consumer. A no-op for an advertisement never surfaced, which is
				// what an id retiring a skipped announce resolves to.
				const retract = () => {
					const previous = advertised.get(path);
					advertised.delete(path);
					this.#consumes.withdraw(path, announced);
					if (!previous?.live) return;

					console.debug(`announced: broadcast=${path} active=false`);
					announced.append({
						prefix: path,
						captures: previous.captures,
						kind: "end",
						route: previous.route,
					});
				};

				// An update keeps the epoch its announcement named, even through a placeholder
				// below: a later update that is not reflected still names that instance.
				if (!restart) epoch ??= advertised.get(path)?.route.epoch;

				// In Lite05+ the sender's origin arrives via AnnounceOk, not in each hop
				// list, so fold it back in before checking.
				if (hops !== undefined) {
					const full = responderOrigin !== undefined ? [...hops, responderOrigin] : hops;
					if (full.includes(this.hop)) {
						// A reflected update means the peer's remaining route loops back through
						// us, so the route is gone even though the message says active. The
						// advertisement stays live: the peer still holds the path and its id still
						// resolves here.
						retract();
						advertised.set(path, {
							live: false,
							route: { epoch, hops: full, cost: Cost.zero },
							captures: undefined,
						});
						continue;
					}
				}

				if (!active) {
					retract();
					continue;
				}

				const fullHops =
					hops !== undefined && responderOrigin !== undefined
						? [...hops, responderOrigin]
						: [...(hops ?? [])];
				// A received empty list is the anonymous mark, not a local announcement.
				if (fullHops.length === 0) fullHops.push(UNKNOWN_HOP);
				// Appending a withheld AnnounceOk(0) onto a 32-entry list is the same
				// drop Rust's Hops::push makes: do not expose an overlong chain.
				if (fullHops.length > MAX_HOPS) {
					console.debug(`announced: broadcast=${path} dropped (hop chain at MAX_HOPS)`);
					advertised.set(path, {
						live: false,
						route: { epoch, hops: [], cost: Cost.zero },
						captures: undefined,
					});
					continue;
				}
				const route: Route = { epoch, hops: fullHops, cost: this.#cost(cost) };
				const captures = scopeCaptures(scope, path);
				if (!visible(path)) {
					advertised.set(path, { live: false, route, captures });
					continue;
				}

				// Another publisher instance: whatever was consumed at the path is the old one, so
				// the next consume subscribes fresh, while the handles already out keep theirs.
				const previous = advertised.get(path);
				if (restart && previous?.live) {
					advertised.set(path, { live: true, route, captures });
					this.#consumes.announce(path, announced, route, true);
					console.debug(`announced: broadcast=${path} restart=true epoch=${epoch}`);
					announced.append({ prefix: path, captures, kind: "restart", route });
					continue;
				}

				// A second advertisement for a path we already carry is an update: either an
				// explicit ANNOUNCE_UPDATE, or (lite-05) a duplicate ANNOUNCE. It updates the
				// route in place, so a forwarder re-prices without retracting.
				if (previous?.live) {
					// An update preserves the advertisement identity, including on epochless drafts.
					advertised.set(path, { live: true, route, captures });
					this.#consumes.announce(path, announced, route);
					console.debug(`announced: broadcast=${path} rerouted`);
					if (!routesEqual(previous.route, route)) {
						announced.append({ prefix: path, captures, kind: "update", route });
					}
					continue;
				}

				advertised.set(path, { live: true, route, captures });
				this.#consumes.announce(path, announced, route);

				console.debug(`announced: broadcast=${path} active=true epoch=${epoch}`);
				announced.append({ prefix: path, captures, kind: "start", route });
			}

			announced.close();
			// Either side ending the interest ends the stream, or the peer keeps it open, still
			// discovering into it and holding a stream slot.
			stream.close();
		} catch (err: unknown) {
			const e = error(err);
			// Reaches here on a protocol violation the peer committed (a second
			// advertisement for a live path, an unknown announce id) as well as on a
			// transport failure. Either way the peer has to be told: closing only our side
			// would leave it announcing into a stream nobody reads.
			stream.abort(e);
			announced.close(e);
			// A violation ends the session, not just this stream, so a nonconforming peer
			// cannot repeat it on the next one. Matches `ietf::Subscriber` and the Rust
			// lite subscriber, where the announce half only ever ends the session on error.
			closeViolation(this.#quic, e);
		} finally {
			// Release only this interest's routes. Existing consumers remain pinned.
			for (const path of advertised.keys()) this.#consumes.withdraw(path, announced);
			advertised.clear();
			stopDrain?.();
		}
	}

	/**
	 * Consumes a broadcast from the connection.
	 *
	 * Deduplicated per serving instance: repeat calls for the same path share one reference-counted
	 * broadcast (and one upstream subscription). The shared broadcast closes once every caller has
	 * closed its handle, so callers close normally.
	 *
	 * @param name - The name of the broadcast to consume
	 * @returns A Broadcast instance
	 */
	consume(path: Path.Valid): broadcast.Consumer {
		const instance = this.#consumes.instance(path);
		return this.#consumes.get(path, instance) ?? this.#createConsume(path, instance);
	}

	#createConsume(path: Path.Valid, instance: ReturnType<BroadcastCache["instance"]>): broadcast.Consumer {
		const consumed = {
			epoch: instance.route.epoch,
			id: this.#consumeNext++,
			get valid() {
				return instance.validity?.signal;
			},
			get answering() {
				return instance.validity?.answering;
			},
		};
		const consumer = new ConsumeBroadcast(this, path, consumed);
		// Registered before anyone can list the broadcast, so no reader syncs without the peer.
		void this.#runListing(path, consumed, wireOf(consumer).lister());

		void (async () => {
			for (;;) {
				const request = await wireOf(consumer).requested();
				if (!request) break;
				void this.#runSubscribe(path, consumed, request);
			}
		})();

		return this.#consumes.insert(path, instance, consumer);
	}

	// Answer listings of a consumed broadcast from the peer's TRACKS while anyone lists it, until
	// the broadcast closes. Every listing of the broadcast shares one TRACKS stream.
	async #runListing(broadcast: Path.Valid, consumed: Consumed, lister: Lister) {
		try {
			for (;;) {
				if (lister.closed.peek() !== undefined || this.#closed.signal.aborted) return;
				if (!lister.wanted.peek()) {
					await untilAborted(race([Signal.race(lister.wanted, lister.closed)]), this.#closed.signal).catch(
						() => {},
					);
					continue;
				}
				if (!hasTracks(this.version)) {
					// An empty answer would claim the peer lists nothing.
					lister.abort(new Error("track listing requires moq-lite-07 or newer"));
					continue;
				}
				await this.#listTracks(broadcast, consumed, lister);
			}
		} finally {
			lister.close();
		}
	}

	// Forward the peer's TRACKS listing of `broadcast` into `lister` until the peer ends it, it
	// fails, our grant stops covering the broadcast, or nobody lists the broadcast any more.
	// Returns without answering when setup was cut short, for the caller to look again.
	async #listTracks(broadcast: Path.Valid, consumed: Consumed, lister: Lister): Promise<void> {
		// The consumed instance must still be the selected one until the request is out, then
		// not replaced until the peer answers: a TRACKS request without an epoch lists whichever
		// instance won the path when it arrived.
		const valid = consumed.valid;
		if (valid?.aborted) {
			lister.abort(error(valid.reason));
			return;
		}
		const setup = valid ? AbortSignal.any([this.#closed.signal, valid]) : this.#closed.signal;
		// A listing is a subscribe: one opened before a token that covers it is answered would be
		// refused by the peer.
		const ready = await this.#auth
			?.ready(broadcast, "subscribe", {
				signal: setup,
				until: async () => {
					while (lister.wanted.peek() && lister.closed.peek() === undefined) {
						await Signal.race(lister.wanted, lister.closed);
					}
				},
			})
			.catch(() => false);
		// The readers may have left while the wait settled, before the unused watch below is armed.
		if (ready === false || setup.aborted || !lister.wanted.peek()) return;

		// Armed before the first check and held to the end, so a shrink is never missed.
		const refused = unauthorized(broadcast);
		const revoked = new AbortController();
		const disposeGrant = this.#grant?.subscribe(() => {
			if (this.#denied(broadcast)) revoked.abort(refused);
		});
		if (this.#denied(broadcast)) {
			disposeGrant?.();
			lister.abort(refused);
			return;
		}
		// Synced with this hop, which holds nothing yet: a relay never waits on its upstream,
		// so the peer's tracks follow as live adds.
		lister.synced();

		// Each listed track holds an announces slot: discovery state the peer makes us hold.
		const listed = new Set<string>();
		const unused = new AbortController();
		const stop = AbortSignal.any([unused.signal, revoked.signal, this.#closed.signal]);
		const disposeWanted = lister.wanted.subscribe((wanted) => {
			if (!wanted) unused.abort(new StreamError(StreamCode.Cancel, { message: "listing unused" }));
		});
		let stream: Stream | undefined;
		try {
			const opening = Stream.open(this.#quic, { version: this.version, ...requestOptions });
			const cut = valid ? AbortSignal.any([stop, valid]) : stop;
			void opening.then(
				(late) => {
					if (cut.aborted) late.abort(error(cut.reason));
				},
				() => {},
			);
			stream = await untilAborted(opening, cut);
			await stream.writer.u53(StreamId.Tracks);
			await new TracksMessage(broadcast, consumed.epoch).encode(stream.writer, this.version);
			let synced = false;
			const answering = consumed.answering;
			let unanswered = answering ? AbortSignal.any([stop, answering]) : stop;
			for (;;) {
				const pause = listingBudget.poll();
				if (pause !== undefined) await untilAborted(pause, stop);
				const event = await untilAborted(decodeTracksEventMaybe(stream.reader, this.version), unanswered);
				unanswered = stop;
				if (!event) {
					// The peer's broadcast ended, whether or not its retraction has arrived: end the
					// listing and let go of what it listed. FIN our half too, so the stream is done.
					stream.writer.close();
					lister.end();
					return;
				}
				if (event.kind === "added") {
					if (listed.has(event.name)) throw new ProtocolViolation(`track listed twice: ${event.name}`);
					if (!this.#announces.tryAcquire()) {
						throw new SessionError(SessionCode.TooManyRequests, { reason: "too many listed tracks" });
					}
					listed.add(event.name);
					lister.add(event.name, this.#toModelInfo(event.info));
				} else if (event.kind === "removed" || event.kind === "finished") {
					if (!listed.delete(event.name))
						throw new ProtocolViolation(`unlisted track removed: ${event.name}`);
					this.#announces.release();
					if (event.kind === "finished") lister.finish(event.name);
					else lister.remove(event.name);
				} else {
					// This hop synced its readers when the listing began; only the order matters here.
					if (synced) throw new ProtocolViolation("duplicate SYNCED");
					synced = true;
				}
			}
		} catch (err) {
			if (revoked.signal.aborted) {
				// Reset rather than cancel, so the peer reads a revocation instead of a routine end.
				console.debug(`tracks revoked: broadcast=${broadcast}`);
				stream?.abort(refused);
				lister.abort(refused);
				return;
			}
			if (unused.signal.aborted) {
				// Nobody lists the broadcast any more: cancel the peer's listing.
				stream?.close();
				lister.clear();
				return;
			}
			closeViolation(this.#quic, err);
			const e = await sessionCause(this.#quic, err);
			console.debug(`tracks failed: broadcast=${broadcast} error=${reason(e)}`);
			stream?.abort(e);
			lister.abort(e);
		} finally {
			disposeGrant?.();
			disposeWanted();
			this.#announces.release(listed.size);
		}
	}

	async #runSubscribe(broadcast: Path.Valid, consumed: Consumed, request: track.Request) {
		const id = this.#subscribeNext++;
		const subscription = request.subscription;
		const initialBounds = groupBounds(subscription.groups);
		if (emptyRange({ startGroup: initialBounds.start, endGroup: initialBounds.end })) {
			request.reject(new Error(EMPTY_RANGE));
			return;
		}
		const refused = unauthorized(broadcast);
		try {
			for (;;) {
				const ready = await this.#auth?.ready(broadcast, "subscribe", {
					signal: consumed.valid,
					until: () => abandoned(request.demand()),
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
		// Armed before the first check and held to the end, so a shrink while the
		// subscription sets up is never missed: it resets whatever reached the wire.
		const state: SubscribeSetup = { cancel: new AbortController() };
		const disposeGrant = this.#grant?.subscribe(() => {
			if (state.revoked || !this.#denied(broadcast)) return;
			state.revoked = refused;
			console.debug(`subscribe revoked: id=${id} broadcast=${broadcast} track=${request.name}`);
			state.cancel.abort(refused);
			state.producer?.abort(refused);
			state.stream?.abort(refused);
		});
		try {
			if (this.#denied(broadcast)) {
				request.reject(refused);
				return;
			}
			await this.#serveSubscribe(id, broadcast, consumed, request, state);
		} finally {
			disposeGrant?.();
		}
	}

	async #serveSubscribe(
		id: bigint,
		broadcast: Path.Valid,
		consumed: Consumed,
		request: track.Request,
		state: SubscribeSetup,
	) {
		const { epoch, valid, answering } = consumed;
		const subscription = request.subscription;

		console.debug(`subscribe start: id=${id} broadcast=${broadcast} track=${request.name}`);
		const bounds = groupBounds(subscription.groups);

		const msg = new Subscribe({
			id,
			broadcast,
			epoch,
			track: request.name,
			priority: subscription.priority ?? 0,
			order: subscription.order,
			maxDelay: subscription.maxDelay,
			startGroup: subscription.groups?.start === undefined ? undefined : bounds.start,
			endGroup: inclusiveGroupEnd(bounds.end),
		});

		// Open the stream under a timeout. The stream handles flow back via `state`
		// so the timeout path can close them if they finish opening after the deadline,
		// and `cancel` ends a setup still running once the deadline passed, resetting
		// its TRACK stream so a peer that never answers can't hold one per attempt.
		const retirer = (signal?: AbortSignal) => () => {
			state.cancel.abort(signal?.reason);
			state.producer?.abort(error(signal?.reason));
			state.stream?.abort(error(signal?.reason));
		};
		const retire = retirer(valid);
		valid?.addEventListener("abort", retire);
		if (valid?.aborted) retire();
		const deadline = new Deadline();
		const setup = this.#openSubscribe(state, msg, request, id, deadline);

		let opened: { stream: Stream; entry: SubscribeEntry };
		try {
			opened = await deadline.wait(untilAborted(setup, valid));
			console.debug(`subscribe ok: id=${id} broadcast=${broadcast} track=${request.name}`);
		} catch (err) {
			closeViolation(this.#quic, err);
			// The setup outlived its deadline waiting for the first response: a control
			// timeout, not content that arrived late. A revocation says so instead.
			const e = state.revoked ?? (await sessionCause(this.#quic, err));
			state.cancel.abort(e);
			valid?.removeEventListener("abort", retire);
			request.reject(e);
			this.#subscribes.delete(id);
			console.warn(`subscribe error: id=${id} broadcast=${broadcast} track=${request.name} error=${reason(e)}`);
			// Close the streams open now, since a write blocked on flow control may never let
			// setup settle, and any that open after the timeout once it does. Cover both
			// branches: setup may resolve late, or it may reject (e.g. encode/decode failure)
			// after a stream is open.
			const leave = () => {
				state.stream?.abort(e);
				state.track?.close();
			};
			leave();
			setup.then(leave, leave);
			return;
		}

		const { stream, entry } = opened;
		valid?.removeEventListener("abort", retire);
		// Sent but unanswered (lite-05+; older drafts already waited for SUBSCRIBE_OK), only a
		// replacement refuses it, since that instance may be the one answering.
		const replaced = retirer(answering);
		if (hasTrackStream(this.version) && answering) {
			answering.addEventListener("abort", replaced);
			entry.answered = () => answering.removeEventListener("abort", replaced);
			if (answering.aborted) replaced();
		}
		const producer = entry.track;
		try {
			// Watch for subscription changes and send SUBSCRIBE_UPDATE. Lite01/Lite02
			// don't carry SUBSCRIBE_UPDATE on the wire, so skip the watcher there
			// and just wait on the stream/track like before.
			//
			// The publisher sends responses on this stream until its FIN. Group streams can
			// still be in flight, so the track ends only once the tail is accounted for. A reset
			// rejects instead, so the track ends with that error rather than a clean tail.
			const responses = this.runResponses(stream, entry);
			entry.responses = responses.catch(() => {});
			let tailSettled = false;
			// Set when the publisher ended the subscription short of the demand.
			let short: Error | undefined;
			const closed = responses.then(async () => {
				if (!(await this.#settleTail(entry))) short = new Error("subscription ended before covering its range");
				tailSettled = true;
			});
			// A reset that lands after the race below settled is moot; the race observes one before.
			void closed.catch((err: unknown) => {
				closeViolation(this.#quic, err);
			});
			const subscriptionUpdates =
				this.version === Version.DRAFT_01 || this.version === Version.DRAFT_02
					? undefined
					: this.#runSubscriptionUpdates(id, broadcast, entry, msg, stream);

			// Terminal conditions (stream end, track close, a failed subscription update) settle at most
			// once; race them into one stable promise so the demand loop doesn't re-subscribe each pass.
			// Updates stop quietly at the FIN or a failed write, either of which can land before the
			// responses ahead of it are decoded, so only a refused update is terminal on its own.
			const terminal: PromiseLike<unknown>[] = [closed, hooks.trackReleased(producer)];
			if (subscriptionUpdates !== undefined) terminal.push(subscriptionUpdates.then(() => closed));
			const done = race(terminal);

			// Serve until a terminal condition fires or the last local subscriber leaves. The unused
			// wake is level-triggered: re-check demand so a subscriber that returns before we tear
			// down (e.g. a quickly unmuted tile) resumes on the same subscription.
			const idle = Symbol("idle");
			const demand = producer.demand();
			for (;;) {
				const reason = await race([done, unwanted(demand).then(() => idle)]);
				if (
					reason === idle &&
					hooks.demandReleased(demand).peek() === undefined &&
					demand.wanted.peek() !== undefined
				)
					continue;
				break;
			}

			// A publisher that covered the range before reading an update widening it sent
			// fewer groups than the track has, so it fails rather than ending clean. Otherwise
			// only a received SUBSCRIBE_END, or before lite-05 a FIN, declares the track's end:
			// a range covered or a subscription nobody wants anymore is a released copy, not
			// finished content.
			if (short) producer.abort(short);
			else {
				if (tailSettled && entry.finished) producer.finish();
				hooks.releaseTrack(producer);
			}
			// A settled lite07 tail acknowledges completion with FIN, without cancelling
			// the publisher's already-finished receive half.
			if (tailSettled && waitsForSubscriberFin(this.version)) stream.writer.close();
			else stream.close();
			console.debug(`subscribe close: id=${id} broadcast=${broadcast} track=${request.name}`);
		} catch (err) {
			const e = await sessionCause(this.#quic, err);
			producer.abort(e);
			console.warn(`subscribe error: id=${id} broadcast=${broadcast} track=${request.name} error=${reason(e)}`);
			stream.abort(e);
		} finally {
			answering?.removeEventListener("abort", replaced);
			entry.held?.close();
			this.#subscribes.delete(id);
		}
	}

	// Determine the track's immutable properties, accept the request (so the
	// application's track.Subscriber resolves and incoming groups have a producer to
	// write into), register it, then open the subscribe stream. `state.stream` and
	// `state.track` are populated as soon as each stream opens so the caller can clean
	// them up on timeout even before this promise settles.
	//
	// On lite-05+ the properties come from a TRACK stream opened first, held open until
	// the SUBSCRIBE is answered, and the SUBSCRIBE is accepted implicitly (no
	// SUBSCRIBE_OK). Older drafts carry no per-track properties, so they resolve to
	// defaults and just drain SUBSCRIBE_OK.
	async #openSubscribe(
		state: SubscribeSetup,
		msg: Subscribe,
		request: track.Request,
		id: bigint,
		deadline: Deadline,
	): Promise<{ stream: Stream; entry: SubscribeEntry }> {
		state.cancel.signal.throwIfAborted();
		let producer: track.Producer;
		let drainOk = false;
		let compress = false;
		let timescale = 0;

		if (hasTrackStream(this.version)) {
			// Fetch the immutable properties once via the TRACK stream.
			const { info, stream } = await this.#trackInfo(
				msg.broadcast,
				msg.epoch,
				msg.track,
				state.cancel.signal,
				deadline,
			);
			state.track = stream;
			// The deadline passed as TRACK_INFO landed: the request is already rejected, so don't
			// register it again or send its SUBSCRIBE.
			state.cancel.signal.throwIfAborted();
			producer = request.accept(this.#toModelInfo(info));
			compress = info.compress;
			timescale = info.timescale ?? 0;
		} else {
			// Older drafts negotiate nothing per-track: verbatim frames with no timeline.
			producer = request.accept();
			drainOk = true;
		}
		// Lite-06+ resolves where the feed starts only with SUBSCRIBE_START, so a relay serving
		// this copy holds its first group until the answer arrives.
		if (resolvesStart(this.version)) hooks.startTrack(producer, { requested: msg.startGroup });

		// Register before opening SUBSCRIBE so a racing GROUP stream finds the entry.
		const entry: SubscribeEntry = {
			track: producer,
			compress,
			timescale,
			// The effective max delay is the stopgap grace: the wrong clock (it bounds
			// presentation-time drift), but it is how long the subscriber was willing to wait
			// for a late group anyway. Already the smaller of the subscriber's and the track's.
			tail: new Tail({
				grace: () => grace(producer.subscription.peek()?.maxDelay ?? Milli.zero),
			}),
			requested: msg.startGroup,
			bounded: msg.endGroup !== undefined,
			asked: groupBounds(request.subscription.groups),
			held: state.track,
		};
		this.#subscribes.set(id, entry);
		state.producer = producer;

		state.stream = await deadline.open(Stream.open(this.#quic, { version: this.version, ...requestOptions }));
		// The deadline passed while the open waited: the late-setup handler resets the stream, so
		// don't send a SUBSCRIBE on it first.
		state.cancel.signal.throwIfAborted();
		await state.stream.writer.u53(StreamId.Subscribe);
		await msg.encode(state.stream.writer, this.version);

		if (drainOk) {
			// The first response MUST be a SUBSCRIBE_OK (older drafts only).
			const resp = await decodeSubscribeResponse(state.stream.reader, this.version);
			if (!("ok" in resp)) {
				throw new Error("first subscribe response must be SUBSCRIBE_OK");
			}
		}

		return { stream: state.stream, entry };
	}

	// Opens a TRACK stream and reads the single TRACK_INFO. Lite-05+ only. The stream
	// stays open, as interest in the track, until the caller closes it.
	async #trackInfo(
		broadcast: Path.Valid,
		epoch: Epoch.Valid | undefined,
		track: string,
		signal: AbortSignal | undefined,
		deadline: Deadline,
	): Promise<{ info: TrackInfo; stream: Stream }> {
		return this.#exchange(
			{ version: this.version },
			async (stream) => {
				await stream.writer.u53(StreamId.Track);
				await new TrackMessage(broadcast, track, epoch).encode(stream.writer, this.version);
				const info = await TrackInfo.decode(stream.reader, this.version);
				return { info, stream };
			},
			signal,
			deadline,
		);
	}

	// Opens a stream and runs a request/response exchange on it, resetting the stream if `run`
	// fails. Subscriber.close() or `signal` also resets it while `run` is pending, so a peer that
	// never answers cannot hold it open, and a stream that opens after either is reset at once.
	async #exchange<T>(
		options: OpenOptions,
		run: (stream: Stream) => Promise<T>,
		signal: AbortSignal | undefined,
		deadline: Deadline,
	): Promise<T> {
		const closed = signal ? AbortSignal.any([this.#closed.signal, signal]) : this.#closed.signal;
		closed.throwIfAborted();
		const opening = deadline.open(Stream.open(this.#quic, { ...options, ...requestOptions }));
		let stream: Stream;
		try {
			stream = await untilAborted(opening, closed);
		} catch (err) {
			void opening.then(
				(late) => late.abort(error(err)),
				() => {},
			);
			throw err;
		}
		const abort = () => stream.abort(error(closed.reason));
		closed.addEventListener("abort", abort);
		try {
			closed.throwIfAborted();
			return await untilAborted(run(stream), closed);
		} catch (err) {
			closeViolation(this.#quic, err);
			stream.abort(error(err));
			throw err;
		} finally {
			closed.removeEventListener("abort", abort);
		}
	}

	// Map the wire TRACK_INFO onto the model track.Info a producer/consumer holds.
	#toModelInfo(info: TrackInfo): track.Info {
		return {
			// Lite-05/06 timestamps may be send times, so the copy is untimed and the ingest drops them.
			timescale: hasMediaTime(this.version) ? info.timescale : undefined,
			// Publisher Max Age rides on the wire, so the local media-time budget
			// matches what the upstream advertises (relays re-serve with the same bound).
			maxAge: info.maxAge === undefined ? undefined : Milli(info.maxAge),
			priority: info.priority,
			refresh: info.refresh,
			compress: info.compress,
		};
	}

	// Resolve a track's immutable model info via a TRACK stream (lite-05+), for the
	// ConsumeBroadcast backing track.Consumer.info(). On older drafts there's no TRACK
	// stream, so this rejects rather than fabricating defaults. The TRACK stream stays
	// open until `hold` aborts, so the publisher keeps the track for whoever asked here;
	// aborting it before TRACK_INFO resets the stream.
	async resolveTrackInfo(
		broadcast: Path.Valid,
		track: string,
		epoch?: Epoch.Valid,
		hold?: AbortSignal,
	): Promise<track.Info> {
		if (!hasTrackStream(this.version)) {
			throw new Error("track info requires moq-lite-05 or newer");
		}
		const deadline = new Deadline();
		const { info, stream } = await deadline.wait(this.#trackInfo(broadcast, epoch, track, hold, deadline));
		if (hold && !hold.aborted) hold.addEventListener("abort", () => stream.close(), { once: true });
		else stream.close();
		return this.#toModelInfo(info);
	}

	// Open a FETCH stream for one group and stream its bare frames into a group, for the
	// ConsumeBroadcast backing track.Consumer.fetchGroup() (lite-05+).
	async fetchGroup(
		broadcast: Path.Valid,
		track: string,
		sequence: number,
		options: track.FetchGroupOptions = {},
		consumed: Consumed = {},
	): Promise<netGroup.Consumer> {
		const { epoch } = consumed;
		options.signal?.throwIfAborted();

		// Coalesce onto a still-open fetch of the same group so we don't open a second FETCH
		// stream (and re-download it); each caller reads an independent mirror.
		//
		// Reserve each caller's mirror before the fetch starts or is awaited: the fetch watches
		// demand from the start, and a fast FIN cannot discard frames before these callers
		// receive their handles. An abort closes only this caller's mirror, so the stream is
		// cancelled once the last one leaves.
		// Different freshness budgets use independent streams: once a FETCH is on the
		// wire its limit cannot be widened for a later, more tolerant caller. Equal
		// budgets still coalesce and each caller keeps an independent mirror.
		const key = JSON.stringify([consumed.id, broadcast, epoch, track, sequence, options.maxDelay]);
		let entry = this.#fetches.get(key);
		let consumer: netGroup.Consumer;
		if (entry && !entry.group.isClosed) {
			consumer = entry.group.mirror();
		} else {
			const group = new netGroup.Producer(sequence);
			consumer = group.mirror();
			entry = {
				group,
				accepted: this.#runFetch(
					broadcast,
					consumed,
					track,
					sequence,
					options.priority ?? 0,
					group,
					options.maxDelay,
				),
			};
			this.#fetches.set(key, entry);
			void group.closed.then(() => {
				if (this.#fetches.get(key)?.group === group) this.#fetches.delete(key);
			});
		}

		try {
			await untilAborted(entry.accepted, options.signal);
			return consumer;
		} catch (err) {
			consumer.close();
			throw err;
		}
	}

	// Open the FETCH stream and pump the response into the shared group. Setup errors close the
	// group, evict the entry, and reject every caller waiting for acceptance. A setup every caller
	// has abandoned is cancelled the same way.
	async #runFetch(
		broadcast: Path.Valid,
		consumed: Consumed,
		track: string,
		sequence: number,
		priority: number,
		group: netGroup.Producer,
		maxDelay?: Milli,
	): Promise<void> {
		const refused = unauthorized(broadcast);
		try {
			// Every caller leaving ends the wait like it ends the setup below. A caller that
			// coalesced back onto the group before we commit keeps it waiting.
			while (
				(await this.#auth?.ready(broadcast, "subscribe", {
					signal: consumed.valid,
					until: () => groupUnwanted(group),
				})) === false
			) {
				if (group.closed.peek() === undefined && group.demand().wanted.peek()) continue;
				throw new StreamError(StreamCode.Cancel, { message: "cancel" });
			}
			// Arm after admission, before checking: another unanswered token may still
			// widen the union while we wait, which is not a revocation of this fetch.
			const disposeGrant = this.#grant?.subscribe(() => {
				if (this.#denied(broadcast)) group.abort(refused);
			});
			void group.closed.then(() => disposeGrant?.());
			if (this.#denied(broadcast)) throw refused;

			// Lite has no FETCH_OK, so a publisher that never answers would hold the setup forever.
			// Subscriber.close() closing the group releases every caller at any stage, and resets
			// the streams the setup opened.
			const deadline = new Deadline();
			const setup = deadline.wait(
				this.#fetchSetup(broadcast, consumed, track, sequence, priority, group, deadline, maxDelay),
			);
			let accepted: { stream: Stream; info: TrackInfo };
			try {
				accepted = await untilAbandoned(group, setup);
			} catch (err: unknown) {
				// A setup that finishes just after the close hands back a stream nobody will read.
				void setup.then(
					({ stream }) => stream.abort(error(err)),
					() => void 0,
				);
				throw err;
			}

			hooks.bindGroupCompression(group, accepted.info.compress);
			void this.#runFetchResponse(accepted.stream, group, accepted.info);
		} catch (err: unknown) {
			group.abort(error(err));
			throw err;
		}
	}

	// Resolve the track's timescale, then open the FETCH stream and wait for it to be accepted.
	// Closing the group during that wait resets the stream.
	async #fetchSetup(
		broadcast: Path.Valid,
		consumed: Consumed,
		track: string,
		sequence: number,
		priority: number,
		group: netGroup.Producer,
		deadline: Deadline,
		maxDelay?: Milli,
	): Promise<{ stream: Stream; info: TrackInfo }> {
		const { epoch, valid } = consumed;
		const cancel = new AbortController();
		void group.closed.then((err) => cancel.abort(err ?? new StreamError(StreamCode.Cancel)));
		const signal = valid ? AbortSignal.any([valid, cancel.signal]) : cancel.signal;
		const answered = this.#trackInfo(broadcast, epoch, track, signal, deadline);
		// Only the properties are needed, so the TRACK stream closes however the wait ends.
		answered.then(
			({ stream }) => stream.close(),
			() => {},
		);
		const { info } = await untilClosed(group, answered);
		return this.#exchange(
			{ sendOrder: sendOrder({ priority }), version: this.version },
			async (stream) => {
				await stream.writer.u53(StreamId.Fetch);
				await new FetchMessage({
					broadcast,
					epoch,
					track,
					priority,
					maxDelay: hasFetchMaxDelay(this.version) ? maxDelay : undefined,
					group: sequence,
				}).encode(stream.writer, this.version);
				// A byte or an empty-group FIN accepts the fetch; a reset rejects it.
				// done() buffers that byte so the response pump can decode it normally.
				await untilClosed(group, stream.reader.done());
				return { stream, info };
			},
			signal,
			deadline,
		);
	}

	// Read the FETCH response (bare frames, zigzag-delta-timestamped unless `timescale` is 0)
	// into the group, then FIN. A stream-level failure aborts the group so its reader observes
	// the gap.
	async #runFetchResponse(stream: Stream, group: netGroup.Producer, info: TrackInfo): Promise<void> {
		try {
			const decode = frameDecoder({
				scale: info.timescale ?? 0,
				timed: hasMediaTime(this.version),
				compress: info.compress,
			});

			// Serve until the stream FINs, the group closes, or every reader leaves. A group can
			// stay open indefinitely (a catalog or JSON stream), so an abandoned fetch is stopped by
			// demand, not by the stream ending. `idling` is watched across frames as one stable
			// promise; the check is level-triggered, so a coalesced fetch that arrives before we
			// cancel re-arms and resumes.
			const idle: unique symbol = Symbol("idle");
			let idling = groupUnwanted(group).then((): typeof idle => idle);
			// A decode consumes its frame whenever it lands, so one outstanding across a re-arm is
			// kept and awaited again rather than abandoned with its frame.
			let pending: Promise<WireFrame | undefined> | undefined;
			for (;;) {
				// Buffered frames are written without an await, as in a group stream.
				let frame = pending === undefined ? stream.reader.tryDecode(decode) : undefined;
				if (!frame) {
					pending ??= stream.reader.decodeMaybe(decode);
					const next = await race([pending, group.closed, idling]);
					if (next === idle) {
						if (group.isClosed) break;
						if (group.demand().wanted.peek()) {
							idling = groupUnwanted(group).then((): typeof idle => idle);
							continue;
						}
						// Abandoned mid-group: the truncated group must never end clean.
						throw new StreamError(StreamCode.Cancel, { message: "cancel" });
					}
					pending = undefined;
					// Reset the stream with the group's error, such as Unauthorized on revocation.
					if (next instanceof Error) throw next;
					if (!next) break;
					frame = next;
				}
				hooks.writeGroupFrame(group, frame, frame.inflated);
			}

			group.finish();
			stream.close();
		} catch (err: unknown) {
			const e = error(err);
			group.abort(e);
			stream.abort(e);
		}
	}

	// Reads the subscribe stream's responses until FIN, recording the range the tail is
	// accounted against: SUBSCRIBE_START/END/DROP on lite-05+, and any SUBSCRIBE_DROP after the
	// OK before then, where the FIN itself ends the track. Reading to the FIN, rather than
	// awaiting it, matters: a pull-driven transport stream closes only on a read.
	// SUBSCRIBE_END declares the track's end right away, so a consumer learns it before the
	// last groups arrive. The publisher must declare the end before FIN unless we cancelled
	// locally, or on lite-07 it covered a bounded range; resets and malformed responses reject.
	private async runResponses(stream: Stream, entry: SubscribeEntry): Promise<void> {
		for (;;) {
			const resp = await decodeSubscribeResponseMaybe(stream.reader, this.version);
			// The publisher answered, so its demand stands on the subscription from here.
			entry.held?.close();
			entry.held = undefined;
			entry.answered?.();
			entry.answered = undefined;
			if (!resp) {
				// Before lite-05 the FIN is the track's only end.
				if (!hasTrackStream(this.version)) {
					entry.finished = true;
					return;
				}
				// A bounded range may have widened since, in an update the publisher never read.
				const covered = hasRanges(this.version) && entry.bounded;
				if (entry.end === undefined && !covered && hooks.trackReleased(entry.track).peek() === undefined)
					throw new ProtocolViolation("subscribe stream ended without SUBSCRIBE_END");
				return;
			}

			if ("ok" in resp) throw new ProtocolViolation("duplicate SUBSCRIBE_OK");
			if ("start" in resp) {
				// Every SUBSCRIBE and update we send carries one range, so one start answers it.
				if (hasRanges(this.version) && resp.start.starts.length !== 1)
					throw new ProtocolViolation(`SUBSCRIBE_START resolves ${resp.start.starts.length} ranges, not 1`);
				// The lowest range start is the first group served; none leaves nothing owed.
				entry.start = resp.start.starts.reduce<number | undefined>(
					(lowest, start) =>
						start === undefined || (lowest !== undefined && lowest <= start) ? lowest : start,
					undefined,
				);
				hooks.startTrack(entry.track, { start: entry.start ?? null, requested: entry.requested });
				hooks.reportedLargest(entry.track, resp.start.largest);
				// The groups the SUBSCRIBE asked for below it are not waited for, whatever the
				// demand asks later. One that still arrives is delivered.
				if (entry.requested !== undefined && entry.start !== undefined)
					entry.tail.account(entry.requested, entry.start);
			} else if ("end" in resp) {
				if (entry.end !== undefined) throw new ProtocolViolation("duplicate SUBSCRIBE_END");
				entry.end = resp.end.group;
				// A local close can win the race with the response; there is nothing left to end.
				if (hooks.trackReleased(entry.track).peek() !== undefined) continue;
				try {
					// Lower groups may still be on the wire behind a higher one, so readers hold
					// at a hole below the end until the tail settles.
					hooks.endTrack(entry.track, { group: entry.end, frame: 0 }, resp.end.cause());
				} catch (err) {
					// lite-05 specified an inclusive end, and @moq/net 0.1.3 to 0.1.9 sent one, so
					// there an end below a received group only costs the early boundary: the FIN
					// still finishes the track. Later drafts made it exclusive.
					if (this.version !== Version.DRAFT_05) {
						throw new ProtocolViolation(`invalid SUBSCRIBE_END: ${reason(error(err))}`);
					}
					console.warn(`invalid SUBSCRIBE_END: ${reason(error(err))}`);
				}
			} else if ("drop" in resp) {
				// The publisher will never send these groups, so they are accounted for without a
				// stream, and a reader of one still arriving ends with the drop's error now.
				entry.tail.account(resp.drop.start, resp.drop.end + 1);
				hooks.dropTrackGroups(entry.track, resp.drop.start, resp.drop.end + 1, resp.drop.cause(this.version));
			}
		}
	}

	// Wait for the group streams the publisher still owes once it has ended the subscription.
	//
	// Received headers and SUBSCRIBE_DROP account for the range. An unaccounted
	// sequence, such as a lost datagram, still waits out the grace. Streams whose
	// headers arrived keep reading until their own FIN or reset.
	//
	// Resolves false when a lite-07 publisher's FIN without SUBSCRIBE_END may have left the
	// demand uncovered: it covered the bounded range it read, and the demand has moved since.
	async #settleTail(entry: SubscribeEntry): Promise<boolean> {
		const { tail, track } = entry;

		const complete = () => {
			const groups = track.subscription.peek()?.groups;
			const bounds = groupBounds(groups ?? {});
			// Without SUBSCRIBE_END (older drafts) nothing says which groups are owed, unless a
			// lite-07 publisher covered the bounded range.
			const end =
				entry.end === undefined
					? hasRanges(this.version)
						? bounds.end
						: undefined
					: Math.min(entry.end, bounds.end ?? entry.end);
			if (end === undefined) return false;
			// Without SUBSCRIBE_START the publisher served no group at all.
			if (entry.start === undefined) return true;
			// Owed from the floor the demand last asked for, which an update can move either
			// way, or where SUBSCRIBE_START resolved a live-edge one. The groups the SUBSCRIBE
			// asked for below its SUBSCRIBE_START were accounted for when it arrived.
			const start = groups?.start === undefined ? entry.start : bounds.start;
			return tail.covers(start, end);
		};

		const released = hooks.trackReleased(track);
		await tail.settle(complete, released);
		if (entry.end !== undefined || !hasRanges(this.version) || released.peek() !== undefined) return true;
		const demand = groupBounds(track.subscription.peek()?.groups);
		if (demand.start === entry.asked.start && demand.end === entry.asked.end) return true;
		// Without SUBSCRIBE_START nothing says which ranges the publisher read, so only the
		// groups that arrived prove the demand covered.
		return entry.start !== undefined && complete();
	}

	/**
	 * Send SUBSCRIBE_UPDATE messages whenever the track's aggregate subscription changes.
	 *
	 * Resolves cleanly when the stream or track closes, or a write fails, so the caller can include
	 * this in a race without leaving a dangling pending write that would
	 * become an unhandled rejection if the user calls update after close.
	 *
	 * Peeks the signal at the top of every iteration so that updates which landed
	 * before SubscribeOk arrived (or between iterations, before .next() registered
	 * its listener) aren't lost.
	 */
	async #runSubscriptionUpdates(
		id: bigint,
		broadcast: Path.Valid,
		entry: SubscribeEntry,
		msg: Subscribe,
		stream: Stream,
	): Promise<void> {
		const track = entry.track;
		// A reset stops updates too; the responses decide how the track ends.
		const stopped: Promise<null> = race([hooks.trackReleased(track), stream.reader.closed]).then(
			() => null,
			() => null,
		);
		let lastSent: track.Subscription = {
			priority: msg.priority,
			order: msg.order,
			maxDelay: Milli(msg.maxDelay),
			groups: {
				start: msg.startGroup === undefined ? undefined : { included: msg.startGroup },
				end: msg.endGroup === undefined ? undefined : { excluded: exclusiveGroupEnd(msg.endGroup) ?? 0 },
			},
		};

		// The wanted group is subscription state: an update announces each new one past our
		// largest, and every later update restates it. Joins asking for the same group
		// coalesce into that one update.
		let sentWanted: number | undefined;
		const demand = track.demand();
		const edge = hooks.producerLargest(track);
		for (;;) {
			const largest = edge.peek()?.group;
			const wanted = demand.wanted.peek();
			const refresh =
				hasRefresh(this.version) &&
				largest !== undefined &&
				wanted !== undefined &&
				wanted > largest &&
				wanted !== sentWanted
					? wanted
					: undefined;
			const current = track.subscription.peek();
			if (current === undefined || (refresh === undefined && this.#sameSubscription(current, lastSent))) {
				// Each wake releases the quiet signals too, including when termination wins.
				const next = await race([Signal.race(track.subscription, demand.wanted, edge), stopped]);
				if (next === null) return;
				continue;
			}

			// Demand collapsing to nothing is refused the same way an initial empty
			// request is: the error closes the track, so every local subscriber sees it.
			const bounds = groupBounds(current.groups);
			if (emptyRange({ startGroup: bounds.start, endGroup: bounds.end })) throw new Error(EMPTY_RANGE);

			// A lowered floor owes groups nobody asked for until now.
			if (current.groups?.start !== undefined) {
				const floor = lastSent.groups?.start === undefined ? entry.start : groupBounds(lastSent.groups).start;
				entry.tail.demand(bounds.start, floor ?? Number.POSITIVE_INFINITY);
			}

			// Round-trip the other Subscribe parameters so the publisher doesn't
			// interpret SUBSCRIBE_UPDATE as a reset of ordered/maxDelay/etc.
			const update = new SubscribeUpdate({
				wantedGroup: refresh ?? sentWanted,
				priority: current.priority ?? 0,
				order: current.order,
				maxDelay: current.maxDelay,
				startGroup: current.groups?.start === undefined ? undefined : bounds.start,
				endGroup: inclusiveGroupEnd(bounds.end),
			});
			// Encoded apart from the write, so a bound the wire can't carry still fails the track.
			const encoded = await Writer.collect(this.version, (w) => update.encode(w, this.version));
			// A failed write means the publisher stopped reading, as it does once it finishes the
			// track. The responses decide how the track ends, cleanly at the FIN or with a reset's error.
			try {
				await stream.writer.write(encoded);
			} catch {
				return;
			}
			if (refresh !== undefined) sentWanted = refresh;
			entry.bounded ||= update.endGroup !== undefined;
			lastSent = { ...current };
			console.debug(`subscribe update: id=${id} broadcast=${broadcast} track=${track.name}`);
		}
	}

	#sameSubscription(a: track.Subscription, b: track.Subscription): boolean {
		const ag = groupBounds(a.groups);
		const bg = groupBounds(b.groups);
		// `groupBounds` reads an omitted start as 0. A pre-06 wire tells them apart: omitted
		// joins at the publisher's start, and 0 is group 0. Lite-06 encodes both as 0.
		const sameStart =
			resolvesStart(this.version) || (a.groups?.start === undefined) === (b.groups?.start === undefined);
		// A wire without `Group Order` cannot carry a change to it.
		const sameOrder = !hasOrder(this.version) || (a.order ?? "descending") === (b.order ?? "descending");
		return (
			(a.priority ?? 0) === (b.priority ?? 0) &&
			sameOrder &&
			(a.maxDelay ?? 0) === (b.maxDelay ?? 0) &&
			sameStart &&
			ag.start === bg.start &&
			ag.end === bg.end
		);
	}

	/**
	 * Handles a group message.
	 * @param group - The group message
	 * @param stream - The stream to read frames from
	 *
	 * @internal
	 */
	async runGroup(group: GroupMessage, stream: Reader) {
		const entry = this.#subscribes.get(group.subscribe);
		if (!entry) {
			if (group.subscribe >= this.#subscribeNext) {
				throw new Error(`unknown subscription: id=${group.subscribe}`);
			}

			return;
		}

		const { track, timescale, tail } = entry;
		let producer: netGroup.Producer | undefined;
		const read = tail.open(group.sequence);

		try {
			// The publisher contradicted its own end, which no later group can repair. lite-05
			// specified an inclusive end, so its last group lands on it: the write below drops
			// only that group there.
			if (entry.end !== undefined && group.sequence >= entry.end && this.version !== Version.DRAFT_05) {
				const violation = new ProtocolViolation(
					`group ${group.sequence} is at or past the declared end ${entry.end}`,
				);
				track.abort(violation);
				throw violation;
			}
			producer = hooks.receiveGroup(track, { sequence: group.sequence, start: group.frameStart });

			await readFrames(stream, producer, {
				scale: timescale,
				timed: hasMediaTime(this.version),
				compress: entry.compress,
			});

			producer.finish();
			stream.stop(StreamCode.Cancel);
		} catch (err: unknown) {
			// A violation ends the session, which the connection does once it escapes; anything
			// else ends only this group.
			if (err instanceof ProtocolViolation) {
				producer?.abort(err);
				throw err;
			}
			const e = await sessionCause(this.#quic, err);
			producer?.abort(e);
			stream.stop(e);
		} finally {
			read();
		}
	}

	/**
	 * Receives QUIC datagrams and routes each to its subscription's track producer (lite-05 §6.4).
	 *
	 * Returns immediately on a non-datagram transport or pre-lite-05 version. A decode error or an
	 * unknown subscribe id drops that datagram without tearing down the session (best-effort); the
	 * loop ends only when the datagram stream closes.
	 *
	 * @internal
	 */
	async runDatagrams(): Promise<void> {
		if (!hasDatagrams(this.version) || DatagramStream.maxDatagramSize(this.#quic) === 0) {
			return;
		}

		// Never reject: this loop is awaited alongside the connection's other tasks, so a
		// datagram-stream failure must not tear the whole session down (it's best-effort).
		const reader = DatagramStream.datagramReader(this.#quic);
		if (!reader) return;

		try {
			try {
				for (;;) {
					const { value, done } = await reader.read();
					if (done) break;
					if (!value) continue;

					try {
						await this.#routeDatagram(value);
					} catch (err: unknown) {
						console.debug(`dropping datagram: ${reason(err)}`);
					}
				}
			} finally {
				reader.releaseLock();
			}
		} catch (err: unknown) {
			const e = error(err);
			if (e.message === "The session is closed.") {
				console.debug(`datagram receive stopped: ${e.message}`);
			} else {
				console.warn("datagram stream error", err);
			}
		}
	}

	// Decode one datagram body and hand it to the matching subscription's producer. Drops the
	// datagram (best-effort) if the subscription is unknown or closed.
	async #routeDatagram(payload: Uint8Array): Promise<void> {
		if (payload.byteLength > DatagramMessage.MAX_SIZE) return;
		// An unknown subscription's datagram is dropped below, however it parses.
		const dg = await DatagramMessage.decode(payload, this.version, (id) => ({
			timed: !!this.#subscribes.get(id)?.timescale,
			compressed: this.#subscribes.get(id)?.compress ?? false,
		}));

		const entry = this.#subscribes.get(dg.subscribe);
		if (!entry) return; // Unknown or already-closed subscription.

		const scale = entry.timescale;

		// Dropped where it may be a send time, like a frame's.
		const timestamp =
			dg.timestamp === undefined || !hasMediaTime(this.version)
				? undefined
				: new Time.Timestamp(dg.timestamp, Time.Timescale(scale));
		// A datagram's sequence is never owed a stream, so it never holds the tail open.
		entry.tail.account(dg.sequence, dg.sequence + 1);
		hooks.writeDatagram(entry.track, { sequence: dg.sequence, timestamp, payload: dg.payload }, dg.inflated);
	}

	/**
	 * Opens a PROBE bidi stream to receive bandwidth estimates from the publisher.
	 * Returns immediately if recv bandwidth is not supported.
	 *
	 * Probe is best-effort telemetry: a stream-level failure (peer reset, FIN,
	 * missing peer support, transport hiccup) is caught and logged, never
	 * propagated to the connection. On exit the bandwidth/RTT signals are
	 * cleared so consumers see them as stale.
	 *
	 * @internal
	 */
	// Await the peer's advertised probe level, blocking until its SETUP arrives. The peer
	// MUST send exactly one SETUP, so this resolves once that stream is read.
	async #peerProbeLevel(peerSetup: Signal<Setup | undefined>, scope: Effect): Promise<ProbeLevel | undefined> {
		let setup = peerSetup.peek();
		while (setup === undefined) {
			setup = await scope.race(Signal.race(peerSetup));
			if (setup === undefined) return;
		}
		return setup.probe;
	}

	async runProbe(signal?: AbortSignal): Promise<void> {
		if (!this.#probe) return;
		if (this.version === Version.DRAFT_01 || this.version === Version.DRAFT_02) return;
		const scope = new Effect();
		const abort = signal ? AbortSignal.any([signal, this.#closed.signal]) : this.#closed.signal;
		const stop = () => scope.close();
		abort.addEventListener("abort", stop, { once: true });
		if (abort.aborted) stop();

		// Probe is best-effort: transport failures stay per-stream, but malformed control
		// input ends the session. Drop the estimates so consumers know they're stale.
		try {
			// Lite-05+ gates PROBE on the peer advertising Probe >= Report in its SETUP.
			if (hasSetupStream(this.version) && this.#peerSetup) {
				const level = await this.#peerProbeLevel(this.#peerSetup, scope);
				if (level === undefined || level < ProbeLevel.Report) return;
			}
			if (this.#goingAway() || abort.aborted) return;
			const opening = Stream.open(this.#quic, { version: this.version });
			const stream = await scope.race(opening);
			if (!stream) {
				// A queued transport open cannot be cancelled; give its slot back unused.
				void opening.then((stream) => stream.close()).catch(() => {});
				return;
			}
			scope.cleanup(() => stream.close());
			await scope.race(stream.writer.u53(StreamId.Probe));
			for (;;) {
				const probe = await scope.race(Probe.decodeMaybe(stream.reader, this.version));
				if (!probe) break;
				// Lite-03 carries no RTT; newer drafts retract it with a zero.
				const prev = this.#probe.peek();
				const rtt = probe.rtt !== undefined ? Milli(probe.rtt) : undefined;
				this.#probe.set({
					// `undefined` is the peer reporting "unknown"; a 0 would become a real ABR target.
					estimatedRecvRate: probe.bitrate,
					rtt: hasProbeRtt(this.version) ? rtt : (rtt ?? prev.rtt),
				});
			}
		} catch (err: unknown) {
			closeViolation(this.#quic, err);
			if (!this.#closed.signal.aborted) {
				console.warn("probe stream error", err);
			}
		} finally {
			scope.close();
			abort.removeEventListener("abort", stop);
			this.#probe.set({});
		}
	}

	/**
	 * Wait until every subscribe stream's responses are read to its FIN or reset. Bounded only
	 * once the transport is closed, so that its reads end.
	 *
	 * @internal
	 */
	async drain(): Promise<void> {
		await Promise.all(Array.from(this.#subscribes.values(), (entry) => entry.responses));
	}

	/**
	 * Ends every subscribed track, fetch, and setup exchange with `err`, the session's end, which
	 * defaults to a clean close: they were cut off rather than ended. A track whose publisher
	 * already ended it finishes instead.
	 */
	close(err: Error = new SessionError(SessionCode.Cancel)) {
		this.#closed.abort(err);

		for (const { track, finished } of this.#subscribes.values()) {
			if (finished) track.finish();
			track.abort(err);
		}

		this.#subscribes.clear();

		// This also releases callers still awaiting acceptance.
		for (const { group } of this.#fetches.values()) {
			group.abort(err);
		}
	}
}

// Settles with `step`, or rejects with the group's error once it closes first. A publisher
// may never answer a FETCH, so Subscriber.close() closing the group is what releases it.
async function untilClosed<T>(group: netGroup.Producer, step: Promise<T>): Promise<T> {
	const value = await race([step, group.closed]);
	const closed = group.closed.peek();
	if (closed !== undefined) throw closed ?? new Error("fetch closed before it was accepted");
	return value as T;
}

// Resolve once no reader wants `group`, or it closed.
async function groupUnwanted(group: netGroup.Producer): Promise<void> {
	const demand = group.demand();
	while (demand.wanted.peek() && demand.closed.peek() === undefined) {
		await Signal.race(demand.wanted, demand.closed);
	}
}

// Like untilClosed, but also cancels once every reader has left. Demand is level-triggered, so a
// caller that coalesces onto the group before the check re-arms it.
async function untilAbandoned<T>(group: netGroup.Producer, step: Promise<T>): Promise<T> {
	const idle: unique symbol = Symbol("idle");
	for (;;) {
		const value = await untilClosed(group, race([step, groupUnwanted(group).then((): typeof idle => idle)]));
		if (value !== idle) return value as T;
		if (!group.demand().wanted.peek()) {
			// Close here rather than where the error lands, so no fetch coalesces onto the group
			// in between only to fail with it.
			const err = new StreamError(StreamCode.Cancel, { message: "cancel" });
			group.abort(err);
			throw err;
		}
	}
}

/**
 * A broadcast consumed from a lite session. It resolves `track.Consumer.info()` and
 * `.fetchGroup()` over the wire (lite-05+ TRACK / FETCH streams) by reaching into the
 * {@link Subscriber} it was opened from, the way the Rust `BroadcastConsumer` holds its
 * session. Live subscribes still flow through the inherited requested() queue.
 */
class ConsumeBroadcast extends broadcast.Consumer {
	#subscriber: Subscriber;
	#path: Path.Valid;
	#consumed: Consumed;

	constructor(subscriber: Subscriber, path: Path.Valid, consumed: Consumed, state?: never) {
		super(state);
		overrideBroadcastWire(this, {
			resolveTrackInfo: async (name, hold) => {
				if (!consumed.valid) return subscriber.resolveTrackInfo(path, name, consumed.epoch, hold);
				// Retirement cancels setup, but a one-shot query still releases its TRACK
				// as soon as it answers instead of holding it for the broadcast's lifetime.
				const query = new AbortController();
				const signal = AbortSignal.any([consumed.valid, hold ?? query.signal]);
				try {
					return await subscriber.resolveTrackInfo(path, name, consumed.epoch, signal);
				} finally {
					if (!hold) query.abort();
				}
			},
			// FETCH streams start at lite-05; older sessions offer no fetch at all.
			fetchGroup: hasTrackStream(subscriber.version)
				? (name, sequence, options) => subscriber.fetchGroup(path, name, sequence, options, consumed)
				: undefined,
		});
		this.#subscriber = subscriber;
		this.#path = path;
		this.#consumed = consumed;
	}

	// Preserve the subclass (and its wire-backed info/fetchGroup) when the consume cache shares
	// this broadcast across callers.
	override clone(): ConsumeBroadcast {
		return new ConsumeBroadcast(this.#subscriber, this.#path, this.#consumed, this.shareState());
	}
}
