import { Duration, yieldNow } from "@moq/clock";
import { type Dispose, Effect, type GetPromise, type Getter, Once, race, Signal } from "@moq/signals";
import * as announce from "../announced.ts";
import type { Grant } from "../auth.ts";
import type { AuthSession } from "../auth_session.ts";
import * as broadcast from "../broadcast.ts";
import type { Goaway as Received } from "../connection/goaway.ts";
import type { Probe as ProbeStats } from "../connection/stats.ts";
import { BroadcastCache } from "../consume.ts";
import type * as datagram from "../datagram.ts";
import * as DatagramStream from "../datagram_stream.ts";
import {
	error,
	NotFound,
	ProtocolViolation,
	reason,
	SessionCode,
	SessionError,
	StreamCode,
	StreamError,
	sessionCause,
	toStreamCode,
	unauthorized,
} from "../error.ts";
import { type Judged, rejectGroups } from "../fill.ts";
import * as netGroup from "../group.ts";
import { Cost, type Hop, MAX_HOPS, routesEqual, UNKNOWN_HOP, type WireRoute } from "../hop.ts";
import * as Instance from "../instance.ts";
import {
	type Arriving,
	hiddenBelow,
	hooks,
	maxDelayOf,
	minDelayOf,
	NO_MAX_DELAY,
	type Run,
	removeRun,
	runGaps,
	scopeCaptures,
	scopeHead,
	scopeOverlaps,
	staleHole,
	wireMaxDelay,
} from "../internal.ts";
import type { Lister } from "../listing.ts";
import * as Mint from "../mint.ts";
import * as Path from "../path.ts";
import { sendOrder } from "../priority.ts";
import { type OpenOptions, type Reader, Stream, Writer } from "../stream.ts";
import { type Counted, Tail } from "../tail.ts";
import * as Time from "../time.ts";
import type * as track from "../track.ts";
import { untilAborted } from "../util/abort.ts";
import { closeUnwanted } from "../util/announce.ts";
import { toNumber } from "../util/bigint.ts";
import { Deadline } from "../util/deadline.ts";
import { isAbandoned, isUnrequested, unrequested, unwanted } from "../util/demand.ts";
import { max, min, UNBOUNDED } from "../util/key.ts";
import { LIMITS, type Limits, SlotMap, Slots } from "../util/limits.ts";
import { Budget } from "../util/yield.ts";
import { AT_KEYS, EMPTY, rangesStart, sameRanges, span, Window, wireRanges, wireSpan } from "../window.ts";
import { overrideBroadcastWire, wireOf } from "../wire.ts";
import {
	type AnnounceBroadcast,
	AnnounceHistory,
	AnnounceInit,
	AnnounceOk,
	AnnounceRequest,
	decodeAnnounceBroadcastMaybe,
} from "./announce.ts";
import { AnnounceStream } from "./announce_stream.ts";
import { Datagram as DatagramMessage, Datagrams, DatagramsInfo, DatagramsUpdate } from "./datagram.ts";
import { closeViolation } from "./error.ts";
import { Fetch as FetchMessage } from "./fetch.ts";
import { dropped, passing, RangeFills, type RecoveryHandle, type Refusal, type Refused, refuseRun } from "./fills.ts";
import * as Fit from "./fit.ts";
import { type Group as GroupMessage, readFrames } from "./group.ts";
import { Probe } from "./probe.ts";
import { Routed } from "./routed.ts";
import { ProbeLevel, type Setup } from "./setup.ts";
import { StreamId } from "./stream.ts";
import {
	decodeSubscribeResponse,
	decodeSubscribeResponseMaybe,
	EMPTY_RANGE,
	inclusiveGroupEnd,
	LiveRanges,
	type RangeKind,
	rangeKey,
	requestedRange,
	Subscribe,
	SubscribeUpdate,
	toWireRanges,
	type WireAnchor,
	type WireRange,
} from "./subscribe.ts";
import { TrackInfo, Track as TrackMessage } from "./track.ts";
import { decodeTracksEventMaybe, Tracks as TracksMessage } from "./tracks.ts";
import {
	fillsFromStart,
	hasAnnounceId,
	hasAnnounceOk,
	hasDatagrams,
	hasDatagramTracks,
	hasFetchMaxDelay,
	hasGroupEnd,
	hasInstance,
	hasLengthFirst,
	hasMediaTime,
	hasMinDelay,
	hasOrder,
	hasProbeRtt,
	hasRanges,
	hasRefresh,
	hasRoutes,
	hasSetupStream,
	hasSkip,
	hasSynced,
	hasTrackStream,
	hasTracks,
	keepsSubscribeOpen,
	resolvesStart,
	updateSupported,
	Version,
	waitsForSubscriberFin,
} from "./version.ts";

// A peer's buffered TRACKS events decode without reaching a task boundary, and a flood of adds
// and removals of one name never reaches the announces cap: yield so it cannot starve the host.
const listingBudget = new Budget();
const datagramBudget = new Budget();
// The datagrams held for unanswered subscriptions at once, as many as a Rust pass holds: past it,
// one is dropped and counted rather than stalling the receive queue, which would hold up other
// tracks' datagrams behind their ends.
const HELD_DATAGRAMS = 32;

// What a subscription has set up so far, so a revocation (or abandonment) can reach it at
// any stage.
interface SubscribeSetup {
	consumer?: broadcast.Consumer;
	legacyKey?: string;
	stream?: Stream;
	track?: Stream;
	producer?: track.Producer;
	// Ends a setup still running once it failed, nobody wants it, or the grant was revoked.
	cancel: AbortController;
	// Set once our grant stops covering the broadcast.
	revoked?: Error;
}

/**
 * What a consumed broadcast captured when it was opened: the instance it pins, and its own id.
 * Fetches are shared only within one consume, so a fresh consume for a replaced stream never
 * reads a FETCH the old one opened.
 */
interface Consumed {
	readonly instance?: Instance.Valid;
	readonly id?: number;
	readonly valid?: AbortSignal;
	// Aborted once another stream takes the path, which may answer a request without an instance
	// still unanswered. A withdrawal alone leaves the peer to answer it.
	readonly answering?: AbortSignal;
}

// What a request names to pin it: the serving instance, and on lite-07 its publisher node and the
// depth of the route's prefix.
interface Pin {
	readonly instance?: Instance.Valid;
	readonly node?: bigint;
	readonly depth?: number;
}

interface DatagramEntry {
	producer: datagram.Producer;
	timescale?: Time.Timescale;
	// Set once the publisher ended the track, while the datagrams received with the end route:
	// a session close meanwhile finishes it rather than cutting it off.
	ended?: boolean;
}

// How a received copy's groups are read, and the publisher's max age, which bounds every reader.
interface Framing {
	compress: boolean;
	timescale: number;
	// Undefined while the copy has no info, and null for no bound.
	maxAge?: Duration | null;
}

interface SubscribeEntry {
	datagrams?: DatagramEntry;
	/** Release the stream demand held by readers of the legacy datagram track. */
	datagramDemand?: Dispose;
	legacyKey?: string;
	// The write side: incoming GROUP streams are routed here. The application reads
	// the matching track.Subscriber it got from broadcast.Consumer.subscribe.
	track: track.Producer;
	compress: boolean;
	// Per-frame timestamp scale (0 = none), resolved before this entry is registered.
	timescale: number;
	// The track's timescale as its model declares it, which a time anchor counts in.
	scale?: Time.Timescale;
	// The group streams received, so the subscription can wait for the ones still owed
	// after the publisher ends it.
	tail: Tail;
	// The first group the SUBSCRIBE asked for, if any.
	requested?: bigint;
	// The first group the publisher serves, once SUBSCRIBE_START says: null when it starts no
	// range. And the track's exclusive end, once SUBSCRIBE_FINISH declares it.
	start?: bigint | null;
	end?: bigint;
	// The ranges on the wire by Range ID (lite-07), which each update edits.
	live: LiveRanges;
	// Where SUBSCRIBE_START or SUBSCRIBE_STARTED says each live range starts on lite-07, by Range
	// ID, undefined for one with nothing to deliver.
	starts: Map<bigint, bigint | undefined>;
	// The Min Age in milliseconds each live demand range with no start was added under, by Range
	// ID, undefined for not stated, so the copy knows which of its readers' live joins the start
	// its publisher reports answers, and re-adds the range when the Min Age grows past it.
	joins: Map<bigint, number | undefined>;
	// The first group the ranges last sent ask for, which a lite-07 declaration answers.
	floor?: bigint;
	// What `#declare` last declared, so an unchanged one is not declared again.
	declared?: { start?: bigint | null; requested?: bigint };
	// The narrowest max delay the SUBSCRIBE and every SUBSCRIBE_UPDATE asked for, in milliseconds,
	// which a lite-07 stale drop is recorded under: the wire does not say which update a drop
	// crossed, so it may have been judged under any budget asked before it arrived. Too narrow
	// costs a wider reader a request of its own; too wide would settle it on a group it could
	// still get.
	staleBudget: number;
	// The subscribe stream, from the setup that registered this entry to its end.
	stream: SubscribeStream;
	// How many SUBSCRIBE_UPDATEs went out, so a tail settling past SUBSCRIBE_FINISH waits a fresh
	// grace for what one sent meanwhile asks for.
	sent: number;
	// The ranges the SUBSCRIBE asked for, as `wireRanges` gives them.
	asked: track.Range[];
	// A recovery's own subscription, whose groups are cancelled once nobody wants them, as a FETCH's.
	filling?: boolean;
	// Lite-07: the copy's fills of more than one group, which ask through this subscription.
	fills?: RangeFills;
	// The Range IDs of the fills' live ranges; with the probes', the rest are the demand's, which
	// alone say where the live feed starts.
	fillIds: Set<bigint>;
	// The Range IDs of the probes' live ranges: a range of its own from each time start the copy's
	// readers wait on, asking only where the publisher starts it. Like a fill's, it says nothing of
	// where the live feed is.
	probeIds: Set<bigint>;
	// The narrowest max delay in milliseconds each live range was asked under, by Range ID, from when
	// it was added until its start was answered: the wire does not say which update the publisher
	// resolved it under. An answer with no group at or before its time holds only for a reader
	// within it.
	budgets: Map<bigint, number>;
	// The groups the fills' ranges ask for, as last sent: widened with the demand's ranges until
	// they fit, so a run may cover more than the fills ask.
	fillGroups: readonly Run[];
	// Bumped as what the fills ask changes, for the update loop.
	fillsChanged: Signal<number>;
	// Until the SUBSCRIBE's first response (lite-05+); see `Unanswered`.
	unanswered?: Unanswered;
	// Lite-07: the hold each stream reading a group has on the subscription's end, by key, one
	// per stream, so stream credit bounds them. A browser can lose the reset of a stream the publisher
	// gave up on (Chromium drops one that reaches a stream before the page took it), so a drop naming
	// the group excuses it, and the end waits for the stream at most the grace.
	reading?: Map<bigint, Counted>;
}

// A subscribe stream's phases. Every SUBSCRIBE_UPDATE and our FIN go out on it.
type SubscribeStream =
	// Being set up: its responses are not read yet.
	| { kind: "setup" }
	// Its responses are read until the publisher's FIN or reset, which Subscriber.drain waits on.
	| { kind: "reading"; responses: Promise<void> }
	// Our side is FINned too, which lite-05+ does once the tail settles past the publisher's end.
	| { kind: "fin"; responses: Promise<void> }
	// The publisher's FIN finished the track, as only it does before lite-05, where our side
	// never FINs.
	| { kind: "finished"; responses: Promise<void> };

// A lite-05+ SUBSCRIBE waiting for its first response.
interface Unanswered {
	// The TRACK stream that answered, held open as interest in the track, so the publisher's
	// demand never lapses between the two.
	held: Stream;
	// Aborted at the first response. Until then a SUBSCRIBE without an instance may be answered
	// by whichever stream won the path, so a replacement refuses it.
	answered: AbortController;
}

/**
 * The instance a session minted for each path a peer advertises without one (every version
 * before lite-07), shared by its announce streams: a peer serves one broadcast at a path at a
 * time, so the path advertised on two streams at once is one stream. The peer ending it on any
 * stream retires its instance, so the next advertisement is a new stream with a fresh one, even
 * while a lagging stream still holds the old: holders alone cannot tell a replacement from
 * continuity. A stream closing only lets go of it. Mirrors `Minted` in rs/moq-net.
 * @internal
 */
export class Minted {
	#paths = new Map<Path.Valid, { instance: Instance.Valid; holders: number }>();

	acquire(path: Path.Valid): Instance.Valid {
		let entry = this.#paths.get(path);
		if (!entry) {
			entry = { instance: Mint.fresh(), holders: 0 };
			this.#paths.set(path, entry);
		}
		entry.holders += 1;
		return entry.instance;
	}

	/**
	 * Let go of `instance`, retiring it if the peer `ended` its advertisement. A no-op once
	 * another stream's end retired it.
	 */
	release(path: Path.Valid, instance: Instance.Valid, ended: boolean) {
		const entry = this.#paths.get(path);
		if (entry?.instance !== instance) return;
		entry.holders -= 1;
		if (ended || entry.holders === 0) this.#paths.delete(path);
	}
}

/** The groups a fill's range asks for: absolute at both ends, as `RangeFills` asks them. */
function fillRun(range: WireRange): Run | undefined {
	const start = range.start?.absolute;
	const end = range.end?.absolute;
	return start === undefined || end === undefined ? undefined : { start, end };
}

function sameRuns(a: readonly Run[], b: readonly Run[]): boolean {
	return a.length === b.length && a.every((run, i) => run.start === b[i].start && run.end === b[i].end);
}

/** Subscribe to the peer and own its receive-side protocol tasks.
 * @internal
 */
export class Subscriber {
	#quic: WebTransport;
	// Advertisements the peer holds live across every announce stream on this session.
	#announces: Slots;
	// The instances minted for the paths the peer advertises without one, shared by every
	// announce stream on this session.
	#minted = new Minted();

	// The version of the connection.
	readonly version: Version;

	// Shared with the Publisher so reflected announces can be dropped on receipt.
	readonly hop: Hop;

	// Registered only after immutable TRACK_INFO or the older drafts' defaults resolve,
	// so every incoming group and datagram already knows its timestamp framing.
	#subscribes = new Map<bigint, SubscribeEntry>();
	#datagrams = new Map<bigint, DatagramEntry>();
	#legacyDatagrams = new Map<string, DatagramEntry>();
	#legacyRequests = new Map<string, datagram.Request>();
	// Lite-07 datagram requests sent and not yet answered.
	#unanswered = new Set<bigint>();
	// Datagrams for a subscription still awaiting its answer, in arrival order, routed a host task
	// later while the rest go on.
	#held: { subscribe: bigint; payload: Uint8Array }[] = [];
	#heldRouted?: Promise<void>;
	// Set while the receive loop yields to its budget, which may leave datagrams queued.
	#receiveYield?: Promise<void>;
	/** Datagrams received for a subscription with no datagram track; see `Stats.datagramsUnrouted`. */
	datagramsUnrouted = 0;
	// A SUBSCRIBE sent alongside its TRACK, until its TRACK_INFO lands: settles with the
	// entry, or `undefined` if it never does.
	#pending = new Map<bigint, Promise<SubscribeEntry | undefined>>();
	#subscribeNext = 0n;

	// Lite-07: the claims the peer sends, shared by every `routes()` reader.
	#routed?: Routed;
	// Dedup consumed broadcasts per path: repeat consume() calls share one subscription.
	#consumes = new BroadcastCache();
	#consumeNext = 0;

	// Dedup in-flight one-shot fetches, keyed by [consume, broadcast, instance, track, key].
	// Concurrent (or repeat, while still open) fetchGroup() calls for the same group of one consume
	// share one FETCH stream and each get an independent mirror; the entry is evicted once the
	// group closes.
	#fetches = new Map<string, { group: netGroup.Producer; accepted: Promise<void> }>();

	// The peer's PROBE estimates, written as they arrive (Lite03+ only).
	#probe?: Signal<ProbeStats>;

	// The peer's SETUP (lite-05+), undefined until it arrives. Gates opening the PROBE
	// stream on the peer having advertised Probe >= Report.
	#peerSetup?: Signal<Setup | undefined>;

	// Settles when the peer sends GOAWAY, ranking this session's routes last.
	#goaway?: GetPromise<Received>;

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
		goaway?: GetPromise<Received>,
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
		if (hasRoutes(version)) {
			const stream = AnnounceStream.of(quic, version);
			this.#routed = new Routed({
				quic,
				version,
				consumes: this.#consumes,
				slots: this.#announces,
				drained: (route) => this.#drained(route),
				goaway,
				grant,
				send: (msg) => stream.send(msg),
			});
		}
	}

	// Whether the peer has sent GOAWAY. Requests keep opening here until a replacement
	// session's route outranks this one.
	#goingAway(): boolean {
		return this.#goaway?.peek() !== undefined;
	}

	// A route ranks last once the peer has asked us to leave. A later announce on a
	// draining session must not win selection, however cheap the path it advertises.
	#drained(route: WireRoute): WireRoute {
		return this.#goingAway() ? { ...route, draining: true } : route;
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
	 * also asks for those whose subtree overlaps it. A lite-07 stream reports the best claim at
	 * each path, or each instance's with `options.claims`.
	 */
	routes(scope: Path.Pattern = Path.Pattern.all(), options?: announce.RouteOptions): announce.Routes {
		const announced = new announce.Producer();
		// The wire speaks announce interest by prefix, and echoes suffixes beneath it.
		const consumer = announced.consume();
		// The last reader leaving ends the stream, which cancels the request.
		closeUnwanted(announced);
		const prefix = scopeHead(scope);
		const hidden = options?.hidden ?? false;
		const covering = options?.covering ?? false;
		if (this.#routed) {
			// Lite-07: every reader shares the session's one Announce Stream.
			this.#routed.attach(announced, { prefix, scope, hidden, covering, claims: options?.claims ?? false });
		} else {
			void this.#runAnnounced(announced, prefix, scope, hidden, covering);
		}
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
		const msg = new AnnounceRequest(prefix, this.hop);
		const visible = (path: Path.Valid) =>
			(covering ? scopeOverlaps(scope, path) : scope.matches(path)) && (hidden || !hiddenBelow(prefix, path));

		// Opened outside the try so the catch can reach it: a protocol violation below has
		// to reset the stream, not just close our side of it.
		let stream: Stream;
		try {
			// The consumer leaving gives up the wait for a slot, so the next open takes it.
			const left = new AbortController();
			void race([announced.closed]).then(() => left.abort());
			const opened = await Stream.tryOpen(this.#quic, { version: this.version, signal: left.signal });
			// Handed a stream as the consumer left: give the slot back unused.
			if (!opened || announced.closed.peek() !== undefined) {
				opened?.close();
				return;
			}
			stream = opened;
		} catch (err: unknown) {
			announced.close(error(err));
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
			route: WireRoute;
			captures: Path.Pattern[] | undefined;
		};
		// Each holds one of the session's announce slots, reflected and hidden ones included.
		const advertised = new SlotMap<Path.Valid, Advertisement>(this.#announces);
		// The paths this stream holds a session-minted instance for: a version before lite-07
		// names none, so each advertisement stands for one stream until it ends.
		const minted = new Map<Path.Valid, Instance.Valid>();
		const mint = (path: Path.Valid): Instance.Valid => {
			let instance = minted.get(path);
			if (instance === undefined) {
				instance = this.#minted.acquire(path);
				minted.set(path, instance);
			}
			return instance;
		};
		// The peer ended the advertisement, which retires its instance.
		const unmint = (path: Path.Valid) => {
			const instance = minted.get(path);
			if (instance === undefined) return;
			minted.delete(path);
			this.#minted.release(path, instance, true);
		};

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
					// Receive ANNOUNCE_INIT first. Each suffix takes an announce slot, so a set past
					// what is left of the budget is refused before it is decoded.
					const budget = this.#announces.available();
					const init = await race([
						AnnounceInit.decode(stream.reader, this.version, budget),
						announced.closed,
					]);
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
						const route = this.#drained({
							instance: mint(path),
							epoch: 0n,
							hops: [UNKNOWN_HOP],
							cost: Cost.zero,
						});
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

			// A draining peer usually stops announcing, so rank its routes last from the GOAWAY
			// itself. Waiting for another message would leave the route primary until the
			// session closed. Idempotent: a route already draining emits nothing. A GOAWAY that
			// already arrived needs no listener, since `#drained()` marked every route above.
			const drainAdvertised = () => {
				if (announced.closed.peek() !== undefined) return;
				for (const [path, ad] of advertised) {
					if (!ad.live) continue;
					const route = { ...ad.route, draining: true };
					if (routesEqual(ad.route, route)) continue;
					advertised.set(path, { ...ad, route });
					this.#consumes.announce(path, announced, route);
					announced.append({ prefix: path, captures: ad.captures, kind: "update", route });
				}
			};
			stopDrain = this.#goaway?.changed(() => drainAdvertised());

			// Lite06+: announce ids. Each received `active` implicitly assigns the next
			// per-stream ordinal; `endedId`/`update`/`restart` reference it, and lite-07 bases
			// copy from it. Tracked even for announces we skip as reflected, since the sender
			// doesn't know we skipped.
			const history = new AnnounceHistory();
			// Only lite-07 refuses a forwarded path, and it never reaches here.
			const known = (path: Path.Valid | undefined): Path.Valid => {
				if (path === undefined) throw new ProtocolViolation("a refused path before lite-07");
				return path;
			};

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
				// Present on active and restart from lite-07; an update never changes them.
				let instance: Instance.Valid | undefined;
				let epoch: bigint | undefined;
				// The advertisement now serves another instance or epoch (lite-07).
				const restart = false;

				switch (announce.status) {
					case "active": {
						const suffix = hasAnnounceId(this.version) ? known(history.start(announce)) : announce.suffix;
						// The wire names the suffix beneath the interest prefix; the consumer
						// sees the covered path from the session root.
						path = Path.join(prefix, suffix);
						active = true;
						hops = announce.hops ?? [];
						cost = announce.cost;
						break;
					}
					case "ended":
						path = Path.join(prefix, announce.suffix);
						active = false;
						break;
					case "endedId":
						// Resolve and retire the id; an unknown or retired id is a protocol violation.
						path = Path.join(prefix, known(history.end(announce.id)));
						active = false;
						break;
					case "update":
						// Resolve the id; it stays live (the replacement reuses it).
						path = Path.join(prefix, history.get(announce.id));
						active = true;
						hops = announce.hops ?? [];
						cost = announce.cost;
						break;
					case "skipped":
						continue;
					default:
						// The restart and the route layer are lite-07's, on its Announce Stream.
						throw new ProtocolViolation(`unexpected ${announce.status} before lite-07`);
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
				if (announce.status === "active" && !duplicateIsUpdate && advertised.has(path)) {
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

				if (!active) {
					retract();
					unmint(path);
					continue;
				}

				// An update keeps the instance its announcement named, even through a placeholder
				// below: a later update that is not reflected still names that stream. A version
				// before lite-07 names none, so the session mints one per advertisement.
				if (!restart) {
					const previous = advertised.get(path)?.route;
					instance ??= previous?.instance;
					epoch ??= previous?.epoch;
				}
				if (instance === undefined) {
					instance = mint(path);
					epoch = 0n;
				}

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
							route: { instance, epoch, hops: full, cost: Cost.zero },
							captures: undefined,
						});
						continue;
					}
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
						route: { instance, epoch, hops: [], cost: Cost.zero },
						captures: undefined,
					});
					continue;
				}
				const route = this.#drained({ instance, epoch, hops: fullHops, cost: cost ?? Cost.zero });
				const captures = scopeCaptures(scope, path);
				if (!visible(path)) {
					advertised.set(path, { live: false, route, captures });
					continue;
				}

				// Another instance or epoch: whatever was consumed at the path may be stale, so the
				// next consume subscribes fresh, while the handles already out keep theirs.
				const previous = advertised.get(path);
				if (restart && previous?.live) {
					advertised.set(path, { live: true, route, captures });
					this.#consumes.announce(path, announced, route, true);
					console.debug(`announced: broadcast=${path} restart=true instance=${instance} epoch=${epoch}`);
					announced.append({ prefix: path, captures, kind: "restart", route });
					continue;
				}

				// A second advertisement for a path we already carry is an update: either an
				// explicit ANNOUNCE_UPDATE, or (lite-05) a duplicate ANNOUNCE. It updates the
				// route in place, so a forwarder re-prices without retracting.
				if (previous?.live) {
					// An update preserves the advertisement's instance.
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

				console.debug(`announced: broadcast=${path} active=true instance=${instance}`);
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
			// The stream closing ends none of its advertisements, which another may still carry.
			for (const [path, instance] of minted) this.#minted.release(path, instance, false);
			minted.clear();
			stopDrain?.();
		}
	}

	/**
	 * Lite-07: one message the peer sent on its Announce Stream. Its routes and claims apply
	 * here; its own ANNOUNCE_REQUEST only has to arrive outside a DEAD's updates.
	 *
	 * @internal
	 */
	announce(msg: AnnounceBroadcast): Promise<void> {
		if (!this.#routed) throw new ProtocolViolation("Announce Stream before lite-07");
		return this.#routed.receive(msg);
	}

	/**
	 * Lite-07: the peer finished its Announce Stream, so it is leaving and its routes end.
	 *
	 * @internal
	 */
	announceFinished(): void {
		this.#routed?.finish();
	}

	/**
	 * Lite-07: this side finished its Announce Stream, so it asks the peer for nothing more.
	 *
	 * @internal
	 */
	announceSilenced(): void {
		this.#routed?.silence();
	}

	/**
	 * Consumes a broadcast from the connection.
	 *
	 * Deduplicated per serving instance: repeat calls for the same path share one reference-counted
	 * broadcast (and one upstream subscription). The shared broadcast closes once every caller has
	 * closed its handle, so callers close normally.
	 *
	 * @param name - The name of the broadcast to consume
	 * @param pin - Consume only this instance, over its best route. A minted one names an
	 * announcement rather than a stream, so it pins nothing on a wire that carries no instance.
	 * @returns A Broadcast instance
	 */
	consume(path: Path.Valid, pin?: Instance.Valid): broadcast.Consumer {
		const instance =
			pin !== undefined && (hasInstance(this.version) || !Instance.isMinted(pin))
				? this.#consumes.pinned(path, pin)
				: this.#consumes.instance(path);
		return this.#consumes.get(path, instance) ?? this.#createConsume(path, instance);
	}

	#createConsume(path: Path.Valid, instance: ReturnType<BroadcastCache["instance"]>): broadcast.Consumer {
		const consumed = {
			instance: instance.route.instance,
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
				void this.#runSubscribe(path, consumed, request, consumer);
			}
		})();

		void (async () => {
			for (;;) {
				const request = await wireOf(consumer).requestedDatagrams();
				if (!request) return;
				void this.#runDatagramRequest(path, consumed, consumer, request);
			}
		})();
		return this.#consumes.insert(path, instance, consumer);
	}

	// Each request owns its setup and subscription until it ends or loses its last reader.
	async #runDatagramRequest(
		path: Path.Valid,
		consumed: Consumed,
		consumer: broadcast.Consumer,
		request: datagram.Request,
	): Promise<void> {
		const cancel = new AbortController();
		const signal = AbortSignal.any([
			cancel.signal,
			this.#closed.signal,
			...(consumed.valid ? [consumed.valid] : []),
		]);
		let producer: datagram.Producer | undefined;
		let stream: Stream | undefined;
		const id = this.#subscribeNext++;
		const legacyKey =
			hasDatagrams(this.version) && !hasDatagramTracks(this.version)
				? JSON.stringify([consumed.id, request.name])
				: undefined;
		// A stream answer can arrive while this request waits for authorization.
		if (legacyKey !== undefined) this.#legacyRequests.set(legacyKey, request);
		const unused = async () => {
			while (request.priority.peek() !== undefined && request.closed.peek() === undefined)
				await Signal.race(request.priority, request.closed);
			// An accepted legacy producer belongs to the stream subscription, including gaps
			// between datagram readers. Its priority watcher releases only their extra demand.
			if (legacyKey !== undefined && this.#legacyDatagrams.has(legacyKey)) return;
			if (request.priority.peek() === undefined && (!producer || hooks.closeUnusedDatagrams(producer)))
				cancel.abort(new StreamError(StreamCode.Cancel));
		};
		void unused();
		const grant = this.#grant?.subscribe(() => {
			if (this.#denied(path)) cancel.abort(unauthorized(path));
		});
		try {
			if (
				!hasDatagrams(this.version) ||
				DatagramStream.maxDatagramSize(this.#quic) === 0 ||
				!DatagramStream.canReceiveDatagrams(this.#quic)
			)
				throw new Error("transport does not support datagram tracks");
			const ready = await this.#auth?.ready(path, "subscribe", {
				signal,
				until: () => Promise.resolve(request.closed),
			});
			if (ready === false) throw new StreamError(StreamCode.Cancel);
			if (this.#denied(path)) throw unauthorized(path);
			signal.throwIfAborted();
			if (!hasDatagramTracks(this.version)) {
				await this.#legacyDatagramRequest(consumed, consumer, request, signal);
				return;
			}
			const deadline = new Deadline();
			let priority = request.priority.peek() ?? 0;
			const answered = await deadline.wait(
				this.#exchange(
					{ version: this.version },
					async (opened) => {
						stream = opened;
						this.#unanswered.add(id);
						await opened.writer.u53(StreamId.Datagrams);
						await new Datagrams({
							id,
							broadcast: path,
							instance: consumed.instance,
							track: request.name,
							priority,
						}).encode(opened.writer, this.version);
						const info = await DatagramsInfo.decode(opened.reader, this.version);
						return info;
					},
					signal,
					deadline,
				),
			);
			signal.throwIfAborted();
			if (!stream) throw new Error("datagram stream was not opened");
			producer = request.accept(answered);
			const route: DatagramEntry = { producer, timescale: answered.timescale };
			this.#datagrams.set(id, route);
			this.#unanswered.delete(id);
			const opened = stream;
			const receiving = producer;
			// The only thing following DATAGRAMS_INFO is the publisher's FIN. Its last datagram may
			// arrive alongside, so the datagrams already received route first.
			const ended = opened.reader.done().then(async (done) => {
				if (!done) throw new ProtocolViolation("data after DATAGRAMS_INFO");
				route.ended = true;
				await this.#datagramsRouted(id);
				receiving.finish();
			});
			void ended.catch(() => {});
			while (producer.closed.peek() === undefined) {
				const next = producer.priority.peek();
				if (next === undefined && hooks.closeUnusedDatagrams(producer)) break;
				if (next !== undefined && next !== priority) {
					await untilAborted(new DatagramsUpdate(next).encode(opened.writer, this.version), signal);
					priority = next;
					// The priority may have changed again during the write, before the wait below.
					continue;
				}
				await untilAborted(race([ended, Signal.race(producer.priority, producer.closed)]), signal);
			}
			const closed = producer.closed.peek();
			if (closed instanceof Error) throw closed;
			stream.writer.close();
		} catch (err) {
			closeViolation(this.#quic, err);
			const cause = error(err);
			cancel.abort(cause);
			request.reject(cause);
			stream?.abort(cause);
		} finally {
			if (legacyKey !== undefined && this.#legacyRequests.get(legacyKey) === request)
				this.#legacyRequests.delete(legacyKey);
			grant?.();
			this.#unanswered.delete(id);
			this.#datagrams.delete(id);
			cancel.abort(new StreamError(StreamCode.Cancel));
			stream?.reader.stop(StreamCode.Cancel);
		}
	}

	// Lite-05/06: the shared producer holds stream demand while any datagram reader exists.
	async #legacyDatagramRequest(
		consumed: Consumed,
		consumer: broadcast.Consumer,
		request: datagram.Request,
		signal: AbortSignal,
	): Promise<void> {
		const key = JSON.stringify([consumed.id, request.name]);
		const subscriber = wireOf(consumer).subscribe(request.name, { priority: request.priority.peek() ?? 0 });
		try {
			await untilAborted(subscriber.info(), signal);
			const shared = this.#legacyDatagrams.get(key);
			if (!shared) throw new Error("stream subscription did not bind its datagram track");
			const producer = shared.producer;
			// Once registered, the shared producer owns demand regardless of which kind joined first.
			subscriber.close();
			const closed = await untilAborted(Promise.resolve(producer.closed), signal);
			if (closed instanceof Error) throw closed;
		} finally {
			subscriber.close();
		}
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
		// not replaced until the peer answers: a TRACKS request without an instance lists whichever
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
		// Each listed track holds an announces slot: discovery state the peer makes us hold. Each
		// kind is a namespace apart.
		const listedTracks = new Set<string>();
		const listedDatagrams = new Set<string>();
		const listed = (datagrams?: boolean) => (datagrams ? listedDatagrams : listedTracks);
		const unused = new AbortController();
		const stop = AbortSignal.any([unused.signal, revoked.signal, this.#closed.signal]);
		const disposeWanted = lister.wanted.subscribe((wanted) => {
			if (!wanted) unused.abort(new StreamError(StreamCode.Cancel, { message: "listing unused" }));
		});
		let stream: Stream | undefined;
		try {
			const cut = valid ? AbortSignal.any([stop, valid]) : stop;
			stream = await Stream.tryOpen(this.#quic, { version: this.version, signal: cut });
			// Gave up, perhaps handed a stream as it did: the catch below hands back any stream.
			if (!stream || cut.aborted) throw cut.reason;
			await stream.writer.u53(StreamId.Tracks);
			await new TracksMessage(broadcast, consumed.instance).encode(stream.writer, this.version);
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
					if (listed(event.datagrams).has(event.name))
						throw new ProtocolViolation(`track listed twice: ${event.name}`);
					if (!this.#announces.tryAcquire()) {
						throw new SessionError(SessionCode.TooManyRequests, { reason: "too many listed tracks" });
					}
					listed(event.datagrams).add(event.name);
					if (event.datagrams) {
						const info = { priority: event.info.priority, timescale: event.info.timescale ?? null };
						lister.add(event.name, { datagrams: true, info });
					} else {
						lister.add(event.name, { info: this.#toModelInfo(event.info) });
					}
				} else if (event.kind === "removed" || event.kind === "finished") {
					if (!listed(event.datagrams).delete(event.name))
						throw new ProtocolViolation(`unlisted track removed: ${event.name}`);
					this.#announces.release();
					if (event.kind === "finished") lister.finish(event.name, event.datagrams);
					else lister.remove(event.name, event.datagrams);
				} else {
					if (synced) throw new ProtocolViolation("duplicate SYNCED");
					synced = true;
					// Synced when the peer is, so a track listed after it is new upstream too.
					lister.synced();
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
			this.#announces.release(listedTracks.size + listedDatagrams.size);
		}
	}

	async #runSubscribe(
		broadcast: Path.Valid,
		consumed: Consumed,
		request: track.Request,
		consumer: broadcast.Consumer,
	) {
		const id = this.#subscribeNext++;
		if (wireRanges(request.subscription.ranges ?? [{}]).length === 0) {
			request.reject(new Error(EMPTY_RANGE));
			return;
		}
		const refused = unauthorized(broadcast);
		try {
			for (;;) {
				const ready = await this.#auth?.ready(broadcast, "subscribe", {
					signal: consumed.valid,
					until: () => unrequested(request),
				});
				if (ready !== false) break;
				// Demand returning before we commit is not abandonment, matching the serving loop.
				if (!isUnrequested(request)) continue;
				request.reject(new Error("subscribe abandoned before it was authorized"));
				return;
			}
		} catch (err) {
			request.reject(error(err));
			return;
		}
		// Armed before the first check and held to the end, so a shrink while the
		// subscription sets up is never missed: it resets whatever reached the wire.
		const state: SubscribeSetup = {
			cancel: new AbortController(),
			consumer,
			legacyKey: JSON.stringify([consumed.id, request.name]),
		};
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
		const { valid } = consumed;
		const ranges = wireRanges(request.subscription.ranges ?? [{}]);
		const msg = this.#subscribeMessage(id, broadcast, consumed, request.name, request.subscription, ranges);

		console.debug(`subscribe start: id=${id} broadcast=${broadcast} track=${request.name}`);

		// The publisher's application may hold its answer, so the setup waits for it until the
		// last subscriber leaves, the broadcast goes, or the session ends. The stream handles
		// flow back via `state` so a failed setup can close them even if they open later, and
		// `cancel` ends a setup still running, resetting its TRACK stream.
		const retire = this.#retirer(state, valid);
		valid?.addEventListener("abort", retire);
		if (valid?.aborted) retire();
		const deadline = new Deadline();
		const setup = this.#openSubscribe(state, msg, ranges, request, id, deadline);

		// Subscribers and info lookups explicitly own setup, including the handoff from
		// metadata acceptance to reader attachment. Release it when the last owner leaves.
		const left = new AbortController();
		const demand = request.demand();
		const wanted = hooks.requestWanted(request);
		let unwatch: Dispose | undefined;
		const watch = () => {
			if (isUnrequested(request)) {
				left.abort(
					new StreamError(StreamCode.Cancel, { message: "subscribe abandoned before it was accepted" }),
				);
				state.cancel.abort(left.signal.reason);
			} else {
				unwatch = Signal.race(wanted, hooks.demandReleased(demand)).changed(watch);
			}
		};
		watch();

		let opened: { stream: Stream; entry: SubscribeEntry };
		try {
			opened = await deadline.wait(untilAborted(untilAborted(setup, valid), left.signal));
			console.debug(`subscribe ok: id=${id} broadcast=${broadcast} track=${request.name}`);
		} catch (err) {
			closeViolation(this.#quic, err);
			// Local abandonment commits without yielding after the demand check. A revocation
			// says so instead.
			const e = state.revoked ?? (left.signal.aborted ? left.signal.reason : await sessionCause(this.#quic, err));
			state.cancel.abort(e);
			valid?.removeEventListener("abort", retire);
			request.reject(e);
			this.#subscribes.delete(id);
			console.warn(`subscribe error: id=${id} broadcast=${broadcast} track=${request.name} error=${reason(e)}`);
			// Close the streams open now, since a write blocked on flow control may never let
			// setup settle, and any that open later once it does. Cover both branches: setup
			// may resolve late, or it may reject (e.g. encode/decode failure) after a stream
			// is open.
			const leave = () => {
				state.stream?.abort(e);
				state.track?.close();
			};
			leave();
			setup.then(leave, leave);
			return;
		} finally {
			unwatch?.();
		}
		valid?.removeEventListener("abort", retire);

		const { stream, entry } = opened;
		// Lite-05+ FETCHes what the copy's readers miss for as long as the copy lives, across
		// every subscription that feeds it.
		if (hasTrackStream(this.version)) void this.#runFills(broadcast, consumed, entry);
		await this.#serveStream(id, broadcast, consumed, state, stream, entry, msg);
	}

	// The SUBSCRIBE for `subscription`, asking for `ranges`, its wire ranges in the model. Its own
	// wire ranges wait for the track's timescale when they name a time (see `#withRanges`).
	#subscribeMessage(
		id: bigint,
		broadcast: Path.Valid,
		consumed: Consumed,
		name: string,
		subscription: Readonly<track.Subscription>,
		ranges: readonly track.Range[],
	): Subscribe {
		const span = wireSpan(ranges);
		return new Subscribe({
			id,
			broadcast,
			instance: consumed.instance,
			track: name,
			priority: subscription.priority ?? 0,
			order: subscription.order,
			maxDelay: wireMaxDelay(subscription),
			minDelay: minDelayOf(subscription),
			startGroup: span.start,
			endGroup: inclusiveGroupEnd(span.end),
		});
	}

	// Ends a subscription's setup once `signal` aborts: its streams, and the track it accepted.
	#retirer(state: SubscribeSetup, signal?: AbortSignal): () => void {
		return () => {
			state.cancel.abort(signal?.reason);
			state.producer?.abort(error(signal?.reason));
			state.stream?.abort(error(signal?.reason));
		};
	}

	// Serve one SUBSCRIBE stream into its track until it ends.
	async #serveStream(
		id: bigint,
		broadcast: Path.Valid,
		consumed: Consumed,
		state: SubscribeSetup,
		stream: Stream,
		entry: SubscribeEntry,
		msg: Subscribe,
	): Promise<void> {
		const { answering } = consumed;
		const producer = entry.track;
		// Sent but unanswered (lite-05+; older drafts already waited for SUBSCRIBE_OK), only a
		// replacement refuses it, since that instance may be the one answering.
		const replaced = this.#retirer(state, answering);
		if (entry.unanswered && answering) {
			answering.addEventListener("abort", replaced, { signal: entry.unanswered.answered.signal });
			if (answering.aborted) replaced();
		}
		try {
			// Watch for subscription changes and send SUBSCRIBE_UPDATE. Lite01/Lite02
			// don't carry SUBSCRIBE_UPDATE on the wire, so skip the watcher there
			// and just wait on the stream/track like before.
			//
			// The publisher sends responses on this stream until its FIN. Group streams can
			// still be in flight, so the track ends only once the tail is accounted for. A reset
			// rejects instead, so the track ends with that error rather than a clean tail.
			const responses = this.#read(stream, entry);
			let tailSettled = false;
			const closed = responses.then(async () => {
				await this.#settleTail(entry);
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
			let reason_: unknown;
			for (;;) {
				reason_ = await race([done, unwanted(demand).then(() => idle)]);
				if (reason_ === idle && hooks.demandReleased(demand).peek() === undefined && demand.wanted.peek())
					continue;
				break;
			}

			// A settled lite-05+ tail acknowledges completion with FIN, without cancelling
			// the publisher's already-finished receive half.
			if (tailSettled && waitsForSubscriberFin(this.version)) this.#fin(stream, entry);
			else if (entry.stream.kind !== "fin") stream.close();
			console.debug(`subscribe close: id=${id} broadcast=${broadcast} track=${producer.name}`);

			// Only a received SUBSCRIBE_FINISH, or before lite-05 a FIN, declares the track's end: a
			// subscription nobody wants anymore is a released copy, not finished content.
			if (tailSettled && entry.stream.kind === "finished") producer.finish();
			hooks.releaseTrack(producer);
		} catch (err) {
			const e = await sessionCause(this.#quic, err);
			producer.abort(e);
			console.warn(`subscribe error: id=${id} broadcast=${broadcast} track=${producer.name} error=${reason(e)}`);
			stream.abort(e);
		} finally {
			answering?.removeEventListener("abort", replaced);
			entry.tail.close();
			entry.unanswered?.held.close();
			entry.datagramDemand?.();
			if (entry.datagrams) {
				// The stream track's end is its datagrams' end: those received with it route first.
				if (entry.end !== undefined && !(hooks.trackReleased(producer).peek() instanceof Error)) {
					entry.datagrams.ended = true;
					await this.#datagramsRouted(id);
				}
				const closed = hooks.trackReleased(producer).peek();
				if (closed instanceof Error) entry.datagrams.producer.abort(closed);
				else if (entry.end !== undefined) entry.datagrams.producer.finish();
				else entry.datagrams.producer.abort(new StreamError(StreamCode.Cancel));
				this.#datagrams.delete(id);
				if (entry.legacyKey && this.#legacyDatagrams.get(entry.legacyKey) === entry.datagrams)
					this.#legacyDatagrams.delete(entry.legacyKey);
			}

			this.#subscribes.delete(id);
		}
	}

	// Open the subscribe stream and accept the request (so the application's track.Subscriber
	// resolves and incoming groups have a producer to write into) with the track's immutable
	// properties. `state.stream` is populated as soon as it opens, and `state.track` once
	// answered, so the caller can clean them up once it gives up, even before this promise settles.
	//
	// On lite-05+ the properties come from a TRACK stream, held open until the SUBSCRIBE is
	// answered, and the SUBSCRIBE goes out alongside it rather than a round trip after: it is
	// accepted implicitly (no SUBSCRIBE_OK), and needs the info only to read what it receives.
	// A time anchor counts in the track's timescale, so a SUBSCRIBE naming one waits for the
	// info, as the Rust `pipeline` does. A group stream that beats the info waits on `#pending`,
	// unread. Older drafts carry no per-track properties, so they resolve to defaults and just
	// drain SUBSCRIBE_OK.
	async #openSubscribe(
		state: SubscribeSetup,
		msg: Subscribe,
		ranges: readonly track.Range[],
		request: track.Request,
		id: bigint,
		deadline: Deadline,
	): Promise<{ stream: Stream; entry: SubscribeEntry }> {
		state.cancel.signal.throwIfAborted();

		if (!hasTrackStream(this.version)) {
			// Older drafts negotiate nothing per-track: verbatim frames with no timeline.
			const framing = { compress: false, timescale: 0, scale: undefined };
			// Nor a max age: no publisher limit, not the authored default.
			const entry = this.#register(id, msg, ranges, request.accept({ maxAge: null }), framing, state);
			const stream = await this.#sendSubscribe(state, msg, deadline);
			// The first response MUST be a SUBSCRIBE_OK (older drafts only).
			const resp = await decodeSubscribeResponse(stream.reader, this.version);
			if (!("ok" in resp)) {
				throw new Error("first subscribe response must be SUBSCRIBE_OK");
			}
			return { stream, entry };
		}

		// The TRACK opens first, then the SUBSCRIBE, which is waiting on before it is written.
		// The answered TRACK is held at once, so a cancellation while the SUBSCRIBE still opens
		// closes it rather than waiting on an open that may never finish.
		const info = this.#trackInfo(msg.broadcast, msg, msg.track, state.cancel.signal, deadline).then((answered) => {
			state.track = answered.stream;
			// Cancelled as it was answered: the caller may already have left.
			if (state.cancel.signal.aborted) answered.stream.close();
			return answered;
		});
		let settle: (entry: SubscribeEntry | undefined) => void = () => {};
		this.#pending.set(
			id,
			new Promise((resolve) => {
				settle = resolve;
			}),
		);
		const sending = this.#fitted(this.#withRanges(msg, ranges)).then((msg) =>
			this.#sendSubscribe(state, msg, deadline),
		);
		try {
			// Either failing fails the subscription: a reset TRACK rejects the request, so the
			// origin can try another route, and the caller resets the SUBSCRIBE.
			const [answered, stream] = await Promise.all([info, sending]);
			// The setup ended as TRACK_INFO landed: the request is already rejected, so don't
			// accept it again.
			state.cancel.signal.throwIfAborted();
			const model = this.#toModelInfo(answered.info);
			const producer = request.accept(model);
			const framing = {
				compress: answered.info.compress,
				timescale: answered.info.timescale ?? 0,
				scale: model.timescale ?? undefined,
			};
			const entry = this.#register(id, msg, ranges, producer, framing, state);
			entry.unanswered = { held: answered.stream, answered: new AbortController() };
			settle(entry);
			return { stream, entry };
		} catch (err) {
			settle(undefined);
			// Whichever exchange outlived the other is no longer wanted.
			void info.then(
				({ stream }) => stream.abort(error(err)),
				() => {},
			);
			void sending.then(
				(stream) => stream.abort(error(err)),
				() => {},
			);
			throw err;
		} finally {
			this.#pending.delete(id);
		}
	}

	// `msg` asking for `ranges` on a wire that carries them, its time anchors counted in `scale`,
	// the timescale the track declares: refused on an untimed one.
	#withRanges(msg: Subscribe, ranges: readonly track.Range[]): Subscribe {
		if (hasRanges(this.version)) msg.ranges = toWireRanges(ranges);
		return msg;
	}

	// `msg` with its lite-07 ranges widened until they fit, as the Rust `pipeline` does: the union of
	// a relay's readers' ranges need not fit one message though each reader's does.
	async #fitted(msg: Subscribe): Promise<Subscribe> {
		if (!msg.ranges || !hasRanges(this.version)) return msg;
		const fields = await this.#subscribeFields(msg, msg);
		msg.ranges = [...Fit.fit(msg.ranges, [], [], Fit.budget(fields, 0n, msg.ranges.length)).ranges];
		return msg;
	}

	// The bytes a SUBSCRIBE like `msg` under `terms` takes besides its ranges, at the widest id,
	// measured with the smallest range, since it carries at least one. Mirrors the Rust
	// `subscribe_fields`.
	async #subscribeFields(
		msg: Subscribe,
		terms: { priority: number; order?: track.Order; maxDelay: number | null; minDelay?: number },
	): Promise<number> {
		const widest = new Subscribe({
			...msg,
			priority: terms.priority,
			order: terms.order,
			maxDelay: terms.maxDelay,
			minDelay: terms.minDelay,
			id: (1n << 64n) - 1n,
			ranges: [{}],
		});
		try {
			return (await Writer.collect(this.version, (w) => widest.encode(w, this.version))).byteLength;
		} catch {
			// One that can't go out leaves no room, and fails as it would have.
			return Number.POSITIVE_INFINITY;
		}
	}

	// Register a subscription's accepted copy, asking for `ranges`, so incoming groups and
	// datagrams find it.
	#register(
		id: bigint,
		msg: Subscribe,
		ranges: readonly track.Range[],
		producer: track.Producer,
		framing: { compress: boolean; timescale: number; scale: Time.Timescale | undefined },
		state: SubscribeSetup,
	): SubscribeEntry {
		hooks.receiveTrack(producer, {
			resolvesStart: resolvesStart(this.version),
			fillsFromStart: fillsFromStart(this.version),
		});
		// A new upstream subscription: stale runs an earlier one reported were judged under other terms.
		hooks.supersedeStale(producer);
		if (!hasRanges(this.version)) {
			hooks.dropAnchors(producer);
			// A wire without ranges starts a time anchor at the first group at or after it, so its
			// readers do too, rather than at an earlier group the copy happens to hold.
			hooks.startsAtKeys(producer);
		}
		// Lite-06+ resolves where the feed starts only with SUBSCRIBE_START, so a relay serving
		// this copy holds its first group until the answer arrives.
		if (resolvesStart(this.version)) hooks.startTrack(producer, { requested: msg.startGroup });
		const entry: SubscribeEntry = {
			track: producer,
			staleBudget: msg.maxDelay ?? NO_MAX_DELAY,
			...framing,
			// The effective max delay is the stopgap grace: the wrong clock (it bounds
			// presentation-time drift), but it is how long the subscriber was willing to wait
			// for a late group anyway, and no longer than the publisher keeps one fresh.
			tail: new Tail({ grace: () => hooks.tailGrace(producer) }),
			requested: msg.startGroup,
			floor: msg.startGroup,
			asked: [...ranges],
			live: new LiveRanges(this.version, msg.ranges ?? []),
			starts: new Map(),
			joins: new Map(),
			sent: 0,
			stream: { kind: "setup" },
			fillIds: new Set(),
			probeIds: new Set(),
			budgets: new Map(),
			fillGroups: [],
			fillsChanged: new Signal(0),
		};
		for (const id of entry.live.ids()) entry.budgets.set(id, entry.staleBudget);
		for (const [id, range] of entry.live.entries())
			if (range.start === undefined) entry.joins.set(id, msg.minDelay);
		if (hasDatagrams(this.version) && !hasDatagramTracks(this.version) && state.consumer && state.legacyKey) {
			const info = { priority: producer.priority, timescale: framing.scale };
			const pending = this.#legacyRequests.get(state.legacyKey);
			const request = pending?.closed.peek() === undefined ? pending : undefined;
			this.#legacyRequests.delete(state.legacyKey);
			const datagrams = request
				? request.accept(info)
				: wireOf(state.consumer).createDatagrams(producer.name, info, false);
			// Undefined while a live datagram track already holds the name: serve the stream track alone.
			if (datagrams) {
				entry.datagrams = { producer: datagrams, timescale: framing.scale };
				entry.legacyKey = state.legacyKey;
				this.#datagrams.set(id, entry.datagrams);
				this.#legacyDatagrams.set(state.legacyKey, entry.datagrams);
				let held: track.Subscriber | undefined;
				const sync = () => {
					const priority = datagrams.priority.peek();
					if (priority === undefined) {
						held?.close();
						held = undefined;
					} else if (held) {
						held.update({ priority });
					} else {
						held = producer.subscribe({ priority });
					}
				};
				const dispose = datagrams.priority.subscribe(sync);
				sync();
				entry.datagramDemand = () => {
					dispose();
					held?.close();
				};
			}
		}

		this.#subscribes.set(id, entry);
		state.producer = producer;
		return entry;
	}

	// Open the subscribe stream and write the SUBSCRIBE on it.
	async #sendSubscribe(state: SubscribeSetup, msg: Subscribe, deadline: Deadline): Promise<Stream> {
		// Giving up leaves the line, so the next open takes the stream this one was waiting for.
		const signal = state.cancel.signal;
		const opening = Stream.tryOpen(this.#quic, { version: this.version, signal }).then((opened) => {
			if (!opened) throw signal.reason;
			return opened;
		});
		const stream = await deadline.open(opening);
		state.stream = stream;
		// Setup was given up as the open handed over its stream: the late-setup handler resets
		// the stream, so don't send a SUBSCRIBE on it first.
		signal.throwIfAborted();
		await stream.writer.u53(StreamId.Subscribe);
		await msg.encode(stream.writer, this.version);
		return stream;
	}

	// Opens a TRACK stream and reads the single TRACK_INFO. Lite-05+ only. The stream
	// stays open, as interest in the track, until the caller closes it.
	async #trackInfo(
		broadcast: Path.Valid,
		{ instance }: Pin,
		track: string,
		signal: AbortSignal | undefined,
		deadline: Deadline,
	): Promise<{ info: TrackInfo; stream: Stream }> {
		return this.#exchange(
			{ version: this.version },
			async (stream) => {
				await stream.writer.u53(StreamId.Track);
				await new TrackMessage(broadcast, track, instance).encode(stream.writer, this.version);
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
		// Giving up leaves the line, so the next open takes the stream this one was waiting for.
		const opening = Stream.tryOpen(this.#quic, { ...options, signal: closed }).then((opened) => {
			if (!opened) throw closed.reason;
			return opened;
		});
		const stream = await deadline.open(opening);
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
			timescale: info.timescale ?? null,
			// Publisher Max Age rides on the wire, so the local media-time budget
			// matches what the upstream advertises (relays re-serve with the same bound).
			maxAge: info.maxAge === undefined ? null : Duration.millis(info.maxAge),
			priority: info.priority,
			refresh: {
				ratio: info.refresh ?? null,
				interval: info.refreshInterval === undefined ? null : Duration.millis(info.refreshInterval),
			},
			compression: info.compress ? "deflate" : "none",
			// Lite-05/06 timestamps may be send times, so the copy's groups skip the rewind check.
			...(hasMediaTime(this.version) ? {} : { unchecked: true }),
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
		pin: Pin = {},
		hold?: AbortSignal,
	): Promise<track.Info> {
		if (!hasTrackStream(this.version)) {
			throw new Error("track info requires moq-lite-05 or newer");
		}
		const deadline = new Deadline();
		const { info, stream } = await deadline.wait(this.#trackInfo(broadcast, pin, track, hold, deadline));
		if (hold && !hold.aborted) hold.addEventListener("abort", () => stream.close(), { once: true });
		else stream.close();
		return this.#toModelInfo(info);
	}

	// Open a FETCH stream for one group and stream its bare frames into a group, for the
	// ConsumeBroadcast backing track.Consumer.fetchGroup() (lite-05+).
	async fetchGroup(
		broadcast: Path.Valid,
		track: string,
		key: bigint,
		options: track.FetchGroupOptions = {},
		consumed: Consumed = {},
	): Promise<netGroup.Consumer> {
		const { instance } = consumed;
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
		// A minted instance is an object, which JSON prints as {}, so key on its text and flag.
		const pin = instance === undefined ? null : [Instance.text(instance), Instance.isMinted(instance)];
		const slot = JSON.stringify([consumed.id, broadcast, pin, track, String(key), options.maxDelay?.asNanos()]);
		let entry = this.#fetches.get(slot);
		let consumer: netGroup.Consumer;
		if (entry && entry.group.closed.peek() === undefined) {
			consumer = entry.group.mirror();
		} else {
			const group = new netGroup.Producer(key);
			consumer = group.mirror();
			entry = {
				group,
				accepted: this.#runFetch(
					broadcast,
					consumed,
					track,
					key,
					options.priority ?? 0,
					group,
					options.maxDelay,
				),
			};
			this.#fetches.set(slot, entry);
			void group.closed.then(() => {
				if (this.#fetches.get(slot)?.group === group) this.#fetches.delete(slot);
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
		slot: bigint,
		priority: number,
		group: netGroup.Producer,
		maxDelay?: Duration,
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

			// Lite has no FETCH_OK, and the publisher's application may hold its answer. Every
			// caller leaving, or Subscriber.close() closing the group, releases the setup at any
			// stage and resets the streams it opened.
			const deadline = new Deadline();
			const setup = deadline.wait(
				this.#fetchSetup(broadcast, consumed, track, slot, priority, group, deadline, maxDelay),
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

	// Open the FETCH stream alongside the TRACK stream that says the track's timescale, and wait
	// for both: the response waits unread in its stream until the timescale lands. Closing the
	// group during that wait resets the stream, and a failed TRACK_INFO fails the fetch.
	async #fetchSetup(
		broadcast: Path.Valid,
		consumed: Consumed,
		track: string,
		key: bigint,
		priority: number,
		group: netGroup.Producer,
		deadline: Deadline,
		maxDelay?: Duration,
	): Promise<{ stream: Stream; info: TrackInfo }> {
		const { instance, valid } = consumed;
		const cancel = new AbortController();
		void group.closed.then((err) => cancel.abort(err ?? new StreamError(StreamCode.Cancel)));
		const signal = valid ? AbortSignal.any([valid, cancel.signal]) : cancel.signal;
		const answered = this.#trackInfo(broadcast, consumed, track, signal, deadline);
		// Only the properties are needed, so the TRACK stream closes however the wait ends.
		answered.then(
			({ stream }) => stream.close(),
			(err: unknown) => cancel.abort(error(err)),
		);
		let stream: Stream;
		try {
			stream = await this.#exchange(
				{ sendOrder: sendOrder({ priority }), version: this.version },
				async (stream) => {
					await stream.writer.u53(StreamId.Fetch);
					await new FetchMessage({
						broadcast,
						instance,
						track,
						priority,
						maxDelay:
							hasFetchMaxDelay(this.version) && maxDelay !== undefined
								? Math.ceil(maxDelay.asMillis())
								: undefined,
						group: key,
					}).encode(stream.writer, this.version);
					// A byte or an empty-group FIN accepts the fetch; a reset rejects it.
					// done() buffers that byte so the response pump can decode it normally.
					await untilClosed(group, stream.reader.done());
					return stream;
				},
				signal,
				deadline,
			);
		} catch (err: unknown) {
			cancel.abort(error(err));
			throw err;
		}
		try {
			const { info } = await untilClosed(group, answered);
			return { stream, info };
		} catch (err: unknown) {
			stream.abort(error(err));
			throw err;
		}
	}

	// Read the FETCH response (bare frames, zigzag-delta-timestamped unless `timescale` is 0)
	// into the group, then FIN. A stream-level failure aborts the group so its reader observes
	// the gap.
	async #runFetchResponse(stream: Stream, group: netGroup.Producer, info: TrackInfo): Promise<void> {
		try {
			// One guard covers both header waits and a stalled payload. Abandonment aborts
			// the group before resetting the stream, releasing the shared frame pump too.
			const end = await untilAbandoned(
				group,
				readFrames(stream.reader, group, {
					scale: info.timescale ?? 0,
					compress: info.compress,
					lengthFirst: hasLengthFirst(this.version),
				}),
			);

			// Whole at a GROUP_END's end, else without one: every frame sent arrived.
			if (end === undefined) group.finish();
			else group.finishAt(end);
			stream.close();
		} catch (err: unknown) {
			const e = error(err);
			group.abort(e);
			stream.abort(e);
		}
	}

	// Serve the received copy's misses as its request handler, until the copy closes. A miss of more
	// than one group is asked through a subscription (see `RangeFills`): on lite-07 the copy's own,
	// and before it one of its own per run. Any other FETCHes its groups upstream one at a time,
	// newest first for a descending reader, as the Rust `FillServeRun` does.
	async #runFills(broadcast: Path.Valid, consumed: Consumed, entry: SubscribeEntry): Promise<void> {
		const producer = entry.track;
		const requests = producer.requests();
		const jobs = new Set<Promise<void>>();
		const fills = new RangeFills(producer, this.version, (groups, priority) =>
			this.#recover(broadcast, consumed, entry, groups, priority),
		);
		entry.fills = fills;
		const driving = this.#driveFills(entry, fills);
		try {
			// Accepted before its fills run, so this only reads what TRACK_INFO said.
			const maxAge = (await producer.info().catch(() => undefined))?.maxAge;
			const framing = { compress: entry.compress, timescale: entry.timescale, maxAge };
			for (let fill = await requests.requestedFill(); fill; fill = await requests.requestedFill()) {
				// One group is what FETCH carries; more go through a subscription.
				if (fill.end - fill.start > 1n) {
					fills.push(fill);
					continue;
				}
				const job = this.#serveFill(broadcast, consumed, producer, framing, fill);
				jobs.add(job);
				void job.then(() => jobs.delete(job));
			}
		} finally {
			requests.close();
			fills.close();
			await Promise.all([driving, ...jobs]);
		}
	}

	// Settle `fills` as their inputs change, until the copy closes, asking what they owe through
	// `entry`'s subscription. Mirrors the Rust serve loop's `RangeFills::poll`.
	async #driveFills(entry: SubscribeEntry, fills: RangeFills): Promise<void> {
		const demand = entry.track.demand();
		while (!fills.closed) {
			// A stream still arriving on a recovery is kept only for someone reading it.
			if (fills.poll(entry.tail, !isAbandoned(demand))) entry.fillsChanged.update((revision) => revision + 1);
			await fills.changed(entry.tail);
		}
		entry.fillsChanged.update((revision) => revision + 1);
	}

	// Serve `fill` until nobody waits on it, then close it, naming what is left dropped.
	async #serveFill(
		broadcast: Path.Valid,
		consumed: Consumed,
		producer: track.Producer,
		framing: Framing,
		fill: track.FillGroups,
	): Promise<void> {
		try {
			let last: bigint | undefined;
			for (let key = fill.wanted(); key !== undefined; key = fill.wanted()) {
				// Every fetch answers its group, so the same one coming back is a bug: closing the
				// fill names it dropped rather than fetching it forever.
				if (key === last) break;
				last = key;
				await this.#fillGroup(broadcast, consumed, producer.name, framing, fill, key);
			}
		} finally {
			fill.close();
		}
	}

	// Ask `asked` of `copy` on a SUBSCRIBE of their own with no max delay, as the Rust `Recovery`
	// does: on lite-07 `copy`'s subscription dropped them as stale, or delivered them before and they
	// were since evicted, so it never sends them again, and before lite-07 every run is asked this
	// way. Its groups land in the copy as a subscription's do. It FINs once every group is accounted
	// for and read, and is over at the publisher's FIN in reply, refusing whatever it did not deliver.
	#recover(
		broadcast: Path.Valid,
		consumed: Consumed,
		copy: SubscribeEntry,
		asked: Run,
		priority: number,
	): RecoveryHandle {
		const producer = copy.track;
		const id = this.#subscribeNext++;
		// Narrowed in place once the first group before lite-07 settles how far it reaches.
		const groups = { ...asked };
		// Before lite-07 the subscription proves only the keys below its start, so the range stays open
		// until the first group that arrives caps it: only upstream knows which group it still holds
		// past the run, and a relay's start may be its floor, which names no group.
		const oneStart = !hasGroupEnd(this.version);
		const ranges: track.Range[] = [span(groups.start, oneStart ? undefined : groups.end) ?? EMPTY];
		const msg = this.#withRanges(
			this.#subscribeMessage(
				id,
				broadcast,
				consumed,
				producer.name,
				{ priority, order: "ascending", delay: { max: null } },
				ranges,
			),
			ranges,
		);
		const entry: SubscribeEntry = {
			track: producer,
			staleBudget: msg.maxDelay ?? NO_MAX_DELAY,
			compress: copy.compress,
			timescale: copy.timescale,
			tail: new Tail({ grace: () => hooks.tailGrace(producer) }),
			requested: msg.startGroup,
			floor: msg.startGroup,
			asked: ranges,
			live: new LiveRanges(this.version, msg.ranges ?? []),
			starts: new Map(),
			joins: new Map(),
			sent: 0,
			// Its responses are read here, which owns this stream's end.
			stream: { kind: "setup" },
			filling: true,
			fillIds: new Set(),
			probeIds: new Set(),
			budgets: new Map(),
			fillGroups: [],
			fillsChanged: new Signal(0),
		};
		for (const id of entry.live.ids()) entry.budgets.set(id, entry.staleBudget);
		const left = new AbortController();
		const signal = consumed.valid ? AbortSignal.any([consumed.valid, left.signal]) : left.signal;
		const leaving = new Promise<void>((resolve) =>
			left.signal.addEventListener("abort", () => resolve(), { once: true }),
		);
		let refused: Refused[] = [];
		// Those the fills have yet to take: each refusal is handed over once.
		let fresh: Refused[] = [];
		const changed = new Signal(0);
		// What the publisher gives up of the groups, accounted for in the tail. The latest verdict on a
		// group replaces any earlier one, so a peer repeating its drops grows nothing.
		const giveUp = (start: bigint, end: bigint, refusal: Refusal) => {
			entry.tail.account(start, end);
			refused = refuseRun(refused, max(start, groups.start), min(end, groups.end), refusal);
			fresh = refuseRun(fresh, max(start, groups.start), min(end, groups.end), refusal);
			changed.update((revision) => revision + 1);
		};
		this.#subscribes.set(id, entry);
		const done = (async () => {
			let failed: Error = new StreamError(StreamCode.Cancel, { message: "recovery ended" });
			let stream: Stream | undefined;
			try {
				while ((await this.#auth?.ready(broadcast, "subscribe", { signal, until: () => leaving })) === false) {
					if (left.signal.aborted) return fresh;
				}
				if (this.#denied(broadcast)) throw unauthorized(broadcast);
				const opened = await this.#exchange(
					{ sendOrder: sendOrder({ priority }), version: this.version },
					async (stream) => {
						await stream.writer.u53(StreamId.Subscribe);
						await msg.encode(stream.writer, this.version);
						return stream;
					},
					signal,
					new Deadline(),
				);
				stream = opened;
				let fin = false;
				// The update capping the open range at the first group that arrives, which goes out before
				// our FIN.
				let capping: Promise<unknown> = Promise.resolve();
				let capped = !oneStart;
				let started = false;
				const cap = () => {
					capped = true;
					const update = new SubscribeUpdate({
						priority: msg.priority,
						order: msg.order,
						maxDelay: msg.maxDelay,
						minDelay: msg.minDelay,
						startGroup: msg.startGroup,
						endGroup: inclusiveGroupEnd(groups.end),
					});
					// A failed write means the publisher stopped reading: its responses decide the end.
					capping = Writer.collect(this.version, (w) => update.encode(w, this.version))
						.then((encoded) => opened.writer.write(encoded))
						.catch(() => undefined);
				};
				// Only a group that arrives proves one, so once started the range stops past the first;
				// what lies between is named by the publisher's drops, or given up with the grace.
				const capAtFirst = () => {
					const first = entry.tail.first;
					if (capped || !started || first === undefined || first < groups.start) return;
					if (first + 1n < groups.end) groups.end = first + 1n;
					cap();
				};
				const responses = this.#recoveryResponses(
					opened,
					entry,
					groups,
					(from) => {
						started = true;
						// Starting past the run answers all of it, so nothing more is asked.
						if (!capped && from >= groups.end) cap();
						capAtFirst();
					},
					giveUp,
					() => fin,
				);
				// FIN once every group is accounted for and read, as a subscriber does once its tail
				// settles, and the publisher FINs in reply.
				let ended = false;
				const over = responses.then(
					() => {
						ended = true;
					},
					() => {
						ended = true;
					},
				);
				// An excused stream is waited for at most the grace, as at a subscription's end: a browser
				// can lose its reset.
				const stopped = new Once<null>();
				const covered = () => entry.tail.covers(groups.start, groups.end);
				try {
					while (!ended && !left.signal.aborted) {
						capAtFirst();
						if (covered()) {
							await race([entry.tail.settle(covered, stopped), over, leaving]);
							if (ended || left.signal.aborted) break;
							// The cap goes out before our FIN; a cancel, which resets a write still blocked, or
							// the publisher's end stops the wait.
							await race([capping, over, leaving]);
							if (ended || left.signal.aborted) break;
							fin = true;
							opened.writer.close();
							break;
						}
						await race([Signal.race(entry.tail.changed), over, leaving]);
					}
				} finally {
					stopped.set(null);
				}
				// After our FIN, a cancel still stops the wait for the publisher's.
				if (!ended) await race([over, leaving]);
				// Cancelled: nothing more is read, and our own reset ends the reads above.
				if (!ended) {
					opened.abort(failed);
					await over;
				} else {
					await responses;
					opened.close();
				}
			} catch (err) {
				closeViolation(this.#quic, err);
				failed = error(err);
				// The publisher reset its side, or reading failed: give back our side too.
				stream?.abort(failed);
			} finally {
				left.abort();
				this.#subscribes.delete(id);
				entry.tail.close();
			}
			// What nothing accounted for is refused, as nothing more arrives on it.
			let unanswered: Run[] = [{ ...groups }];
			for (const run of [...entry.tail.accounted(groups.start, groups.end), ...refused])
				unanswered = removeRun(unanswered, run.start, run.end);
			for (const gap of unanswered) fresh.push({ ...gap, refusal: passing(failed) });
			// Only what was not handed over while it was out.
			return fresh;
		})();
		const takeRefused = () => {
			const taken = fresh;
			fresh = [];
			return taken;
		};
		return {
			groups: () => groups,
			done,
			takeRefused,
			changed,
			readingChanged: entry.tail.changed,
			reading: () => entry.tail.reading,
			cancel: () => left.abort(),
		};
	}

	// Read a recovery's responses to the publisher's FIN, giving up what it names it will not
	// deliver. `fin` says whether our FIN went out, which the publisher's answers. With `cap`,
	// before lite-07, `groups` narrows to the group it starts at, and `cap` asks no further.
	async #recoveryResponses(
		stream: Stream,
		entry: SubscribeEntry,
		groups: Run,
		onStart: (from: bigint) => void,
		giveUp: (start: bigint, end: bigint, refusal: Refusal) => void,
		fin: () => boolean,
	): Promise<void> {
		let started = false;
		let finished = false;
		for (;;) {
			const resp = await decodeSubscribeResponseMaybe(stream.reader, this.version);
			if (!resp) {
				// The publisher FINs in reply to ours, or once it finished the track.
				if (fin() || finished) return;
				throw new ProtocolViolation("recovery subscribe ended without SUBSCRIBE_FINISH");
			}
			if ("start" in resp) {
				// One start, for its one range.
				if (started || resp.start.starts.length !== 1)
					throw new ProtocolViolation("unexpected recovery SUBSCRIBE_OK");
				started = true;
				// Below its start the publisher, asked with no max delay, lacks them.
				const from = resp.start.starts[0] ?? groups.end;
				giveUp(
					groups.start,
					max(groups.start, min(from, groups.end)),
					passing(new NotFound(`group ${groups.start}`)),
				);
				onStart(from);
			} else if ("drop" in resp) {
				const { start, end } = resp.drop;
				const cause = resp.drop.cause(this.version);
				this.#dropped(entry, start, end);
				// Final, so a stream it overtook is told from one whose gap the grace gave up on.
				entry.tail.drop(start, end);
				giveUp(start, end, dropped(cause));
			} else if ("skip" in resp) {
				// Never produced: final, as the subscription feeding the copy records it.
				const { start, end } = resp.skip;
				entry.tail.account(start, end);
				hooks.skipTrackGroups(entry.track, start, end);
			} else if ("finish" in resp) {
				if (finished) throw new ProtocolViolation("duplicate SUBSCRIBE_FINISH");
				finished = true;
				// Nothing at or past the track's end will be produced.
				const end = max(resp.finish.group, groups.start);
				giveUp(end, groups.end, dropped(new NotFound(`group ${end}`)));
			} else {
				throw new ProtocolViolation("unexpected recovery subscribe response");
			}
		}
	}

	// The budget an OLD answer to a request carrying `asked` judged: the publisher's max age once
	// it reached it, which binds every reader, as the Rust `Inner::binding_max_age` judges. No
	// budget leaves only the max age; undefined when neither is known.
	#stale(framing: Framing, asked: number | undefined): Judged | undefined {
		const bound = framing.maxAge?.asMillis();
		if (asked !== undefined && (bound === undefined || asked < bound)) return { budget: asked, everyone: false };
		return bound === undefined ? undefined : { budget: bound, everyone: true };
	}

	// FETCH group `key` of `fill` and write it into the copy once the publisher answers: lite
	// has no FETCH_OK, so a publisher without the group resets the stream, which refuses the group
	// to the readers waiting now, as the Rust `GroupFill::reject` does. A fetch nobody waits on any
	// more is cancelled.
	async #fillGroup(
		broadcast: Path.Valid,
		consumed: Consumed,
		name: string,
		framing: Framing,
		fill: track.FillGroups,
		key: bigint,
	): Promise<void> {
		const cancel = new AbortController();
		void fill.unwanted(key).then(() => cancel.abort(new StreamError(StreamCode.Cancel, { message: "cancel" })));
		const stream = await this.#requestFill(broadcast, consumed, name, framing, fill, key, cancel.signal);
		if (!stream) {
			cancel.abort();
			return;
		}
		let group: netGroup.Producer | undefined;
		try {
			// The live feed may have brought it meanwhile, or every reader left.
			if (fill.isWanted(key)) group = fill.accept(key);
			if (!group) throw new StreamError(StreamCode.Cancel, { message: "cancel" });
		} catch (err) {
			stream.abort(error(err));
			return;
		} finally {
			cancel.abort();
		}
		try {
			// Once no reader holds the group or waits to pick it up, cancel upstream and abort the
			// truncated group rather than cache it whole, as the Rust fetch's ingest does.
			const end = await untilAbandoned(
				group,
				readFrames(stream.reader, group, {
					scale: framing.timescale,
					compress: framing.compress,
					lengthFirst: hasLengthFirst(this.version),
				}),
			);
			// Whole at a GROUP_END's end, else without one: every frame sent arrived.
			if (end === undefined) group.finish();
			else group.finishAt(end);
			stream.close();
		} catch (err: unknown) {
			const e = error(err);
			group.abort(e);
			stream.abort(e);
		}
	}

	// Open the FETCH for group `key` of `fill` once the session's grant admits it, and wait
	// for the publisher's answer. A refusal answers the readers waiting now, unless the fill's
	// budget widened since the FETCH carried it: a reader that joined meanwhile may reach what
	// the narrower one could not, so it asks again. An OLD one judged the budget the FETCH
	// carried, which binds only readers no more tolerant, unless it reached the publisher's max
	// age, as the Rust `Inner::reject` judges. Undefined once refused, or once nobody waits.
	async #requestFill(
		broadcast: Path.Valid,
		consumed: Consumed,
		name: string,
		framing: Framing,
		fill: track.FillGroups,
		key: bigint,
		cancel: AbortSignal,
	): Promise<Stream | undefined> {
		const signal = consumed.valid ? AbortSignal.any([consumed.valid, cancel]) : cancel;
		for (;;) {
			const priority = fill.priority;
			// Lite-07 FETCH carries the budget; older ones leave it to the publisher's max age.
			const budget = fill.maxDelay;
			// Rounded up, since a budget shortened by rounding skips a group a reader still wants.
			const maxDelay =
				hasFetchMaxDelay(this.version) && budget !== null ? Math.ceil(budget.asMillis()) : undefined;
			try {
				// Wait for the grant as a subscribe or fetch does; nobody waiting ends the wait.
				while (
					(await this.#auth?.ready(broadcast, "subscribe", {
						signal: consumed.valid,
						until: () => fill.unwanted(key),
					})) === false
				) {
					if (!fill.isWanted(key)) return undefined;
				}
				if (this.#denied(broadcast)) throw unauthorized(broadcast);
				const deadline = new Deadline();
				return await deadline.wait(
					this.#exchange(
						{ sendOrder: sendOrder({ priority }), version: this.version },
						async (stream) => {
							await stream.writer.u53(StreamId.Fetch);
							await new FetchMessage({
								broadcast,
								instance: consumed.instance,
								track: name,
								priority,
								maxDelay,
								group: key,
							}).encode(stream.writer, this.version);
							// A byte or an empty-group FIN accepts the fetch; a reset rejects it.
							await stream.reader.done();
							return stream;
						},
						signal,
						deadline,
					),
				);
			} catch (err) {
				if (cancel.aborted || !fill.isWanted(key)) return undefined;
				if (maxDelay !== undefined && (fill.maxDelay?.asMillis() ?? Number.POSITIVE_INFINITY) > maxDelay)
					continue;
				rejectGroups(fill, key, key + 1n, error(err), this.#stale(framing, maxDelay));
				return undefined;
			}
		}
	}

	// Start reading the subscribe stream's responses; see `runResponses`.
	#read(stream: Stream, entry: SubscribeEntry): Promise<void> {
		const responses = this.runResponses(stream, entry);
		entry.stream = { kind: "reading", responses: responses.catch(() => {}) };
		return responses;
	}

	// Reads the subscribe stream's responses until FIN, recording the range the tail is
	// accounted against: SUBSCRIBE_START/END/DROP on lite-05+, and any SUBSCRIBE_DROP after the
	// OK before then, where the FIN itself ends the track. Reading to the FIN, rather than
	// awaiting it, matters: a pull-driven transport stream closes only on a read.
	// SUBSCRIBE_FINISH declares the track's end right away, so a consumer learns it before the
	// last groups arrive. The publisher must declare the end before FIN unless we cancelled
	// locally; resets and malformed responses reject.
	private async runResponses(stream: Stream, entry: SubscribeEntry): Promise<void> {
		for (;;) {
			const resp = await decodeSubscribeResponseMaybe(stream.reader, this.version);
			// The publisher answered, so its demand stands on the subscription from here.
			if (entry.unanswered) {
				entry.unanswered.held.close();
				entry.unanswered.answered.abort();
				entry.unanswered = undefined;
			}
			if (!resp) {
				// Before lite-05 the FIN is the track's only end.
				if (!hasTrackStream(this.version)) {
					// Our side never FINs there, so the stream is still being read.
					if (entry.stream.kind === "reading")
						entry.stream = { kind: "finished", responses: entry.stream.responses };
					return;
				}
				if (entry.end === undefined && hooks.trackReleased(entry.track).peek() === undefined)
					throw new ProtocolViolation("subscribe stream ended without SUBSCRIBE_FINISH");
				return;
			}

			if ("ok" in resp) throw new ProtocolViolation("duplicate SUBSCRIBE_OK");
			if ("start" in resp) {
				if (hasRanges(this.version)) {
					if (entry.start !== undefined) throw new ProtocolViolation("duplicate SUBSCRIBE_OK");
					// One start per range the SUBSCRIBE carried, by Range ID.
					if (BigInt(resp.start.starts.length) !== entry.live.initial)
						throw new ProtocolViolation(
							`SUBSCRIBE_OK resolves ${resp.start.starts.length} ranges, not ${entry.live.initial}`,
						);
					// An update may have retired some already, whose starts say nothing now.
					resp.start.starts.forEach((start, index) => {
						if (entry.live.get(BigInt(index))) entry.starts.set(BigInt(index), start);
					});
				}
				// The lowest range start is the first group served; none leaves nothing owed.
				const start = resp.start.starts.reduce<bigint | undefined>(
					(lowest, start) =>
						start === undefined || (lowest !== undefined && lowest <= start) ? lowest : start,
					undefined,
				);
				entry.start = start ?? null;
				// A lite-07 tail waits for SUBSCRIBE_START to say where delivery starts.
				entry.tail.touch();
				// Lite-07 says where every live range starts, so the declaration follows them
				// whatever an update changed since.
				if (hasRanges(this.version)) this.#declare(entry);
				else hooks.startTrack(entry.track, { start: entry.start, requested: entry.requested });
				// Recorded whole, so a relay answers its own readers that the group ended.
				hooks.reportedLargest(entry.track, resp.start.largest?.position, resp.start.largest?.ended ?? false);
				// The groups the SUBSCRIBE asked for below it are not waited for, whatever the
				// demand asks later. One that still arrives is delivered.
				if (entry.requested !== undefined && start !== undefined) entry.tail.account(entry.requested, start);
			} else if ("finish" in resp) {
				if (entry.end !== undefined) throw new ProtocolViolation("duplicate SUBSCRIBE_FINISH");
				entry.end = resp.finish.group;
				// A local close can win the race with the response; there is nothing left to end.
				if (hooks.trackReleased(entry.track).peek() !== undefined) continue;
				try {
					// Lower groups may still be on the wire behind a higher one, so readers hold
					// at a hole below the end until the tail settles.
					hooks.endTrack(entry.track, { group: entry.end, frame: 0 }, resp.finish.cause());
				} catch (err) {
					// lite-05 specified an inclusive end, and @moq/net 0.1.3 to 0.1.9 sent one, so
					// there an end below a received group only costs the early boundary: the FIN
					// still finishes the track. Later drafts made it exclusive.
					if (this.version !== Version.DRAFT_05) {
						throw new ProtocolViolation(`invalid SUBSCRIBE_FINISH: ${reason(error(err))}`);
					}
					console.warn(`invalid SUBSCRIBE_FINISH: ${reason(error(err))}`);
				}
				// Lite-07 keeps the stream open past SUBSCRIBE_FINISH: FIN once the tail settles,
				// and the publisher's FIN in reply ends this loop.
				if (keepsSubscribeOpen(this.version)) void this.#finishTail(stream, entry).catch(() => {});
			} else if ("started" in resp) {
				// Where a range an update added starts, from which its groups are owed.
				const { id, start } = resp.started;
				if (
					entry.start === undefined ||
					!entry.live.assigned(id) ||
					id < entry.live.initial ||
					entry.starts.has(id)
				)
					throw new ProtocolViolation(`unexpected SUBSCRIBE_STARTED: ${id}`);
				// One retired since is owed nothing.
				const range = entry.live.get(id);
				if (range) {
					entry.starts.set(id, start);
					// A fill's range starting past where it asked skips groups it will not deliver.
					const asked = entry.fillIds.has(id) ? fillRun(range) : undefined;
					if (asked) {
						const skipped = max(asked.start, min(start ?? asked.end, asked.end));
						entry.tail.account(asked.start, skipped);
						entry.fills?.skip(asked, asked.start, skipped);
					}
					// The tail judges what is owed again.
					entry.tail.touch();
					this.#declare(entry);
				}
			} else if ("drop" in resp) {
				// The publisher will never send these groups on this subscription, so they are accounted
				// for without a stream, and a reader of one still arriving ends with the drop's error now.
				const { start, end } = resp.drop;
				const error = resp.drop.cause(this.version);
				// Lite-07 tells a stream a drop overtook from one whose gap the grace gave up on.
				if (hasSkip(this.version)) entry.tail.drop(start, end);
				else entry.tail.account(start, end);
				this.#dropped(entry, start, end);
				// On lite-07 an OLD drop is stale, recorded under the narrowest budget it may have been
				// judged under: a reader wider than that does not settle on it, and asks for the group
				// with a request of its own.
				const stale = hasSkip(this.version) && toStreamCode(error) === StreamCode.Old;
				const hole = stale ? staleHole(entry.staleBudget) : { error };
				hooks.dropTrackGroups(entry.track, start, end, hole);
				entry.fills?.refuse(start, end, error);
			} else if ("skip" in resp) {
				const { start, end } = resp.skip;
				this.#skipped(entry, start, end);
			}
		}
	}

	// The publisher never produced `[start, end)`, as a SUBSCRIBE_SKIP says, so nothing is waited
	// for and no reader ends. A skip replaces any drop of the same groups; a reader a drop already
	// ended stays ended.
	#skipped(entry: SubscribeEntry, start: bigint, end: bigint): void {
		entry.tail.account(start, end);
		// A fill owing them is answered: the source never had them.
		entry.fills?.refuse(start, end, new NotFound(`group ${start}`));
		// A group that arrived was produced after all, so the skip contradicts it: it keeps the
		// group, and the session goes on, since the draft requires no more.
		const [cached] = hooks.skipTrackGroups(entry.track, start, end);
		if (cached) {
			console.warn(
				`protocol error: SUBSCRIBE_SKIP skips [${start}, ${end}), which holds group ${cached.start}, which arrived; keeping it`,
			);
		}
	}

	// Settle a lite-07 tail past SUBSCRIBE_FINISH, then FIN. An update sent meanwhile asks for more,
	// which waits a fresh grace rather than one that may have passed while a stream was read.
	async #finishTail(stream: Stream, entry: SubscribeEntry): Promise<void> {
		for (;;) {
			const sent = entry.sent;
			await this.#settleTail(entry);
			if (entry.sent !== sent) continue;
			// Fills still ask through this subscription, which a FIN would leave no way to update.
			if (!entry.fills || entry.fills.isEmpty() || hooks.trackReleased(entry.track).peek() !== undefined) break;
			await race([Signal.race(entry.fillsChanged), hooks.trackReleased(entry.track)]);
		}
		this.#fin(stream, entry);
	}

	// FIN our side once, acknowledging a settled tail without cancelling the publisher's half.
	#fin(stream: Stream, entry: SubscribeEntry): void {
		if (entry.stream.kind !== "reading") return;
		entry.stream = { kind: "fin", responses: entry.stream.responses };
		stream.writer.close();
	}

	// Wait for the group streams the publisher still owes once it has ended the subscription.
	//
	// Received headers and SUBSCRIBE_DROP account for the range. An unaccounted
	// key, such as a lost datagram, still waits out the grace. Streams whose
	// headers arrived keep reading until their own FIN or reset.
	async #settleTail(entry: SubscribeEntry): Promise<void> {
		const { tail, track } = entry;
		await tail.settle(() => this.#covered(entry), hooks.trackReleased(track));
	}

	// Whether the groups received and dropped account for every group the demand is owed once
	// the publisher ended the subscription.
	#covered(entry: SubscribeEntry): boolean {
		// Without SUBSCRIBE_FINISH (older drafts) nothing says which groups are owed.
		if (entry.end === undefined) return false;
		const ranges = wireRanges(hooks.requested(entry.track).peek()?.ranges ?? [{}]);
		const end = min(entry.end, wireSpan(ranges).end ?? entry.end);
		const floor = rangesStart(ranges);
		// Lite-07 answers with SUBSCRIBE_OK once the track ends, unless nothing was asked below the
		// end. Each range the publisher holds is then owed from the start SUBSCRIBE_OK or
		// SUBSCRIBE_STARTED named, which accounts for a skipped prefix and resolves a live join or a
		// time start; one whose SUBSCRIBE_STARTED is still on its way owes what nothing says yet.
		if (hasRanges(this.version)) {
			if (entry.start === undefined) return !(end > (floor ?? 0n));
			const owed: track.Range[] = [];
			for (const [id, range] of entry.live.entries()) {
				if (!entry.starts.has(id)) return false;
				const at = entry.starts.get(id);
				if (at === undefined) continue;
				const model = requestedRange(range);
				owed.push({ ...model, start: at });
			}
			// Each starts at the group its publisher reported starting it at.
			const window = new Window(owed, entry.track.largest()?.group, AT_KEYS);
			return window.runs(0n, end).every((run) => entry.tail.covers(run.start, run.end));
		}
		// Without SUBSCRIBE_START the publisher served no group at all. Otherwise each group is owed
		// from the floor the demand last asked for, which an update can move either way, or where
		// SUBSCRIBE_START resolved a live-edge one. The groups the SUBSCRIBE asked for below its
		// SUBSCRIBE_START were accounted for when it arrived.
		if (entry.start === undefined || entry.start === null) return true;
		const window = new Window(ranges, entry.track.largest()?.group);
		return window.runs(floor ?? entry.start, end).every((run) => entry.tail.covers(run.start, run.end));
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
			delay: {
				max: msg.maxDelay === null ? null : Duration.millis(msg.maxDelay),
				min: msg.minDelay === undefined ? null : Duration.millis(msg.minDelay),
			},
			ranges: entry.asked,
		};

		// The wanted group is subscription state: an update announces each new one past our
		// largest, and every later update restates it. Requests asking for the same group
		// coalesce into that one update.
		let sentWanted: bigint | undefined;
		// What the fills asked with the last update, and the priority it carried.
		let sentFills: Run[] = [];
		let sentPriority = msg.priority;
		// The time starts the readers waited on as the last update went out.
		let sentAsks: (readonly [bigint, number])[] = [];
		const asked = hooks.demandAsked(track.demand());
		const edge = hooks.producerLargest(track);
		for (;;) {
			const largest = edge.peek()?.group;
			const wanted = asked.peek();
			const refresh =
				hasRefresh(this.version) &&
				largest !== undefined &&
				wanted !== undefined &&
				wanted > largest &&
				wanted !== sentWanted
					? wanted
					: undefined;
			const current = hooks.requested(track).peek();
			// Lite-07 asks upstream to place each time start the readers wait on.
			const asks = hasRanges(this.version) ? hooks.requested(track).asks() : [];
			// Lite-07 asks the held fills' groups through the same subscription, under the demand's
			// terms but the more urgent priority.
			const fills = entry.fills?.ask;
			const fillGroups = fills?.groups ?? [];
			const priority = Math.max(current?.priority ?? 0, fills?.priority ?? -128);
			if (
				current === undefined ||
				(refresh === undefined &&
					priority === sentPriority &&
					sameRuns(fillGroups, sentFills) &&
					sameAsks(asks, sentAsks) &&
					this.#sameSubscription(current, lastSent))
			) {
				// Each wake releases the quiet signals too, including when termination wins.
				const next = await race([
					Signal.race(hooks.requested(track), asked, edge, entry.fillsChanged),
					stopped,
				]);
				if (next === null) return;
				continue;
			}

			// Demand collapsing to nothing is refused the same way an initial empty
			// request is: the error closes the track, so every local subscriber sees it.
			const ranges = wireRanges(current.ranges ?? [{}]);
			if (ranges.length === 0) throw new Error(EMPTY_RANGE);
			const span = wireSpan(ranges);

			// Changed ranges owe groups nobody asked for until now: below a lowered floor, or
			// between ranges a widening joins while keeping the floor. Each gap they reach waits
			// a fresh grace, which only delays giving up on one still owed.
			const start = rangesStart(ranges);
			if (!sameRanges(ranges, wireRanges(lastSent.ranges ?? [{}])))
				entry.tail.demand(start ?? entry.start ?? 0n, UNBOUNDED);

			// Lite-07 edits the live ranges: a range already live keeps its Range ID, and where it
			// started. The retired ones owe nothing more.
			let edit: { remove: bigint[]; add: WireRange[] } | undefined;
			if (hasRanges(this.version)) {
				// The demand's ranges and the fills' groups, widened together until they fit. The fills
				// keep their exact ask, so what they ask next is compared with that, not the wire.
				const demanded = toWireRanges(ranges);
				const fields = await this.#subscribeFields(msg, {
					priority,
					order: current.order,
					maxDelay: wireMaxDelay(current),
					minDelay: minDelayOf(current),
				});
				const budget = Fit.budget(fields, entry.live.next(), demanded.length + asks.length + fillGroups.length);
				const fitted = Fit.fit(demanded, fillGroups, entry.fills?.avoid() ?? [], budget);
				// A fill's range starting past its run speaks only while that run is asked whole.
				entry.fills?.retireSkipped(fitted.fills);
				const fillRanges: WireRange[] = fitted.fills.map((run) => ({
					start: { absolute: run.start },
					end: { absolute: run.end },
				}));
				// A live join resolves once, so one whose Min Age grew is retired and added again, which
				// the publisher places afresh under the new one: how a reader joining further back asks.
				const age = minDelayOf(current);
				const renew = new Set<bigint>();
				for (const [id, added] of entry.joins)
					if (added !== undefined && (age === undefined || age > added)) renew.add(id);
				// The probes the readers' asks need, as many as fit beside the rest: those left out go
				// once an answer lets the readers waiting on others go.
				const asked = maxDelayOf(current);
				const wanted = probes(entry, asks, fitted.ranges, asked);
				const probed = Fit.fitAfter(budget, [...fitted.ranges, ...fillRanges], wanted.probes);
				const kept = new Set(probed.map(rangeKey));
				for (const id of wanted.reasked) {
					const range = entry.live.get(id);
					if (range && kept.has(rangeKey(range))) renew.add(id);
				}
				const first = entry.live.next();
				// The demand's ranges, then the probes, then the fills'.
				const kindOf = (id: bigint): RangeKind =>
					entry.fillIds.has(id) ? "fill" : entry.probeIds.has(id) ? "probe" : "demand";
				const edited = entry.live.edit(
					[...fitted.ranges, ...probed, ...fillRanges],
					{ probes: probed.length, fills: fillRanges.length, kindOf },
					renew,
				);
				edit = edited;
				for (const id of entry.starts.keys()) if (!entry.live.get(id)) entry.starts.delete(id);
				for (const id of entry.joins.keys()) if (!entry.live.get(id)) entry.joins.delete(id);
				for (const id of entry.budgets.keys()) if (!entry.live.get(id)) entry.budgets.delete(id);
				// A range still unanswered may be resolved under this budget as well.
				for (const [id, held] of entry.budgets)
					if (!entry.starts.has(id)) entry.budgets.set(id, Math.min(held, asked));
				for (let id = first; id < entry.live.next(); id++) entry.budgets.set(id, asked);
				const fillIds = new Set(edited.fillIds);
				edited.add.forEach((range, index) => {
					const id = first + BigInt(index);
					if (range.start === undefined && !fillIds.has(id)) entry.joins.set(id, age);
				});
				for (const id of entry.fillIds) if (!entry.live.get(id)) entry.fillIds.delete(id);
				for (const id of edited.fillIds) entry.fillIds.add(id);
				for (const id of entry.probeIds) if (!entry.live.get(id)) entry.probeIds.delete(id);
				for (const id of edited.probeIds) entry.probeIds.add(id);
				// The groups a fill newly asks for wait a fresh grace too.
				for (const run of fitted.fills)
					for (const gap of runGaps(entry.fillGroups, run.start, run.end))
						entry.tail.demand(gap.start, gap.end);
				entry.fillGroups = fitted.fills;
				entry.floor = span.start;
				// Pending while a range the update adds awaits SUBSCRIBE_STARTED.
				this.#declare(entry);
			}
			// Round-trip the other Subscribe parameters so the publisher doesn't
			// interpret SUBSCRIBE_UPDATE as a reset of ordered/delay/etc.
			const update = new SubscribeUpdate({
				wantedGroup: refresh ?? sentWanted,
				priority,
				order: current.order,
				maxDelay: wireMaxDelay(current),
				minDelay: minDelayOf(current),
				startGroup: span.start,
				endGroup: inclusiveGroupEnd(span.end),
				ranges: edit?.add,
				remove: edit?.remove,
			});
			entry.staleBudget = Math.min(entry.staleBudget, update.maxDelay ?? NO_MAX_DELAY);
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
			entry.sent += 1;
			// A tail settling past SUBSCRIBE_FINISH re-judges what the update now asks for.
			entry.tail.touch();
			lastSent = { ...current };
			sentFills = fillGroups;
			sentPriority = priority;
			sentAsks = asks;
			console.debug(`subscribe update: id=${id} broadcast=${broadcast} track=${track.name}`);
		}
	}

	// Declare where a lite-07 feed starts: the lowest start the live ranges report, or the
	// requested floor, pending, while SUBSCRIBE_START or a range's SUBSCRIBE_STARTED has yet to
	// say where one starts. Mirrors the Rust `SubStream::declare`.
	#declare(entry: SubscribeEntry): void {
		// Where the publisher started each live range asked from a group's start, or from the oldest
		// group (time 0) when it has no start under an unstated Min Age, with the budget it was
		// answered under, which a reader's time start there resolves to when the copy can't place it
		// among its groups. Mirrors the Rust `SubStream::report_starts`.
		const times: [bigint, bigint | undefined, number][] = [];
		// And each live join, by the Min Age it was asked under, which places a reader's live join the
		// copy can't prove from the groups it holds.
		const joins: [number, bigint, boolean][] = [];
		for (const id of entry.live.ids()) {
			const range = entry.live.get(id);
			if (range === undefined || !entry.starts.has(id)) continue;
			const time = range.start?.time ?? range.start?.absolute;
			// Undefined for a range with no group to deliver.
			const at = entry.starts.get(id);
			const age = entry.joins.get(id);
			if (time === undefined && age !== undefined) {
				// A join answered empty starts past its range's end: the group containing the live edge
				// minus its Min Age is a later one, and the edge only moves on.
				const last = through(range.end, 0n);
				if (at !== undefined) joins.push([age, at, false]);
				else if (last !== undefined) joins.push([age, last + 1n, true]);
				continue;
			}
			const asked = time ?? 0n;
			// One with no group to deliver says nothing of its time unless it reaches it.
			if (at !== undefined || reaches(range.end, asked)) times.push([asked, at, entry.budgets.get(id) ?? 0]);
		}
		hooks.timeStarts(entry.track, times);
		hooks.joinStarts(entry.track, joins);

		let start: bigint | null = null;
		let pending = entry.start === undefined;

		for (const id of entry.live.ids()) {
			// Only the demand's ranges say where the live feed starts, not a fill's or a probe.
			if (entry.fillIds.has(id) || entry.probeIds.has(id)) continue;
			if (!entry.starts.has(id)) {
				pending = true;
				break;
			}
			const at = entry.starts.get(id);
			// A range with no group there serves nothing to start from.
			if (at !== undefined && (start === null || at < start)) start = at;
		}
		const next = { start: pending ? undefined : start, requested: entry.floor };
		const last = entry.declared;
		if (last && last.start === next.start && last.requested === next.requested) return;
		entry.declared = next;
		hooks.startTrack(entry.track, next);
	}

	#sameSubscription(a: track.Subscription, b: track.Subscription): boolean {
		const ar = wireRanges(a.ranges ?? [{}]);
		const br = wireRanges(b.ranges ?? [{}]);
		let rangesEqual: boolean;
		if (hasRanges(this.version)) rangesEqual = sameRanges(ar, br);
		else {
			const as = wireSpan(ar);
			const bs = wireSpan(br);
			// A lite-06 `Group Start` of 0 is no floor, so the two spell the same; a pre-06 wire tells
			// them apart: omitted joins at the publisher's start, and 0 is group 0.
			const start = resolvesStart(this.version)
				? (span: typeof as) => span.start ?? 0n
				: (span: typeof as) => span.start;
			rangesEqual = start(as) === start(bs) && as.end === bs.end;
		}
		// A wire without `Group Order` cannot carry a change to it.
		const sameOrder = !hasOrder(this.version) || (a.order ?? "descending") === (b.order ?? "descending");
		return (
			(a.priority ?? 0) === (b.priority ?? 0) &&
			sameOrder &&
			maxDelayOf(a) === maxDelayOf(b) &&
			// A wire without `Subscriber Min Age` cannot carry a change to it.
			(!hasMinDelay(this.version) || minDelayOf(a) === minDelayOf(b)) &&
			rangesEqual
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
		// One that beats its subscription's TRACK_INFO waits for it unread, which flow control
		// bounds. Waiters resume in the order they arrived.
		const entry = this.#subscribes.get(group.subscribe) ?? (await this.#pending.get(group.subscribe));
		if (!entry) {
			if (group.subscribe >= this.#subscribeNext) {
				throw new Error(`unknown subscription: id=${group.subscribe}`);
			}

			return;
		}

		const { track, timescale, tail } = entry;
		// Asked before this stream accounts for the group: a drop that overtook it named it.
		let owed = !tail.covers(group.key, group.key + 1n);
		// A drop that overtook this stream, rather than a gap the grace gave up on.
		const dropped = !owed && tail.dropped(group.key);
		let producer: netGroup.Producer | undefined;
		let arriving: Arriving | undefined;
		// Lite-07: this stream's entry in `SubscribeEntry.reading`.
		let reading = false;
		const counted = tail.open(group.key);

		try {
			// The publisher contradicted its own end, which no later group can repair. lite-05
			// specified an inclusive end, so its last group lands on it: the write below drops
			// only that group there.
			if (entry.end !== undefined && group.key >= entry.end && this.version !== Version.DRAFT_05) {
				const violation = new ProtocolViolation(
					`group ${group.key} is at or past the declared end ${entry.end}`,
				);
				track.abort(violation);
				throw violation;
			}
			// Registered before any duplicate wait below, so a drop that lands during it finds the read.
			if (hasSkip(this.version)) {
				entry.reading ??= new Map();
				entry.reading.set(group.key, counted);
				reading = true;
			}
			// A duplicate of a copy still arriving, such as one another subscription sharing the copy
			// delivers, waits unread, which flow control bounds. Created once that copy ends, it is
			// refused as a duplicate if the copy finished and takes the group if it failed,
			// continuing the reads of that copy, which wait while it is held. Its own reset ends the
			// wait; a clean end leaves it on the copy alone.
			for (;;) {
				// Kept held past a failure until this copy takes the group, below.
				const next = hooks.arrivingGroup(track, group.key);
				if (next === undefined) break;
				arriving?.release();
				arriving = next;
				// Served again once that copy fails, so owed again.
				owed = true;
				const settled = arriving.settled;
				await race([settled, stream.closed.then(() => settled)]);
			}
			producer ??= hooks.receiveGroup(track, {
				start: group.key,
				offset: group.frameStart,
				fill: entry.filling,
			});
			// Released once this copy took the group, so the failed copy's readers find it.
			arriving?.release();
			// A drop that overtook the header already accounts for the group; a copy served again is
			// owed whatever dropped the earlier one.
			if (reading && !owed && dropped) counted.excuse();

			const format = { scale: timescale, compress: entry.compress, lengthFirst: hasLengthFirst(this.version) };
			const frames = readFrames(stream, producer, format);
			const end = await (entry.filling ? untilAbandoned(producer, frames) : frames);

			// Whole at a GROUP_END's end, else without one: every frame sent arrived.
			if (end === undefined) producer.finish();
			else {
				producer.finishAt(end);
				// No other group starts inside the span, which the tail counts as it counts the
				// group, and the track records skipped, as its own publisher's append did.
				if (end > group.key + 1n) {
					tail.account(group.key + 1n, end);
					hooks.skipTrackGroups(track, group.key + 1n, end);
				}
			}
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
			arriving?.release();
			if (reading && entry.reading?.get(group.key) === counted) entry.reading.delete(group.key);
			counted.done();
		}
	}

	// A SUBSCRIBE_DROP named `[start, end)`: a stream still reading a group it names holds the
	// subscription's end at most the grace, since a browser can lose that stream's reset.
	#dropped(entry: SubscribeEntry, start: bigint, end: bigint): void {
		for (const [key, counted] of entry.reading ?? []) {
			if (key >= start && key < end) counted.excuse();
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
					const yielded = datagramBudget.poll();
					if (yielded !== undefined) {
						this.#receiveYield = yielded;
						await yielded;
						this.#receiveYield = undefined;
					}
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

	// Decode one datagram body and hand it to the datagram track it was sent for. A publisher
	// sends a track's first datagram right behind its answer, which the transport may hand over
	// first: one for a subscription still awaiting its answer is held a host task, so an answer
	// already received decodes before it, while other tracks' datagrams go on. One that still
	// finds no track is dropped and counted.
	async #routeDatagram(payload: Uint8Array): Promise<void> {
		if (payload.byteLength > DatagramMessage.MAX_SIZE) return;
		let unrouted = await this.#deliverDatagram(payload, true);
		if (unrouted === undefined) return;
		// Its track may have registered while the decode awaited.
		if (!this.#isHeld(unrouted) && this.#datagrams.has(unrouted))
			unrouted = await this.#deliverDatagram(payload, true);
		if (unrouted === undefined) return;
		const awaiting = this.#isHeld(unrouted) || this.#unanswered.has(unrouted) || this.#pending.has(unrouted);
		if (awaiting && this.#held.length < HELD_DATAGRAMS) {
			this.#held.push({ subscribe: unrouted, payload });
			this.#heldRouted ??= this.#routeHeld();
			return;
		}
		this.#dropUnrouted(unrouted);
	}

	// Wait for the datagrams received for `subscribe` so far to route: a host task, then until the
	// receive loop reads the transport's queue empty rather than yielding to its budget, and those
	// held for its answer. A peer keeping the queue full delays the end, holding nothing more.
	async #datagramsRouted(subscribe: bigint): Promise<void> {
		await yieldNow();
		for (;;) {
			const receiveYield = this.#receiveYield;
			if (receiveYield !== undefined) {
				await receiveYield;
				// The loop reads on in microtasks, until the queue empties or its next yield.
				await yieldNow();
			} else if (this.#isHeld(subscribe)) {
				// A held datagram keeps its routing pass running.
				await this.#heldRouted;
			} else return;
		}
	}

	// Whether a datagram for `subscribe` is held, which later ones wait behind to keep its order.
	#isHeld(subscribe: bigint): boolean {
		return this.#held.some((held) => held.subscribe === subscribe);
	}

	// A host task later, route the datagrams held until now, then any held meanwhile.
	async #routeHeld(): Promise<void> {
		await yieldNow();
		for (let count = this.#held.length; count > 0; count--) {
			const [{ payload }] = this.#held;
			try {
				const unrouted = await this.#deliverDatagram(payload, false);
				if (unrouted !== undefined) this.#dropUnrouted(unrouted);
			} catch (err: unknown) {
				console.debug(`dropping datagram: ${reason(err)}`);
			}
			this.#held.shift();
		}
		this.#heldRouted = this.#held.length > 0 ? this.#routeHeld() : undefined;
	}

	#dropUnrouted(subscribe: bigint): void {
		this.datagramsUnrouted++;
		console.debug(`dropping datagram for a subscription with no datagram track: id=${subscribe}`);
	}

	// Deliver one datagram body, returning its Subscribe ID if that subscription has no datagram
	// track, or, `behindHeld`, has a datagram held.
	async #deliverDatagram(payload: Uint8Array, behindHeld: boolean): Promise<bigint | undefined> {
		const dg = await DatagramMessage.decode(payload, this.version);
		const entry = this.#datagrams.get(dg.subscribe);
		if (!entry) return dg.subscribe;
		if (behindHeld && this.#isHeld(dg.subscribe)) return dg.subscribe;
		// A scale-less datagram's timestamp is bare, which the track checks is its key.
		const timestamp =
			entry.timescale === undefined
				? Time.Timestamp.unscaled(dg.timestamp)
				: new Time.Timestamp(toNumber(dg.timestamp), entry.timescale);
		entry.producer.insert({ sequence: dg.sequence, timestamp, payload: dg.payload });
		return undefined;
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

	async runProbe(signal?: AbortSignal, target?: Getter<number>): Promise<void> {
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
			// Only a peer that probes up to a target is sent one; a Report-level peer ignores it.
			let increase = false;
			if (hasSetupStream(this.version) && this.#peerSetup) {
				const level = await this.#peerProbeLevel(this.#peerSetup, scope);
				if (level === undefined || level < ProbeLevel.Report) return;
				increase = level >= ProbeLevel.Increase;
			}
			if (this.#goingAway() || abort.aborted) return;
			const stream = await Stream.tryOpen(this.#quic, { version: this.version, signal: abort });
			if (!stream) return;
			if (abort.aborted) {
				// Handed a stream as it gave up: give the slot back unused.
				stream.close();
				return;
			}
			scope.cleanup(() => stream.close());
			await scope.race(stream.writer.u53(StreamId.Probe));
			if (target && increase) this.#sendTarget(stream, target, scope);
			for (;;) {
				const probe = await scope.race(Probe.decodeMaybe(stream.reader, this.version));
				if (!probe) break;
				// Lite-03 carries no RTT; newer drafts retract it with a zero.
				const prev = this.#probe.peek();
				const rtt = probe.rtt !== undefined ? Duration.millis(probe.rtt) : undefined;
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

	// Send `target` on the PROBE stream whenever it changes, in order, while `scope` lives.
	// Nothing is sent while it is zero, until it retracts a target already sent. One write is
	// in flight at a time, and the next sends the latest target, so a peer that stops reading
	// holds one target however often it changes.
	#sendTarget(stream: Stream, target: Getter<number>, scope: Effect): void {
		let sent = 0;
		let writing = false;
		const write = async () => {
			writing = true;
			try {
				for (;;) {
					// The wire carries whole bits per second.
					const next = Math.floor(target.peek());
					if (next === sent) return;
					sent = next;
					// Zero reads as unknown on the wire, which retracts the last target.
					await new Probe({ bitrate: next > 0 ? next : undefined }).encode(stream.writer, this.version);
				}
			} catch {
				// The writer and reader are independent, so a failed write does not end the
				// reads: end the probe here, which also closes the stream.
				scope.close();
			} finally {
				writing = false;
			}
		};
		scope.run((effect) => {
			effect.get(target);
			if (!writing) void write();
		});
		// A close queues behind a write the peer is not reading, which would hold the stream
		// open past the probe; a reset rejects that write and frees the stream.
		scope.cleanup(() => {
			if (writing) stream.abort(new StreamError(StreamCode.Cancel, { message: "probe ended" }));
		});
	}

	/**
	 * Wait until every subscribe stream's responses are read to its FIN or reset. Bounded only
	 * once the transport is closed, so that its reads end.
	 *
	 * @internal
	 */
	async drain(): Promise<void> {
		await Promise.all(
			Array.from(this.#subscribes.values(), (entry) => entry.stream.kind !== "setup" && entry.stream.responses),
		);
	}

	/**
	 * Ends every subscribed track, fetch, and setup exchange with `err`, the session's end, which
	 * defaults to a clean close: they were cut off rather than ended. A track whose publisher
	 * already ended it finishes instead, datagram tracks included.
	 */
	close(err: Error = new SessionError(SessionCode.Cancel)) {
		this.#closed.abort(err);
		this.#routed?.close(err);

		for (const { track, stream, end, datagrams } of this.#subscribes.values()) {
			if (stream.kind === "finished") track.finish();
			track.abort(err);
			// Lite-05/06: the stream track's end is its datagrams' end. An abort after the declared
			// end settled ends it cleanly, even while the FIN that would finalize it is in flight.
			if (datagrams && end !== undefined && hooks.trackReleased(track).peek() === null) datagrams.ended = true;
		}

		this.#subscribes.clear();
		for (const { producer, ended } of this.#datagrams.values()) {
			if (ended) producer.finish();
			producer.abort(err);
		}
		this.#datagrams.clear();
		this.#legacyDatagrams.clear();

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
				if (!consumed.valid) return subscriber.resolveTrackInfo(path, name, consumed, hold);
				// Retirement cancels setup, but a one-shot query still releases its TRACK
				// as soon as it answers instead of holding it for the broadcast's lifetime.
				const query = new AbortController();
				const signal = AbortSignal.any([consumed.valid, hold ?? query.signal]);
				try {
					return await subscriber.resolveTrackInfo(path, name, consumed, signal);
				} finally {
					if (!hold) query.abort();
				}
			},
			// FETCH streams start at lite-05; older sessions offer no fetch at all.
			fetchGroup: hasTrackStream(subscriber.version)
				? (name, key, options) => subscriber.fetchGroup(path, name, key, options, consumed)
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

// The probes to ask for `asks`, each a time start the copy's readers wait on with the widest budget
// one waits under in milliseconds, beside the demand's `ranges` going out under `budget`, and the
// live probes to ask afresh: one answered under a narrower budget than its readers'. A demand range
// from the same time, asked under a budget as wide, answers it already. Mirrors the Rust `probes`.
function probes(
	entry: SubscribeEntry,
	asks: readonly (readonly [bigint, number])[],
	ranges: readonly WireRange[],
	budget: number,
): { probes: WireRange[]; reasked: bigint[] } {
	// The live ranges of a kind equal to `range`, each with the budget it was asked under.
	const live = (range: WireRange, probe: boolean): [bigint, number][] => {
		const key = rangeKey(range);
		const found: [bigint, number][] = [];
		for (const [id, held] of entry.live.entries()) {
			if (entry.fillIds.has(id) || entry.probeIds.has(id) !== probe || rangeKey(held) !== key) continue;
			found.push([id, entry.budgets.get(id) ?? 0]);
		}
		return found;
	};
	const out: WireRange[] = [];
	const reasked: bigint[] = [];
	for (const [time, wanted] of asks) {
		const from = (range: WireRange) =>
			(range.start?.time ?? (range.start?.frame ? undefined : range.start?.absolute)) === time;
		// Asked under this budget unless live already, under the one it was recorded with.
		const answers = (range: WireRange) => {
			const held = live(range, false);
			return (held.length > 0 ? Math.max(...held.map(([, asked]) => asked)) : budget) >= wanted;
		};
		if (ranges.some((range) => from(range) && answers(range))) continue;
		const range = probe(time);
		for (const [id, asked] of live(range, true)) if (asked < wanted) reasked.push(id);
		out.push(range);
	}
	return { probes: out, reasked };
}

// The range a copy asks its upstream to place time start `time` with: the group containing it
// alone, so it delivers nothing another range does not, and readers starting there share it.
// Mirrors the Rust `probe`.
function probe(time: bigint): WireRange {
	return { start: { time }, end: { time } };
}

// Whether two lists of asks, each a time with its budget, are the same.
function sameAsks(a: readonly (readonly [bigint, number])[], b: readonly (readonly [bigint, number])[]): boolean {
	return a.length === b.length && a.every(([time, budget], i) => time === b[i][0] && budget === b[i][1]);
}

// Whether a lite-07 range with no group to deliver says its publisher held none at or before its
// reach: only one whose end takes in the group containing its time, which a range that ends before
// its time may not. Mirrors the Rust `SubStream::report_starts`.
function reaches(end: WireAnchor | undefined, asked: bigint): boolean {
	if (end === undefined) return false;
	if (end.time !== undefined) return end.time >= asked;
	if (end.absolute !== undefined) return end.absolute > asked;
	return false;
}

// The last time a lite-07 range ending at `end` reaches, undefined while it follows the live edge:
// an exclusive group end stops at the time before it, though never before the range's own time
// `asked`. Mirrors the Rust `SubStream::report_starts`.
function through(end: WireAnchor | undefined, asked: bigint): bigint | undefined {
	if (end === undefined) return undefined;
	if (end.time !== undefined) return end.time;
	return end.absolute > asked ? end.absolute - 1n : asked;
}
