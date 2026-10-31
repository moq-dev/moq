import { Instant, Milli, Nano, now, sleep } from "@moq/clock";
import { type Dispose, type Getter, Once, race, Signal } from "@moq/signals";
import type { Credential, Grant } from "../auth.ts";
import { Gate } from "../auth_gate.ts";
import type { AuthSession } from "../auth_session.ts";
import { enforceGrant } from "../auth_session.ts";
import type * as broadcast from "../broadcast.ts";
import { Withdrawal } from "../connection/withdrawal.ts";
import * as DatagramStream from "../datagram_stream.ts";
import type * as Epoch from "../epoch.ts";
import {
	controlTimeout,
	error,
	NoStreamCredit,
	NotFound,
	ProtocolViolation,
	reason,
	SessionCode,
	SessionError,
	StreamCode,
	StreamError,
	unauthorized,
} from "../error.ts";
import type * as group from "../group.ts";
import type { Hop, Route } from "../hop.ts";
import { hiddenBelow, hooks, presented } from "../internal.ts";
import type { Consumer as OriginConsumer } from "../origin.ts";
import * as Path from "../path.ts";
import { Priority, sendOrder } from "../priority.ts";
import { type Stream, Writer } from "../stream.ts";
import { Timescale } from "../time.ts";
import type { Groups, Info, Position as TrackPosition, Subscriber as TrackSubscriber } from "../track.ts";
import { untilAborted } from "../util/abort.ts";
import { nextChange } from "../util/change.ts";
import { Deadline, REQUEST_TIMEOUT_MS, requestOptions } from "../util/deadline.ts";
import { untilGivenUp } from "../util/given_up.ts";
import { TimeoutError, withTimeout } from "../util/timeout.ts";
import * as Varint from "../varint.ts";
import { type Advertised, type Advertisements, sameInstance, wireOf } from "../wire.ts";
import * as Cluster from "./cluster.ts";
import { encodeDatagram } from "./datagram.ts";
import { requestReason } from "./error.ts";
import { type Fetch, FetchHeader, FetchOk, type FetchType } from "./fetch.ts";
import * as Filter from "./filter.ts";
import { MAX_AGE_PARAMETER } from "./max_age.ts";
import * as Message from "./message.ts";
import * as Namespace from "./namespace.ts";
import { FetchFrame, Frame, Group as GroupMessage } from "./object.ts";
import { Parameters } from "./parameters.ts";
import { DEFAULT_WIRE, fromWire, toWire } from "./priority.ts";
import * as Properties from "./properties.ts";
import { Publish, PublishDone, PublishDoneStatus } from "./publish.ts";
import { type AdvertParams, PublishNamespace, PublishNamespaceUpdate } from "./publish_namespace.ts";
import { RequestError, RequestOk } from "./request.ts";
import {
	type Framed,
	malformed,
	namespaceUpdate,
	type Refusal,
	RequestStream,
	refuseReserved,
	refuseUpdate,
} from "./request_stream.ts";
import type { Session } from "./session.ts";
import { GroupOrder, Subscribe, SubscribeOk } from "./subscribe.ts";
import {
	type SubscribeNamespace,
	SubscribeNamespaceEntry,
	SubscribeNamespaceEntryDone,
} from "./subscribe_namespace.ts";
import { PublishSkipped, type SubscribeTracks } from "./subscribe_tracks.ts";
import { MalformedAuthToken } from "./token.ts";
import { TrackStatusOk, type TrackStatusRequest } from "./track.ts";
import { type IetfVersion, Version } from "./version.ts";

/** First wait before re-offering a namespace the peer refused or we couldn't open for. */
const RETRY_BASE = 100;

/** Ceiling on that wait. The loop retries for the life of the session, so it must not spin. */
const RETRY_MAX = 5000;

/** Cluster parameters for one snapshot: the advertised hops, then our own id last. */
function clusterFor(base: Cluster.Advert | undefined, route: Route): Cluster.Advert | undefined {
	if (!base) return undefined;
	const self = base.hops.at(-1);
	if (self === undefined) return undefined;
	const hops = route.hops.at(-1) === self ? [...route.hops] : [...route.hops, self];
	return { hops, cost: route.cost };
}

/**
 * What it takes to move the peer from `held` to `next`.
 *
 * - `"same"`: the peer would decode the same advertisement. The route without Cluster
 *   never reaches the wire, so a change there sends nothing.
 * - `"restart"`: nothing is held, nothing is wanted, or a different broadcast, which is
 *   withdrawn and advertised again.
 * - Otherwise the same broadcast at a new hop chain or static cost: the cluster parameters
 *   the peer holds and the ones it should, for the target to update.
 */
function change(
	base: Cluster.Advert | undefined,
	held: Advertised | undefined,
	next: Advertised | undefined,
): "same" | "restart" | Reprice {
	// Another publisher instance: moq-transport has no restart, so the peer sees the namespace
	// end and start again, and resubscribes.
	if (held === undefined || next === undefined || !sameInstance(held, next)) return "restart";
	const from = clusterFor(base, held.route);
	const to = clusterFor(base, next.route);
	if (from === undefined || to === undefined) return from === to ? "same" : "restart";
	if (from.cost === to.cost && sameHops(from.hops, to.hops)) return "same";
	return { from, to };
}

/**
 * What changed for a namespace the peer holds: its cluster parameters (from what it holds
 * to what it should), its token, or both.
 */
interface Reprice {
	from?: Cluster.Advert;
	to?: Cluster.Advert;
	/** The token the request should carry instead, when it changed. */
	token?: Credential;
}

/** Whether two hop paths list the same hops in the same order. */
function sameHops(a: Hop[], b: Hop[]): boolean {
	return a.length === b.length && a.every((hop, i) => hop === b[i]);
}

/**
 * How long one advertisement may take to be answered. Accepting the stream is only half
 * the exchange: one it never answers on holds the
 * loop just as effectively as one it never grants.
 */
const ADVERTISE_TIMEOUT_MS = REQUEST_TIMEOUT_MS;
const ADVERTISE_TIMEOUT = Nano.fromMilli(Milli(ADVERTISE_TIMEOUT_MS));

/** How long a joining FETCH waits for the subscription it names to arrive and resolve. */
const JOIN_TIMEOUT = Nano.fromMilli(Milli(REQUEST_TIMEOUT_MS));

/** Sleep `delay`, jittered, so a relay's namespaces don't all retry on the same tick. */
function retryAfter(delay: number): Promise<void> {
	return sleep(Nano.fromMilli(Milli(delay * (0.5 + Math.random() / 2))));
}

/**
 * What prevents re-offering the namespace.
 *
 * A peer answers a request it declines with a retry interval, and ignoring it is how a
 * permanent refusal (unauthorized, uninterested) turns into a request every few seconds
 * for the life of the session. `"never"` is an interval of 0 or a terminal local failure;
 * a number is the epoch milliseconds before which we must not come back.
 */
type Refused = "never" | number;

/**
 * How the peer answered an advertisement or an update: it holds the namespace, it does
 * not and said nothing about coming back (`"dropped"`), or it refused.
 */
type Answer = "held" | "dropped" | Refused;

/** One open PUBLISH_NAMESPACE request. */
type NamespaceRequest = {
	path: Path.Valid;
	requestId: bigint;
	stream: RequestStream;
	auth?: Credential;
	reply?: Promise<Framed | undefined>;
	updating?: boolean;
};

/** The open PUBLISH_NAMESPACE request per advertised path. */
type Requests = Map<Path.Valid, NamespaceRequest>;

/** What one announce loop knows about the peer, keyed the way that loop names namespaces. */
interface Namespaces {
	/** What the peer holds, by identity plus route. */
	active: Map<Path.Valid, Advertised>;

	/** What the peer refused, and whether coming back is worth anything. */
	refused: Map<Path.Valid, Refused>;

	/** The advertisement each refusal was about, so a republish or new epoch at the same path clears it. */
	offered: Map<Path.Valid, Advertised>;
}

/** The bytes a request carries for a token this session issued. */
interface Carrier {
	carried(token: Credential | undefined): Uint8Array | undefined;
	/** Bumps whenever one of the session's tokens ends, so a request carrying it drops it. */
	readonly ended: Getter<number>;
}

/** Where one announce loop sends its advertisements. */
interface Target {
	/** Advertise a namespace the peer does not hold. */
	advertise(key: Path.Valid, next: Advertised): Promise<Answer>;

	/**
	 * Replace the instance behind a namespace the peer holds with another (MoQ Epoch): the
	 * new advertisement goes out before the old is withdrawn, so the peer never sees the
	 * namespace absent.
	 */
	replace(key: Path.Valid, next: Advertised): Promise<Answer>;

	/** Reprice a namespace the peer holds: the same broadcast at a new hop chain or cost. */
	update(key: Path.Valid, next: Advertised, reprice: Reprice): Promise<Answer>;

	/** Withdraw a namespace the peer holds. */
	withdraw(key: Path.Valid): Promise<void>;

	/** The token the peer holds a namespace under, or undefined when its advertisement carries none. */
	held(key: Path.Valid): Credential | undefined;

	/** The token an advertisement naming `auth` carries: only a request can carry one. */
	carry(auth: Credential | undefined): Credential | undefined;
}

/** What {@link Publisher.runGroup} needs to serve one group. */
interface RunGroup {
	/** The source retaining the exact terminal independently of cached groups. */
	track: TrackSubscriber;
	/** The subscription's request ID, doubling as the track alias. */
	requestId: bigint;

	/** The group to serve. */
	group: group.Consumer;

	/**
	 * The units each object's presentation timestamp is written in, or undefined when objects
	 * carry none.
	 *
	 * Undefined when SUBSCRIBE_OK carries no TIMESCALE: the track has no timeline, the
	 * subscriber sent INCLUDE_PROPERTIES=0. A
	 * timestamp whose units were never declared is worse than none. Our own reader discards
	 * it; another may read it as some default and time the media wrong.
	 */
	timescale?: Timescale;

	/** The publisher's tie-break priority, already converted to the IETF wire convention. */
	publisherPriority: number;

	/** The subscription's ranking, which orders its group streams by priority and GROUP_ORDER. */
	ranking: Priority;

	/** The objects of this group the subscription's filter selects, marked once a narrowing retires them. */
	live: LiveGroup;
	/** The requested range, whose narrowing may give this stream up. */
	bounds: Getter<ServeRange>;

	/** Settles when the subscriber leaves, dropping a group still queued for a stream slot. */
	unsubscribed: Promise<void>;

	/** The subscription's data stream count, which PUBLISH_DONE reports. */
	streams: StreamCount;
}

/** How many data streams a subscription opened, fill streams included. */
type StreamCount = { opened: number; terminalSent?: boolean };

/** A PUBLISH's state across every pause and resume. */
interface Published {
	streams: StreamCount;
	unlisted: Once<Unlisted>;
	answer?: { requestId: bigint };
}

/** How a listed track left the listing: it finished, or it left unfinished. */
type Unlisted = "finished" | "removed";

/** The next listed track to PUBLISH, ready but for its stream. */
interface Staged {
	name: string;
	info: Info;
	/** On draft-18, claims the track's one subscription from now on. */
	claim?: string;
}

/** What a broadcast's listing woke for. */
type Woken =
	| { kind: "track"; event: broadcast.TracksEvent | undefined }
	| { kind: "opened"; stream: Stream }
	| { kind: "failed"; err: unknown }
	| { kind: "changed" };

/** Stop forwarding without ending the subscription. */
class Paused extends Error {
	/** Whether an update asked for the pause, which is answered once forwarding stopped. */
	readonly answer: boolean;

	constructor(answer = false) {
		super("paused");
		this.answer = answer;
	}
}

/** What {@link Publisher.runFill} needs to serve one subscription's backfill. */
interface RunFill {
	/** The initial SUBSCRIBE or producing update's request ID, which the fetch stream names. */
	requestId: bigint;

	/** The subscriber's delivery priority, applied to the cache subscription the fill reads. */
	priority: number;

	/**
	 * The subscription's ranking. The fill joins it as the group it fills, so backfill goes out
	 * in the subscription's GROUP_ORDER rather than ahead of the live groups or behind them all.
	 */
	ranking: Priority;

	/** The range the fill resolved to. */
	fill: FillServe;

	/**
	 * An independent view of the same track, used to read the cached group without consuming
	 * anything the subscription will deliver.
	 */
	cache: TrackSubscriber;

	/** The units objects are stamped in, if any; see {@link RunGroup.timescale}. */
	timescale?: Timescale;

	/** Settles when the subscriber leaves, releasing a fill still waiting on its group. */
	unsubscribed: Promise<void>;
	/** Interrupts a fill's pending reads and flow-controlled writes. */
	cancelled: AbortSignal;

	/** The subscription's data stream count, which PUBLISH_DONE reports. */
	streams: StreamCount;
}

/**
 * Handles publishing broadcasts using moq-transport protocol.
 * Uses the stream-per-request pattern (real bidi streams for v17, virtual for v14-v16).
 *
 * @internal
 */
export class Publisher {
	#withdrawal = new Withdrawal();

	withdraw(): Promise<void> {
		return this.#withdrawal.close();
	}
	#quic: WebTransport;
	#session: Session;
	#requiresSolicitation: boolean;
	#listings = new Set<Path.Valid>();
	#trackClaims = new Map<string, "publish" | number>();
	// What a joining FETCH needs from each draft-18 SUBSCRIBE, by its request ID, while it lasts:
	// pending from its arrival until it resolves. Request streams race, so a joining FETCH can
	// arrive before the subscription it names has.
	#joins = new Map<bigint, Joinable | "pending">();
	// Bumped on every change to #joins, which a waiting joining FETCH rechecks.
	#joinsChanged = new Signal(0);

	// The one writer for the outbound datagram stream, shared by every subscription since a
	// second getWriter would throw. Undefined when the transport carries no datagrams, which
	// turns them off: there is no stream fallback. Released in close().
	#datagramWriter?: WritableStreamDefaultWriter<Uint8Array>;
	#hidden: boolean;
	// Whether a SUBSCRIBE_NAMESPACE's REQUEST_OK counts the NAMESPACEs that follow it (MoQ Active Count).
	#activeCount: boolean;

	// The origin this session serves, borrowed: it outlives the session, and closing the
	// session leaves its broadcasts alone. Namespaces go out as an unsolicited
	// PUBLISH_NAMESPACE (see {@link runPublishNamespaces}) unless the peer asked to be
	// told on request. On draft-16 and later {@link runSubscribeNamespace} also carries
	// every match: a NAMESPACE is discovery, not a second route.
	#advertised: Getter<Advertisements | undefined>;
	// Bumped when an idle draft-18+ advertisement ends without an update, so the announce
	// loop re-offers it instead of holding a request the peer dropped.
	#lapsed = new Signal(0);
	#publish?: OriginConsumer;

	// What every advertisement carries on a session that negotiated the MoQ Cluster
	// extension: a hop path holding our own id, so the peer can tell that what it hears
	// back came from us. `undefined` when nothing negotiated it.
	#advert?: Cluster.Advert;

	// Whether both SETUPs offered MoQ Epoch: every advertisement names its route's epoch, and
	// a request may name the one it expects.
	#epoch: boolean;

	// Our grant (MoQ Auth): only what it lets us publish is advertised and served, and a
	// shrink withdraws what it no longer covers. Undefined until the peer answers, which
	// allows everything.
	#grant: Getter<Grant | undefined>;
	// Holds every incoming request to that grant from dispatch to its end.
	#gate: Gate;

	// Resolves once the tokens this session presented at setup are answered, so nothing is
	// advertised before the grant it would be checked against.
	#ready: Promise<void>;

	// The bytes a request carries for a route's token, for only the tokens this session issued.
	#auth?: Carrier;

	/**
	 * Creates a new Publisher instance.
	 *
	 * @internal
	 */
	constructor({
		quic,
		session,
		publish,
		requiresSolicitation,
		hidden = false,
		activeCount = false,
		cluster,
		epoch = false,
		grant = new Signal<Grant | undefined>(undefined),
		auth,
		ready = Promise.resolve(),
	}: {
		/** The WebTransport session, for uni streams. */
		quic: WebTransport;
		/** The session abstraction for bidi streams and request IDs. */
		session: Session;
		/** The origin whose broadcasts this session serves; omit to publish nothing. */
		publish?: OriginConsumer;
		/** Whether the peer's SETUP asked to be told on request (MoQ Solicit). */
		requiresSolicitation: boolean;
		/** Whether the peer declared MoQ Hidden. */
		hidden?: boolean;
		/** Whether both SETUPs declared MoQ Active Count. */
		activeCount?: boolean;
		/** The Hop IDs the SETUP exchange settled (MoQ Cluster). */
		cluster?: Cluster.Hops;
		/** Whether both SETUPs offered MoQ Epoch. */
		epoch?: boolean;
		/** The union of our tokens' grants (MoQ Auth), which bounds what we publish. */
		grant?: Getter<Grant | undefined>;
		/** This session's tokens, which a route names for its request to carry. */
		auth?: Carrier;
		/** Resolves once the setup tokens are answered (MoQ Auth). */
		ready?: Promise<void>;
	}) {
		this.#quic = quic;
		this.#grant = grant;
		this.#gate = new Gate(grant);
		this.#auth = auth;
		this.#ready = ready;
		this.#session = session;
		const origin = publish && wireOf(publish);
		this.#advertised = origin?.advertised ?? new Signal(new Map());
		this.#publish = publish;
		this.#requiresSolicitation = requiresSolicitation;
		this.#hidden = hidden;
		this.#activeCount = activeCount;
		this.#advert = Cluster.advertise(cluster);
		this.#epoch = epoch;
		this.#datagramWriter = DatagramStream.datagramWriter(quic);
	}

	/**
	 * Release the datagram writer's lock so the transport can be torn down. The broadcasts
	 * belong to the origin, which outlives this session.
	 *
	 * @internal
	 */
	close() {
		this.#datagramWriter?.releaseLock();
		this.#datagramWriter = undefined;
	}

	/**
	 * Handles an incoming SUBSCRIBE request on a bidi stream.
	 * Owns the full lifecycle: sends response, serves track data, waits for close.
	 *
	 * @internal
	 */
	runSubscribe(msg: Subscribe, stream: Stream): Promise<void> {
		const request = new RequestStream(stream, this.#session, this.#quic);
		return request.serve(this.#runSubscribe(msg, request));
	}

	async #runSubscribe(msg: Subscribe, request: RequestStream) {
		this.#pinned(msg.epoch);
		// Draft-22 replaced joining FETCH with fills.
		const joinable = !Filter.isDraft22(this.#session.version);
		if (joinable) this.#setJoin(msg.requestId, "pending");
		const key = JSON.stringify([msg.trackNamespace, msg.trackName]);
		const claimed = this.#session.version === Version.DRAFT_18;
		let held = false;
		try {
			if (Namespace.reserved(msg.trackNamespace, this.#session.version)) {
				await refuseReserved(request);
				return;
			}
			if (claimed && this.#trackClaims.get(key) === "publish") {
				await Message.write(
					request.writer,
					RequestError.id,
					new RequestError({ errorCode: 0x19, reasonPhrase: "track already PUBLISHed" }),
					this.#session.version,
				);
				return;
			}
			if (claimed) {
				this.#trackClaims.set(key, ((this.#trackClaims.get(key) as number | undefined) ?? 0) + 1);
				held = true;
			}
			await this.#serveSubscribe(msg, request);
		} finally {
			// A joining FETCH arriving after the subscription ended has no edge to end at.
			if (joinable) this.#setJoin(msg.requestId, undefined);
			if (held) {
				const count = (this.#trackClaims.get(key) as number) - 1;
				if (count) this.#trackClaims.set(key, count);
				else this.#trackClaims.delete(key);
			}
		}
	}

	async #serveSubscribe(msg: Subscribe, request: RequestStream) {
		const version = this.#session.version;
		const name = msg.trackNamespace;
		let broadcast: broadcast.Consumer | undefined;
		let refusal: Refusal | undefined;

		// The requester leaving while the broadcast and track resolve ends the request, so a
		// cancelled subscription gives back its slot instead of holding it until the track
		// is answered, which may be never.
		const left = new AbortController();
		const cancel = new StreamError(StreamCode.Cancel, { message: "cancel" });
		void request.ended.then(() => left.abort(cancel));

		// Legal requests we can't honor are refused one at a time: serving a Range Filter
		// unfiltered would deliver objects it excluded.
		// An instance never comes back, so a request pinned to a gone one is missing, not
		// worth waiting for.
		const rendezvous = msg.rendezvous && msg.epoch === undefined;

		if (msg.rangeFilters) {
			refusal = { condition: "invalid_filter", reasonPhrase: "range filters not supported" };
		} else if (!this.#gate.allows(name)) {
			// Serve only what our grant lets us publish. Checked before resolving, so a denied
			// request never reaches the origin.
			refusal = { condition: "unauthorized", reasonPhrase: "not granted" };
		} else {
			try {
				broadcast = await this.#resolve(name, left.signal, msg.epoch);
			} catch (err: unknown) {
				// Nobody is left to refuse.
				if (left.signal.aborted) {
					request.abort(cancel);
					return;
				}
				refusal = this.#failed(err, rendezvous);
			}
		}

		if (refusal) {
			await request.refuse("subscribe", refusal);
			return;
		}
		if (!broadcast) return;

		const subscribe = (source: broadcast.Consumer) => {
			// A range that resolves without the live edge has a fixed start, which is no join:
			// bound the subscription before subscribing so the refresh ratio never judges it.
			const fixed = subscribeRange(msg, {}, version);
			return source.track(msg.trackName).subscribe({
				priority: fromWire(msg.subscriberPriority),
				order: msg.groupOrder,
				// An absent budget preserves everything retained by the producer.
				maxDelay: Milli(msg.maxAge ?? Varint.MAX_U53),
				groups: fixed.start ? rangeGroups(fixed) : undefined,
			});
		};
		let track = subscribe(broadcast);

		try {
			let info: Awaited<ReturnType<TrackSubscriber["info"]>>;
			try {
				info = await untilAborted(track.info(), left.signal);
			} catch (err) {
				if (left.signal.aborted) {
					request.abort(cancel);
					return;
				}
				const failed = error(err);
				if (
					!(failed instanceof StreamError) ||
					(failed.code !== StreamCode.NotFound && failed.code !== StreamCode.Unroutable)
				)
					throw err;
				// The broadcast resolved, so only a vanished route still means a missing publisher;
				// a missing track is the publisher's answer.
				await request.refuse(
					"subscribe",
					this.#failed(err, rendezvous && failed.code === StreamCode.Unroutable),
				);
				return;
			}
			// Every subscription a resume makes counts toward the same PUBLISH_DONE.
			const streams: StreamCount = { opened: 0 };
			let paused = await this.#forward({ msg, request, track, info, streams });
			// A pause drops the subscription, and its demand with it; a resume subscribes again as
			// a new SUBSCRIBE would, so nothing carries across but the request.
			while (paused) {
				track.close();
				if (paused.answer) await Message.write(request.writer, RequestOk.id, new RequestOk(), version);
				const answer = await this.#pausedSubscribe(msg, request, streams);
				if (!answer) return;
				track = subscribe(broadcast);
				let resumed: Info;
				try {
					resumed = await untilAborted(track.info(), left.signal);
					// A resume answers with the live edge, which only a granted subscriber learns.
					if (!this.#gate.allows(name)) throw unauthorized(name);
				} catch (err) {
					if (left.signal.aborted) return;
					await refuseUpdate(request.writer, version, "subscribe", this.#failed(err));
					await this.#publishFailed(request, streams);
					request.close();
					return;
				}
				paused = await this.#forward({ msg, request, track, info: resumed, streams, answer });
			}
		} finally {
			track.close();
		}
	}

	/**
	 * Hold a paused SUBSCRIBE, answering its updates, until one resumes it: the update to
	 * answer then, or none once the request ended.
	 *
	 * A paused subscription holds nothing upstream, so only its priority and max age change
	 * while it waits. It still ends once our grant stops covering it, as a forwarding one does.
	 */
	async #pausedSubscribe(
		msg: Subscribe,
		request: RequestStream,
		streams: StreamCount,
	): Promise<{ requestId: bigint } | undefined> {
		const version = this.#session.version;
		for (;;) {
			const revoked = new Once<true>();
			const gate = this.#gate.watch(msg.trackNamespace, () => revoked.set(true));
			if (gate.revoked()) revoked.set(true);
			let framed: Framed | undefined | true;
			try {
				framed = await race([request.next(), revoked]);
			} finally {
				gate.dispose();
			}
			if (framed === true) {
				const done = new PublishDone({
					statusCode: PublishDoneStatus.UNAUTHORIZED,
					streamCount: BigInt(streams.opened),
					reasonPhrase: "not granted",
				});
				await Message.write(request.writer, PublishDone.id, done, version);
				request.close();
				return undefined;
			}
			if (framed === undefined) {
				await request.ended;
				return undefined;
			}
			let update: { params: Parameters; requestId: bigint };
			try {
				update = await this.#decodeUpdate(request, framed);
			} catch (err) {
				if (!(err instanceof UpdateFailed)) throw err;
				await this.#publishFailed(request, streams);
				request.close();
				return undefined;
			}
			this.#applyPublished(msg, update.params);
			if (update.params.forward === true) return { requestId: update.requestId };
			await Message.write(request.writer, RequestOk.id, new RequestOk(), version);
		}
	}

	/**
	 * Decode a SUBSCRIBE's REQUEST_UPDATE. One this publisher cannot apply is refused on the
	 * wire, and the thrown {@link UpdateFailed} ends the subscription.
	 */
	async #decodeUpdate(request: RequestStream, framed: Framed): Promise<{ params: Parameters; requestId: bigint }> {
		const version = this.#session.version;
		if (framed.type !== 0x02)
			throw new ProtocolViolation(`unexpected message on a subscription: 0x${framed.type.toString(16)}`);
		const refused = async (condition: "not_supported" | "malformed_auth_token", reasonPhrase: string) => {
			await refuseUpdate(request.writer, version, "subscribe", { condition, reasonPhrase });
			return new UpdateFailed();
		};
		let params: Parameters;
		let requestId: bigint;
		try {
			({ params, requestId } = await this.#subscriptionUpdate(framed));
		} catch (err) {
			// Section 8.9 rejects the update, which says more than that we do not support it.
			if (err instanceof MalformedAuthToken) throw await refused("malformed_auth_token", err.message);
			throw malformed(err, "REQUEST_UPDATE");
		}
		if (params.trackPropertyFilter)
			throw new ProtocolViolation("TRACK_PROPERTY_FILTER is only allowed on SUBSCRIBE_TRACKS updates");
		const unsupported =
			[...params.bytes.keys()].some((key) => key !== 0x21n && key !== 0x23n) ||
			params.rangeFilters ||
			[...params.vars.keys()].some(
				(key) =>
					key !== MAX_AGE_PARAMETER &&
					key !== 0x02n &&
					key !== 0x06n &&
					key !== 0x10n &&
					key !== 0x20n &&
					key !== 0x22n,
			);
		if (unsupported) throw await refused("not_supported", "REQUEST_UPDATE parameters not supported");
		return { params, requestId };
	}

	async #forward({
		msg,
		request,
		track,
		info,
		published,
		streams = published?.streams ?? { opened: 0 },
		answer,
	}: {
		msg: Subscribe;
		request: RequestStream;
		track: TrackSubscriber;
		info: Awaited<ReturnType<TrackSubscriber["info"]>>;
		published?: Published;
		/** The data streams opened over every time the subscription forwarded. */
		streams?: StreamCount;
		/** The update a resumed SUBSCRIBE answers with REQUEST_OK instead of SUBSCRIBE_OK. */
		answer?: { requestId: bigint };
	}): Promise<Paused | undefined> {
		const version = this.#session.version;
		const name = msg.trackNamespace;
		const priority = fromWire(msg.subscriberPriority);
		const order = msg.groupOrder;
		let cache: TrackSubscriber | undefined;
		let ranking: Priority | undefined;
		try {
			// A Timestamp goes out only when this SUBSCRIBE_OK actually carries TIMESCALE.
			const timescale = msg.propertiesWanted ? (info.timescale ?? undefined) : undefined;
			// The model ranks higher-first, the IETF wire lower-first. Every group this
			// subscription serves carries the same publisher priority, which is what lets a
			// relay prefer catalog and audio over video when it has no subscriber preference
			// to go on.
			const publisherPriority = toWire(info.priority);

			// The filter and any fill are relative to the live edge, so snapshot it once: the
			// fill ends exactly where a Next Object subscription begins, which is what lets
			// the draft's current-group join (Next Object plus a StartGroup=1 fill) cover the
			// group with no gap and no overlap.
			const edge = liveEdge(track);
			const fill =
				msg.fill && msg.forward && Filter.isDraft22(version)
					? fillRange(msg.fill, msg.filter, edge.largest)
					: undefined;
			const requested = subscribeRange(msg, edge, version);
			// A copy, since an update moves the cursor in place and must not move the bounds too.
			const range = { ...afterFill(requested, fill, edge) };
			// What the subscriber asked for, which only a narrowing changes: the fill's split
			// moves the cursor alone, so it never gives up a stream still in range.
			const bounds = new Signal(requested);

			// The wire request tells an upstream what we need; the cursor is what actually
			// trims this subscriber, since the producer fans every cached group out to every
			// sink regardless. An absent start joins at the latest group, which is what
			// moq-lite means by joining a live track. The range's end is the inclusive last
			// group; the model's `endGroup` is exclusive.
			track.update({
				priority,
				order,
				maxDelay: Milli(msg.maxAge ?? Varint.MAX_U53),
				groups: rangeGroups(range),
			});
			const startGroup = range.start ? Number(range.start.group) : hooks.latestSequence(track);
			if (startGroup !== undefined) track.withRange({ ...rangeGroups(range), start: { included: startGroup } });
			// The live cursor's lowest group, which a fill-only update never lowers.
			let floor = startGroup ?? 0;

			// A fill reads the group cache through its own consumer, independent of the
			// subscription's cursor. Forked from this subscriber rather than resolved through
			// the broadcast again: a dynamic serve is one request per peer subscription, so
			// asking the broadcast would mint a second producer nobody has accepted.
			cache =
				fill && fill.kind !== "empty"
					? track.fork({ priority, order, maxDelay: Milli(Varint.MAX_U53) })
					: undefined;

			const ranked = new Priority(track, info.priority);
			ranking = ranked;
			const properties = trackProperties(info, msg.propertiesWanted);
			// Draft-22 replaced joining FETCH with fills. Older drafts save the Largest Object the
			// subscription starts after, again on every resume, so a joining FETCH ends exactly
			// where the subscription begins. Saved before the answer names it.
			if (!published && !Filter.isDraft22(version)) {
				this.#setJoin(
					msg.requestId,
					msg.filter.kind === "nextObject"
						? {
								kind: "join",
								track,
								largest: edge.largest,
								namespace: name,
								properties,
								timescale,
								ranking: ranked,
							}
						: { kind: "unsupported" },
				);
			}

			if (published) {
				if (published.answer) {
					const parameters = new Parameters();
					if (edge.largest)
						parameters.largest = { groupId: edge.largest.group, objectId: edge.largest.object };
					await Message.write(request.writer, RequestOk.id, new RequestOk({ parameters }), version);
				}
			} else if (answer) {
				// The resuming update learns where the subscription now joins.
				const parameters = new Parameters();
				if (edge.largest) parameters.largest = { groupId: edge.largest.group, objectId: edge.largest.object };
				await Message.write(request.writer, RequestOk.id, new RequestOk({ parameters }), version);
			} else {
				// Send SUBSCRIBE_OK
				const ok = new SubscribeOk({
					trackAlias: msg.requestId,
					// Required once the track has content. A Next Object subscription starts right
					// after it, and recovers the group's head with a FILL on draft-22 or a joining
					// FETCH before it, both of which end here.
					largest: edge.largest && { groupId: edge.largest.group, objectId: edge.largest.object },
					properties,
				});
				await Message.write(request.writer, SubscribeOk.id, ok, version);
				// A SUBSCRIBE may start paused, forwarding nothing until an update resumes it.
				if (!msg.forward) {
					msg.forward = true;
					return new Paused();
				}
			}
			console.debug(`publish ok: broadcast=${name} track=${track.name}`);

			// Cancels groups still queued for a stream slot. Only the subscriber leaving counts:
			// a track that ran out of groups still has to flush the ones already queued, and we
			// close the stream ourselves below to say so.
			let finished = false;
			let unsubscribe!: () => void;
			const fillAbort = new AbortController();
			const unsubscribed = new Promise<void>((resolve) => {
				unsubscribe = () => {
					fillAbort.abort(
						new StreamError(StreamCode.Cancel, { message: "unsubscribed before the fill finished" }),
					);
					resolve();
				};
			});
			let filling = Promise.resolve();
			const startFill = (requestId: bigint, selected: FillServe, source: TrackSubscriber) => {
				filling = this.#runFill({
					requestId,
					priority: fromWire(msg.subscriberPriority),
					ranking: ranked,
					fill: selected,
					cache: source,
					timescale,
					unsubscribed,
					cancelled: fillAbort.signal,
					streams,
				});
			};
			if (fill && cache)
				startFill(answer?.requestId ?? published?.answer?.requestId ?? msg.requestId, fill, cache);

			// Every group started, until its stream finishes or resets, and the data streams
			// opened for PUBLISH_DONE to report.
			const groups = new Set<Promise<void>>();
			// The stream serving the highest group, where a widening fill's tail resumes.
			let live: LiveGroup | undefined;
			const serveGroup = (group: group.Consumer, slice: GroupSlice) => {
				const stream: LiveGroup = { sequence: group.sequence, slice, retired: false };
				if (!live || group.sequence >= live.sequence) live = stream;
				const task = this.#runGroup({
					track,
					requestId: msg.requestId,
					group,
					timescale,
					publisherPriority,
					ranking: ranked,
					live: stream,
					bounds,
					unsubscribed,
					streams,
				});
				groups.add(task);
				void task.finally(() => groups.delete(task));
			};

			const updates = (async () => {
				for (;;) {
					const framed = await request.next();
					// The requester sent its last message, which ends the request only where FIN cancels.
					if (framed === undefined) return request.ended;
					const { params, requestId: ownRequestId } = await this.#decodeUpdate(request, framed);
					// update() replaces every option, so carry the rest over as Rust does.
					if (params.subscriberPriority !== undefined)
						track.update({ ...track.subscription.peek(), priority: fromWire(params.subscriberPriority) });
					if (params.maxAge !== undefined)
						track.update({ ...track.subscription.peek(), maxDelay: Milli(params.maxAge) });
					// A fill holds one cache view. At most this decoded update waits behind
					// it; request parsing then backpressures any further fill requests.
					if (params.fillParameters) await filling;
					const snapshot = liveEdge(track);
					this.#applyPublished(msg, params);
					const selected =
						msg.fill && Filter.isDraft22(version)
							? fillRange(msg.fill, msg.filter, snapshot.largest)
							: undefined;
					if (params.subscriptionFilter !== undefined || selected) {
						// An update's unfiltered range explicitly lowers the floor to zero;
						// an initial unfiltered subscription still joins the live group. A
						// fill-only update keeps the range it already has.
						const requested: ServeRange =
							params.subscriptionFilter === undefined
								? bounds.peek()
								: msg.filter.kind === "unfiltered"
									? { start: { group: 0n, object: 0n } }
									: filterRange(msg.filter, snapshot);
						const tail = afterFill(requested, selected, snapshot);
						range.start = tail.start;
						range.end = tail.end;
						bounds.set(requested, true);
						track.update({ ...track.subscription.peek(), groups: rangeGroups(range) });
						// Below its floor the live cursor already passed every group or never owned it,
						// so lowering it for a fill-only update would hand those groups out a second time.
						let start = range.start ? Number(range.start.group) : 0;
						if (params.subscriptionFilter === undefined) start = Math.max(start, floor);
						floor = start;
						track.withRange({ ...rangeGroups(range), start: { included: start } });
						// The fill took the current group's head. The cursor never hands a group out
						// twice, so what its stream no longer carries resumes from the cache.
						const next = tail !== requested ? tail.start : undefined;
						const at = Number(next?.group);
						if (next && hooks.handedOut(track, at)) {
							const slice = groupSlice(range, at);
							// A stream still serving this group from the fill's edge keeps what it
							// covers, so the tail resumes where that stream stops.
							const streaming =
								live?.sequence === at &&
								live.slice.skip <= slice.skip &&
								!live.retired &&
								!excludes(requested, at, live.slice)
									? live
									: undefined;
							const resume = streaming ? streaming.slice.until : slice.skip;
							if (resume !== undefined) slice.skip = Math.max(slice.skip, resume);
							const group =
								resume !== undefined &&
								(!range.end || range.end.group >= next.group) &&
								(slice.until === undefined || slice.until > slice.skip)
									? hooks.cachedGroup(track, at)
									: undefined;
							if (group) serveGroup(group, slice);
						}
					}
					if (params.groupOrder !== undefined)
						track.update({ ...track.subscription.peek(), order: msg.groupOrder });
					// Every data stream closes before the pause is answered.
					if (params.forward === false) throw new Paused(true);
					const parameters = new Parameters();
					if (snapshot.largest)
						parameters.largest = { groupId: snapshot.largest.group, objectId: snapshot.largest.object };
					const source =
						selected && selected.kind !== "empty"
							? track.fork({
									priority: fromWire(msg.subscriberPriority),
									order: msg.groupOrder,
									maxDelay: Milli(Varint.MAX_U53),
								})
							: undefined;
					try {
						await Message.write(request.writer, RequestOk.id, new RequestOk({ parameters }), version);
					} catch (err) {
						source?.close();
						throw err;
					}
					if (selected && source) startFill(ownRequestId, selected, source);
				}
			})();
			const requestEnded = race([updates, request.ended]);
			void requestEnded.then(
				() => {
					if (!finished) unsubscribe();
				},
				// A reset is always the peer.
				() => unsubscribe(),
			);

			// Serve groups until the track ends or the requester cancels.
			const serving = (async () => {
				for (;;) {
					const group = await servingGroup(track);
					if (!group) return;
					serveGroup(group, groupSlice(range, group.sequence));
				}
			})();

			// Datagrams count toward no stream, so PUBLISH_DONE never waits on them; closing
			// the track below ends the loop.
			void this.#runDatagrams({ requestId: msg.requestId, track, timescale, publisherPriority, range });

			// Losing the grant ends the subscription, leaving the session alone.
			let revoke!: () => void;
			const revoked = new Promise<"revoked">((resolve) => {
				revoke = () => resolve("revoked");
			});
			const watch = this.#gate.watch(name, revoke);
			// The grant may have shrunk during setup, before this watcher existed.
			if (watch.revoked()) revoke();

			let publishError: Error | undefined;
			let unauthorized = false;
			let ended = false;
			try {
				const served = Symbol("served");
				const end = await race([
					serving.then(async () => {
						await filling;
						return served;
					}),
					requestEnded,
					revoked,
					...(published ? [published.unlisted] : []),
				]);
				if (end === "removed") throw new NotFound("track no longer listed");
				if (end === "revoked") {
					console.info(`subscription no longer authorized: broadcast=${name} track=${track.name}`);
					unauthorized = true;
					unsubscribe();
				}
				// A track that finished leaves the listing with TRACK_ENDED, like one served to its end.
				ended = end === served || end === "finished";
			} catch (err: unknown) {
				publishError = error(err);
				// Stop forwarding at once: a pause answers only after every data stream closed.
				if (published || publishError instanceof Paused) {
					unsubscribe();
					track.close();
				}
			} finally {
				watch.dispose();
			}
			// An update that broke the protocol, such as one whose AUTHORIZATION TOKEN names an
			// alias, closes the session: the connection answers a thrown violation with its code.
			if (publishError || !ended) unsubscribe();
			await filling;
			if (publishError instanceof ProtocolViolation) throw publishError;

			// PUBLISH_DONE waits until every stream this subscription will open is closed, as
			// the draft requires, so its count is final. The subscriber leaving cancels the
			// ones still queued instead.
			await race([Promise.all(groups), unsubscribed]);
			if (publishError instanceof Paused) {
				track.close();
				await Promise.all(groups);
				return publishError;
			}

			// PUBLISH_DONE has no end location: an END_OF_TRACK object is what tells the
			// subscriber where the track ended.
			const final = hooks.subscriberEnd(track);
			if (ended && !publishError && final !== undefined && !streams.terminalSent) {
				await this.#runEndOfTrack({
					requestId: msg.requestId,
					final,
					publisherPriority,
					unsubscribed,
					streams,
				});
			}

			console.debug(`publish done: broadcast=${name} track=${track.name}`);
			if (publishError) {
				console.warn(`publish error: broadcast=${name} track=${track.name} error=${reason(publishError)}`);
			}

			// A revocation bridged from upstream, such as a moq-lite UNAUTHORIZED reset, is one here too.
			// PUBLISH_DONE's EXPIRED means the subscription's own lifetime ran out, so an expired
			// token is reported as no longer granted.
			if (
				(publishError instanceof StreamError && publishError.code === StreamCode.Unauthorized) ||
				(publishError instanceof SessionError &&
					(publishError.code === SessionCode.Unauthorized || publishError.code === SessionCode.Expired))
			) {
				unauthorized = true;
			}

			// PUBLISH_DONE is required for every supported draft. The peer may have already
			// closed its side to unsubscribe, in which case there is nowhere to send it.
			const updateFailed = publishError instanceof UpdateFailed;
			try {
				const done = new PublishDone({
					statusCode: unauthorized
						? PublishDoneStatus.UNAUTHORIZED
						: updateFailed
							? PublishDoneStatus.UPDATE_FAILED
							: publishError
								? PublishDoneStatus.INTERNAL_ERROR
								: PublishDoneStatus.TRACK_ENDED,
					streamCount: BigInt(streams.opened),
					reasonPhrase: unauthorized
						? "not granted"
						: updateFailed
							? "update failed"
							: publishError
								? "internal error"
								: "track ended",
				});
				await Message.write(request.writer, PublishDone.id, done, version);
			} catch {
				// Stream might already be closed by peer.
			}

			// Our close follows PUBLISH_DONE; it must not cancel data still queued for delivery.
			finished = true;
			request.close();
		} catch (err: unknown) {
			if (err instanceof ProtocolViolation) throw err;
			const e = error(err);
			console.warn(`publish error: broadcast=${name} track=${track.name} error=${reason(e)}`);
			request.abort(e);
		} finally {
			// Groups still draining keep being ranked until they finish.
			ranking?.close();
			track.close();
			// Idempotent, and the backstop for a throw between the fork and the fill starting:
			// #runFill closes it itself on every path it reaches.
			cache?.close();
		}
		return undefined;
	}

	/**
	 * Runs a group and sends its frames using ObjectStream (Subgroup delivery mode).
	 */
	async #runGroup(options: RunGroup) {
		const { requestId, track, group, timescale, publisherPriority, ranking, live, bounds, unsubscribed, streams } =
			options;
		const { slice } = live;
		const excluded = Promise.withResolvers<void>();
		let rangeError: Error | undefined;
		// Set once the FIN is written, which keeps what the stream delivered.
		let finished = false;
		const narrowed = () => {
			if (excludes(bounds.peek(), group.sequence, slice)) {
				if (!finished) live.retired = true;
				rangeError = new StreamError(StreamCode.Cancel, { message: "group excluded by range update" });
				hooks.releaseGroupReader(group, rangeError);
				excluded.resolve();
			}
		};
		const changed = bounds.subscribe(narrowed);
		narrowed();
		try {
			// One stream per group is faster than a peer at its limit can retire them, so this
			// is the one path that doesn't wait for a slot: the transport would serve the opens
			// in the order we asked, not the subscription's order. Failing here drops the group
			// and lets the next one compete for the next slot.
			// Watch the group while the transport parks the open, and release that watch
			// once the open settles. tryOpen owns resetting a slot that arrives too late.
			const opened = Promise.withResolvers<void>();
			let failure: Error | undefined;
			const abandoned = untilGivenUp(group, () => opened.promise).then((stop) => {
				failure = stop;
			});
			let stream: Writer | undefined;
			try {
				stream = await Writer.tryOpen(this.#quic, {
					version: this.#session.version,
					sendOrder: ranking.rank(group.sequence, slice.skip),
					cancel: race([unsubscribed, abandoned, excluded.promise]),
					waitUntilAvailable: false,
				});
			} finally {
				opened.resolve();
			}
			if (!stream) {
				hooks.releaseGroupReader(group, failure ?? new Error("no stream slot"));
				return;
			}
			streams.opened += 1;
			// A REQUEST_UPDATE re-ranks the subscription, so a group already on the wire follows it.
			ranking.add(stream, group.sequence, slice.skip);

			const header = new GroupMessage({
				trackAlias: requestId,
				groupId: group.sequence,
				subGroupId: 0,
				publisherPriority,
				flags: {
					// The object properties carry the timestamp, so there is nothing to write
					// when the track declared no units to read one in.
					hasExtensions: timescale !== undefined,
					hasSubgroup: false,
					hasSubgroupObject: false,
					// A stream capped by the range stops before the group may end, so its FIN
					// cannot claim END_OF_GROUP.
					hasEnd: slice.until === undefined,
					hasPriority: true,
					// Only honest when the stream really starts at the group's first object;
					// a trimmed head starts partway through.
					firstObject: slice.skip === 0,
				},
			});

			// A pause or the subscriber leaving resets the stream, even mid-write: a write
			// parked on flow control would otherwise hold the pause's answer indefinitely.
			// CANCELLED, as the draft asks of a reset the subscriber caused: 0 would read as our failure.
			let left: StreamError | undefined;
			const cancelled = unsubscribed.then(() => {
				left = new StreamError(StreamCode.Cancel, { message: "unsubscribed while serving the group" });
				return undefined;
			});
			// The subscription's budget, or an eviction of what is left to read, may give up on the
			// group mid-write, which abandons the stream. A group already given up never starts the write.
			const write = async (op: () => Promise<unknown>) => {
				const stop = await untilGivenUp(group, op);
				if (stop) throw stop;
			};
			// Writing the group stops for the pause too; the drain below answers it without a reset.
			const send = async (op: () => Promise<unknown>) => {
				await race([write(op), cancelled, excluded.promise]);
				if (left) throw left;
				if (rangeError) throw rangeError;
			};

			try {
				await send(() => header.encode(stream, this.#session.version));
				// The first written object goes on the wire as its absolute id, so a trimmed
				// head shows the true numbering rather than a silently renumbered group.
				let first = true;
				// The next object id that could be written, which starts at the filter rather
				// than at zero. A backwards range (`startObject` above `endObject` in the same
				// group) is empty, and starting here is what lets the check below see that
				// before the read parks on an object the range already excludes.
				let next = slice.skip;

				for (;;) {
					// The filter ends inside this group: everything at `until` and beyond is
					// outside the requested range, so stop without waiting for the group's end.
					if (slice.until !== undefined && next >= slice.until) break;

					// Reading from the filter's start drops the objects below it: they are outside
					// the requested range, so skipping them is not a gap.
					const read = await race([
						group.readFrameSequence({ from: slice.skip }),
						stream.closed,
						cancelled,
						excluded.promise,
					]);
					if (left) throw left;
					if (rangeError) throw rangeError;
					if (!read) break;
					next = read.sequence + 1;
					if (slice.until !== undefined && read.sequence >= slice.until) break;

					const obj = new Frame({ payload: read.payload, timestamp: read.timestamp });
					const delta = first ? read.sequence : 0;
					first = false;
					await send(() => obj.encode(stream, header.flags, timescale, this.#session.version, delta));
				}

				// The track ends right after this group's last object, so the marker rides this
				// stream rather than a separate one.
				const end = hooks.subscriberEnd(track);
				const marked = end?.group === group.sequence && end.frame === next;
				if (marked) {
					await new Frame({ endOfTrack: true }).encode(
						stream,
						header.flags,
						timescale,
						this.#session.version,
						first ? next : 0,
					);
				}

				stream.close();
				finished = true;
				// The stream holds every byte it owes, so frames past a range's end failing upstream
				// can no longer give it up: release the read, leaving the budget.
				group.close();
				// Its bytes still hold the connection until acknowledged, so a group gone stale
				// meanwhile is reset like one still serving, as the Rust `GroupServe` does. Awaited
				// even without a marker: PUBLISH_DONE waits on this group, so the track, whose
				// timeline judges the budget, stays open until the stream settles.
				const delivered = await race([
					write(() => stream.closed).then(() => true),
					unsubscribed.then(() => false),
					excluded.promise.then(() => false),
				]);
				// Only a stream that closed cleanly delivered the marker: one reset after its FIN
				// may not have, which leaves the standalone fallback in place, as in Rust.
				if (marked) streams.terminalSent = delivered === true;
			} catch (err: unknown) {
				stream.reset(error(err));
			} finally {
				// A closed stream may still have data queued behind its FIN, so it keeps
				// competing at its rank until the stream settles.
				void stream.closed.catch(() => {}).then(() => ranking.remove(stream));
			}
		} catch (err: unknown) {
			// A failed open has no stream to reset, but still ends only this group.
			hooks.releaseGroupReader(group, error(err));
		} finally {
			changed();
			group.close();
		}
	}

	/**
	 * Send a track's datagrams as OBJECT_DATAGRAMs, best-effort, like moq-lite.
	 *
	 * One that does not fit the transport's limit is dropped; there is no stream fallback. A
	 * send failure stops the loop but never fails the subscription.
	 */
	async #runDatagrams(options: {
		requestId: bigint;
		track: TrackSubscriber;
		/** The units datagrams are stamped in, if any; see {@link RunGroup.timescale}. */
		timescale: Timescale | undefined;
		publisherPriority: number;
		range: ServeRange;
	}) {
		const writer = this.#datagramWriter;
		if (!writer) return;
		const { requestId, track, timescale, publisherPriority, range } = options;
		const version = this.#session.version;

		try {
			for (;;) {
				const datagram = await track.recvDatagram();
				if (!datagram) return;
				// A model datagram is object 0 of its group, subject to the reliable slice.
				if (groupSlice(range, datagram.sequence).skip > 0) continue;

				const body = await encodeDatagram(
					datagram,
					{ trackAlias: requestId, publisherPriority, timescale },
					version,
				);
				const max = DatagramStream.maxDatagramSize(this.#quic);
				if (body.byteLength > max) {
					console.debug(`dropping oversize datagram: alias=${requestId} size=${body.byteLength} max=${max}`);
					continue;
				}

				await writer.ready;
				await writer.write(body);
			}
		} catch (err: unknown) {
			console.debug(`datagram send stopped: alias=${requestId} error=${reason(error(err))}`);
		}
	}

	/** Preserve the original terminal on a separate stream when its group already sent FIN. */
	async #runEndOfTrack(options: {
		requestId: bigint;
		final: TrackPosition;
		publisherPriority: number;
		unsubscribed: Promise<void>;
		streams: StreamCount;
	}) {
		const { requestId, final, publisherPriority, unsubscribed, streams } = options;
		const version = this.#session.version;
		const stream = await Writer.tryOpen(this.#quic, { cancel: unsubscribed, version }).catch(() => undefined);
		if (!stream) return;
		streams.opened += 1;

		try {
			const header = new GroupMessage({
				trackAlias: requestId,
				groupId: final.group,
				subGroupId: 0,
				publisherPriority,
				flags: {
					hasExtensions: false,
					hasSubgroup: false,
					hasSubgroupObject: false,
					hasEnd: false,
					hasPriority: true,
					firstObject: final.frame === 0,
				},
			});
			await header.encode(stream, version);
			await new Frame({ endOfTrack: true }).encode(stream, header.flags, Timescale.MILLI, version, final.frame);
			stream.close();
		} catch (err: unknown) {
			stream.reset(error(err));
		}
	}

	/**
	 * Serve the largest group's head on its own draft-22 fetch stream.
	 *
	 * A fill is a promise once requested. An empty range opens no stream, but a range we
	 * cannot serve still opens one and resets it right after the FETCH_HEADER, the draft's
	 * fill-failure signal. Nothing here touches the subscription either way.
	 */
	async #runFill(options: RunFill) {
		const { requestId, ranking, fill, cache, timescale, unsubscribed, cancelled, streams } = options;
		const version = this.#session.version;

		// Everything is inside the try so the cache fork is released on every path out,
		// including a transport failure while opening the stream.
		let stream: Writer | undefined;
		try {
			if (fill.kind === "empty") return;

			// A fill we cannot serve is reset right after its header, so it needs no rank.
			const sequence = fill.kind === "group" ? Number(fill.start.group) : undefined;
			const object = fill.kind === "group" ? Number(fill.start.object) : 0;
			stream = await Writer.tryOpen(this.#quic, {
				cancel: unsubscribed,
				version,
				sendOrder: sequence === undefined ? undefined : ranking.rank(sequence, object),
			});
			if (!stream) {
				console.debug(`fill stream failed to open: fill=${requestId}`);
				return;
			}
			streams.opened += 1;
			if (sequence !== undefined) ranking.add(stream, sequence, object);

			await untilAborted(new FetchHeader({ requestId }).encode(stream, version), cancelled);

			if (fill.kind !== "group") {
				throw new Error("fill is not supported");
			}

			const out = new FetchWriter({ stream, version, timescale });
			const group = hooks.cachedGroup(cache, Number(fill.start.group));
			if (!group) throw new NotFound("fill group unavailable");
			try {
				await this.#writeFillGroup({ out, group, slice: groupSlice(fill, group.sequence), cancelled });
			} finally {
				group.close();
			}

			stream.close();
			console.debug(`fill complete: fill=${requestId}`);
		} catch (err: unknown) {
			// A fill never fails the subscription; its own stream carries the failure.
			const e = error(err);
			console.debug(`fill failed, resetting its stream: fill=${requestId} error=${reason(e)}`);
			stream?.reset(e);
		} finally {
			// Ranked until it settles, like a group stream.
			if (stream) {
				const opened = stream;
				void opened.closed.catch(() => {}).then(() => ranking.remove(opened));
			}
			cache.close();
		}
	}

	/**
	 * Write the largest group's cached head as fetch objects.
	 *
	 * The cap is the Largest Object snapshot: the group may keep growing, but everything
	 * past the snapshot belongs to the subscription, not the fill.
	 */
	async #writeFillGroup({
		out,
		group,
		slice,
		cancelled,
	}: {
		out: FetchWriter;
		group: group.Consumer;
		slice: GroupSlice;
		cancelled: AbortSignal;
	}): Promise<void> {
		let next = slice.skip;

		// The subscriber leaving has to end the fill too. An absolute filter ending below the
		// edge with no end object leaves `until` unset, so this reads until the group closes,
		// which a still-open past group may never do. Watching only the fetch stream would
		// then pin the group and the cache subscription behind it for the life of the track.
		for (;;) {
			// The group may keep growing, but everything at `until` and beyond belongs to the
			// subscription, so stop without waiting for the group's end.
			if (slice.until !== undefined && next >= slice.until) break;

			// Reading from the fill's start drops everything below it; see the same read in #runGroup.
			const frame = await nextFrame(group, slice.skip, cancelled, out.stream.closed);
			if (!frame) break;
			next = frame.sequence + 1;
			if (slice.until !== undefined && frame.sequence >= slice.until) break;
			await out.object(group.sequence, frame, cancelled);
		}
	}

	/**
	 * Close the session when our origin publishes a broadcast our grant does not cover;
	 * see {@link enforceGrant}.
	 *
	 * @internal
	 */
	async runEnforce(setupAnswered: Promise<void>, auth?: AuthSession): Promise<void> {
		await enforceGrant({ quic: this.#quic, advertised: this.#advertised, grant: this.#grant, setupAnswered, auth });
	}

	/**
	 * Handles an incoming SUBSCRIBE_NAMESPACE on a bidi stream.
	 *
	 * On draft-16 and later every visible match is a NAMESPACE on this stream, whatever
	 * the peer's SETUP said. A NAMESPACE is discovery, not a route, so a peer that also
	 * hears the unsolicited PUBLISH_NAMESPACE still gets the match here.
	 *
	 * @internal
	 */
	runSubscribeNamespace(msg: SubscribeNamespace, stream: Stream): Promise<void> {
		const request = new RequestStream(stream, this.#session, this.#quic);
		return this.#withdrawal.track(request.serve(this.#runSubscribeNamespace(msg, request)));
	}

	async #runSubscribeNamespace(msg: SubscribeNamespace, request: RequestStream) {
		if (Namespace.reserved(msg.namespace, this.#session.version)) {
			await refuseReserved(request);
			return;
		}
		if (this.#withdrawal.closing.peek()) return;
		const version = this.#session.version;
		let prefix = msg.namespace;

		const ending = request.ended;
		const readUpdate = () =>
			(async () => {
				const framed = await request.next();
				return { update: framed && (await namespaceUpdate(framed, version)) };
			})().catch((err: unknown) => ({ error: error(err) }));
		let updating: ReturnType<typeof readUpdate> | undefined = readUpdate();
		try {
			const answer = async (activeCount?: number) => {
				const parameters = new Parameters();
				parameters.activeCount = activeCount;
				await Message.write(request.writer, RequestOk.id, new RequestOk({ parameters }), version);
			};

			// MoQ Active Count: the OK counts what the first pass below sends, so it waits for
			// that pass. Otherwise answer now.
			let answered = !this.#activeCount;
			if (answered) await answer();

			// Hidden namespaces are left out unless the peer opted in (MoQ Hidden).
			const visible = (covered: Path.Valid) => !this.#hidden || msg.hidden || !hiddenBelow(prefix, covered);
			const carries = (covered: Path.Valid) => visible(covered) && this.#gate.allows(covered);

			// Nothing is advertised before the grant it would be checked against.
			await this.#ready;

			// Inline entries always land, and the receiver treats a repeated NAMESPACE as a
			// replacement, so repricing one is just sending it again, a new original
			// publisher included, as Rust does.
			const namespace = async (suffix: Path.Valid, snap: Advertised): Promise<Answer> => {
				// Either extension puts the Parameters field on NAMESPACE, even an empty one.
				const params = this.#advert !== undefined || this.#epoch ? this.#params(snap.route) : undefined;
				const entry = new SubscribeNamespaceEntry({ suffix, params });
				await Message.write(request.writer, SubscribeNamespaceEntry.id, entry, version);
				return "held";
			};
			const target: Target = {
				advertise: namespace,
				// A repeat under another epoch replaces the instance in place.
				replace: namespace,
				update: namespace,
				held: () => undefined,
				carry: () => undefined,
				withdraw: async (suffix) => {
					const done = new SubscribeNamespaceEntryDone({ suffix });
					await Message.write(request.writer, SubscribeNamespaceEntryDone.id, done, version);
				},
			};

			// Keyed by suffix, as the entries name them.
			const ns: Namespaces = { active: new Map(), refused: new Map(), offered: new Map() };
			let retry = 0;

			for (;;) {
				// Subscribe BEFORE reconciling, for the same reason as
				// {@link runPublishNamespaces}: a publish landing while an advertisement
				// waits for its reply only notifies listeners already registered.
				// A grant change re-diffs the same way, withdrawing what it no longer covers.
				// The loop top re-reads the table, so an ended origin still stops it there.
				// A token that ended leaves the request carrying it to be made again without.
				const { changed, dispose } = nextChange(this.#advertised, this.#grant, this.#auth?.ended);

				const advertised = this.#advertised.peek();
				const updated = advertised ? presented(prefix, advertised, carries) : new Map<Path.Valid, Advertised>();
				// A fresh pass sends a NAMESPACE for every entry, ahead of any change.
				if (!answered) {
					answered = true;
					await answer(updated.size);
				}
				if (!advertised) {
					dispose();
					break;
				}

				const outstanding = await this.#reconcile(ns, updated, target);

				retry = outstanding ? Math.min(retry ? retry * 2 : RETRY_BASE, RETRY_MAX) : 0;

				// Wait for the next change, or for the peer to unsubscribe.
				const next = await (retry
					? race([
							changed,
							this.#withdrawal.closing,
							ending,
							...(updating !== undefined ? [updating] : []),
							retryAfter(retry).then(() => advertised),
						])
					: race([changed, this.#withdrawal.closing, ending, ...(updating !== undefined ? [updating] : [])]));
				dispose();
				if (!next || next === true) break;
				if (typeof next === "object" && "error" in next) throw next.error;
				if (typeof next === "object" && "update" in next) {
					const update = next.update;
					updating = undefined;
					if (!update) continue;
					this.#session.checkRequestId(update.requestId);
					if (update.refused || (update.prefix !== undefined && Namespace.reserved(update.prefix, version))) {
						await refuseUpdate(request.writer, version, "subscribe_namespace", {
							condition: update.refused ?? "does_not_exist",
							reasonPhrase: "namespace update refused",
						});
						break;
					}
					if (update.prefix !== undefined && update.prefix !== prefix) {
						for (const suffix of ns.active.keys()) await target.withdraw(suffix);
						ns.active.clear();
						ns.offered.clear();
						ns.refused.clear();
						prefix = update.prefix;
						// A new cursor starts its backoff fresh, as in Rust.
						retry = 0;
					}
					await Message.write(request.writer, RequestOk.id, new RequestOk(), version);
					updating = readUpdate();
				}
			}

			request.close();
			await request.writer.closed;
		} catch (err: unknown) {
			const e = error(err);
			console.debug(`subscribe_namespace stream error: ${reason(e)}`);
			request.abort(e);
			if (e instanceof ProtocolViolation || this.#withdrawal.closing.peek()) throw e;
		} finally {
			// Closing stopped the read, which settles it.
			await updating;
		}
	}

	/**
	 * Advertise every published broadcast with an unsolicited PUBLISH_NAMESPACE, until
	 * the publisher is closed.
	 *
	 * The peers that never send SUBSCRIBE_NAMESPACE are exactly the ones expecting a
	 * publisher to announce itself, so announcing is the default. A peer that would
	 * rather ask says so in its SETUP (MoQ Solicit) and this does nothing.
	 * {@link runSubscribeNamespace} also carries every match, so a peer that
	 * did not ask hears each namespace both ways. A NAMESPACE is discovery only, not a
	 * second route.
	 *
	 * @internal
	 */
	runPublishNamespaces(): Promise<void> {
		return this.#withdrawal.track(this.#runPublishNamespaces());
	}

	async #runPublishNamespaces() {
		if (this.#withdrawal.closing.peek()) return;
		if (this.#requiresSolicitation) {
			// The peer asked to be told on request; runSubscribeNamespace answers it.
			return;
		}

		// The open PUBLISH_NAMESPACE request per advertised path.
		const requests: Requests = new Map();
		const target: Target = {
			advertise: (path, snap) => this.#advertise(path, requests, snap.route),
			replace: (path, snap) => this.#replace(path, requests, snap.route),
			update: (path, _snap, reprice) => this.#update(path, requests, reprice),
			withdraw: (path) => this.#withdraw(path, requests),
			held: (path) => requests.get(path)?.auth,
			carry: (auth) => this.#carry(auth),
		};

		// The origin outlives the session and its signal never ends, so this loop needs the
		// session's own ending to stop: without it a closed connection leaves the loop parked
		// on a shared origin forever, waking on someone else's publish to fail on a dead
		// transport. `runSubscribeNamespace` gets this from the stream the peer asked on; an
		// unsolicited loop has no stream of its own, so the session is what it watches.
		const closed = this.#quic.closed.then(
			() => undefined,
			() => undefined,
		);

		let dispose: Dispose | undefined;
		try {
			// Nothing is advertised before the grant it would be checked against.
			if ((await Promise.race([this.#ready.then(() => "ready" as const), closed])) !== "ready") return;

			// Keyed by path.
			const ns: Namespaces = { active: new Map(), refused: new Map(), offered: new Map() };
			// How long to stay away the next time the peer ends each advertisement while idle,
			// and the instance that wait is about.
			const lapses = new Map<Path.Valid, { snap: Advertised; wait: number }>();
			let retry = 0;

			for (;;) {
				// Subscribe BEFORE reconciling. Each advertisement below waits a round trip
				// for the peer's reply, and a publish that lands in that window notifies
				// only the listeners already registered; one created afterwards would sleep
				// through it and leave the namespace unadvertised until something unrelated
				// changed. A grant change re-diffs the same way, withdrawing what it no longer
				// covers; the loop top re-reads the table, so an ended origin still stops it there.
				// A token that ended leaves the request carrying it to be made again without.
				const change = nextChange(this.#advertised, this.#grant, this.#auth?.ended, this.#lapsed);
				dispose = change.dispose;

				// The peer no longer holds a namespace whose request ended while idle. The wait
				// before offering it again doubles each time, so a peer that ends every one it
				// accepts sees them on a backoff rather than in a loop.
				for (const [key, snap] of [...ns.active]) {
					if (requests.has(key)) continue;
					ns.active.delete(key);
					const wait = lapses.get(key)?.wait ?? RETRY_BASE;
					lapses.set(key, { snap, wait: Math.min(wait * 2, RETRY_MAX) });
					ns.refused.set(key, Instant.add(now(), Nano.fromMilli(Milli(wait))));
					ns.offered.set(key, snap);
				}

				const advertised = this.#advertised.peek();
				if (!advertised) {
					dispose?.();
					break;
				}

				const updated = new Map<Path.Valid, Advertised>();
				for (const [covered, candidates] of advertised) {
					// Only peers that declared MoQ Hidden filter unsolicited discovery, and
					// nothing our grant does not cover reaches the peer at all (MoQ Auth).
					if (
						(this.#hidden && hiddenBelow(Path.empty(), covered)) ||
						!this.#gate.allows(covered) ||
						candidates.length === 0
					)
						continue;
					updated.set(covered, candidates[0]);
				}
				// Another broadcast at the path starts its backoff fresh, as a refusal does.
				for (const [key, lapse] of lapses) {
					const snap = updated.get(key);
					if (snap === undefined || !sameInstance(lapse.snap, snap)) lapses.delete(key);
				}

				const outstanding = await this.#reconcile(ns, updated, target);
				retry = outstanding ? Math.min(retry ? retry * 2 : RETRY_BASE, RETRY_MAX) : 0;

				// Wait for the next change, which has already fired if one landed above.
				const next = await (retry
					? race([change.changed, this.#withdrawal.closing, closed, retryAfter(retry).then(() => advertised)])
					: race([change.changed, this.#withdrawal.closing, closed]));
				dispose?.();
				if (!next || next === true) break;
			}
		} catch (err: unknown) {
			// Nothing restarts this loop, so whatever got us here cost the session its
			// discovery. Not a debug-level event.
			console.warn(`publish_namespace loop failed: ${reason(error(err))}`);
			if (this.#withdrawal.closing.peek()) throw err;
		} finally {
			dispose?.();
			// Close out every open PUBLISH_NAMESPACE request.
			for (const path of [...requests.keys()]) {
				await this.#withdraw(path, requests);
			}
		}
	}

	/**
	 * Bring the peer's view in line with `updated`, one namespace at a time, and report
	 * whether anything it should hold is still missing and worth coming back for.
	 *
	 * Nothing the peer cannot see is sent, a reprice updates the namespace in place, another
	 * epoch replaces it in place (MoQ Epoch), and anything else is withdrawn first so a
	 * republish reads as withdraw-then-advertise.
	 * `ns` records what the peer holds rather than what we attempted: a declined
	 * advertisement or update stays out of it, so the next turn offers it fresh instead
	 * of believing it is already up.
	 */
	async #reconcile(ns: Namespaces, updated: Map<Path.Valid, Advertised>, target: Target): Promise<boolean> {
		// A namespace that is gone, or that a republish replaced, takes its refusal with
		// it: the peer refused a broadcast, not a path forever, so a different one at the
		// same path is offered again. The origin swaps the front in one mutation, so the
		// path never leaves `updated` and only the front says they differ. Rust gets this
		// by rebuilding the watched entry.
		for (const key of [...ns.refused.keys()]) {
			const snap = updated.get(key);
			const offered = ns.offered.get(key);
			// Another publisher instance is another broadcast too.
			if (snap === undefined || offered === undefined || !sameInstance(offered, snap)) {
				ns.refused.delete(key);
				ns.offered.delete(key);
			}
		}

		const held = new Map(ns.active);
		for (const [key, snap] of ns.active) {
			const next = updated.get(key);
			if (this.#change(key, snap, next, target) !== "restart" || this.#replaces(snap, next)) continue;
			await target.withdraw(key);
			held.delete(key);
		}

		for (const [key, snap] of updated) {
			const prev = held.get(key);
			const diff = this.#change(key, prev, snap, target);
			if (diff === "same") continue;

			let answer: Answer;
			if (prev !== undefined && diff !== "restart") {
				answer = await target.update(key, snap, diff);
			} else if (prev !== undefined) {
				// Still held through a restart: another epoch replacing it.
				answer = await target.replace(key, snap);
			} else {
				// A fresh offer waits for what a refusal asked for, whatever brought us back.
				if (!this.#offerable(key, ns.refused)) continue;
				answer = await target.advertise(key, snap);
			}
			// `offered` pairs with `refused`, so a held or withdrawn path leaves nothing behind.
			if (answer === "held") {
				held.set(key, snap);
				ns.refused.delete(key);
				ns.offered.delete(key);
			} else {
				held.delete(key);
				if (answer !== "dropped") {
					ns.refused.set(key, answer);
					ns.offered.set(key, snap);
				}
			}
		}

		ns.active = held;

		// Keep the backoff only for a peer refusal that permits another offer. Local
		// credit failures and unanswered requests are terminal for this broadcast instance.
		return [...updated].some(
			([key, snap]) =>
				this.#change(key, held.get(key), snap, target) !== "same" && this.#pending(key, ns.refused),
		);
	}

	/**
	 * What it takes to move the peer from `held` to `next` at `key`: {@link change}, plus
	 * the token the request carries.
	 *
	 * moq-transport renames a request's token with REQUEST_UPDATE, but cannot name one on a
	 * request that had none, nor drop one: those withdraw the request and make it again.
	 */
	#change(
		key: Path.Valid,
		held: Advertised | undefined,
		next: Advertised | undefined,
		target: Target,
	): "same" | "restart" | Reprice {
		const diff = change(this.#advert, held, next);
		if (diff === "restart" || held === undefined || next === undefined) return diff;
		const from = target.held(key);
		const to = target.carry(next.route.auth);
		if (from === to) return diff;
		if (from === undefined || to === undefined) return "restart";
		return { ...(diff === "same" ? {} : diff), token: to };
	}

	/**
	 * Whether `next` replaces the instance the peer holds in `held` with another, both named
	 * by an epoch on the wire (MoQ Epoch). Gaining or losing one is withdrawn and advertised
	 * afresh, and so is any instance change on a peer that cannot tell them apart.
	 */
	#replaces(held: Advertised, next: Advertised | undefined): boolean {
		const [from, to] = [held.route.epoch, next?.route.epoch];
		return this.#epoch && from !== undefined && to !== undefined && from !== to;
	}

	/** `auth` when a request of this session can carry it. */
	#carry(auth: Credential | undefined): Credential | undefined {
		return this.#auth?.carried(auth) === undefined ? undefined : auth;
	}

	/**
	 * Whether a namespace may be offered to the peer right now.
	 *
	 * A peer that asked never to be offered it again means it, whatever brought us back;
	 * one that named a minimum wait gets it, even when our own backoff comes round sooner.
	 */
	#offerable(key: Path.Valid, refused: Map<Path.Valid, Refused>): boolean {
		const entry = refused.get(key);
		if (entry === undefined) return true;
		return entry !== "never" && now() >= entry;
	}

	/**
	 * Whether the loop should keep coming back to a namespace the peer does not hold.
	 *
	 * Distinct from {@link offerable}, and the difference is what arms the retry: a
	 * namespace waiting out a minimum is not offerable yet but is still pending, and
	 * gating the timer on offerable instead would disarm it for exactly the wait it is
	 * supposed to be counting. A terminal failure or a refusal that forbids retrying ends it.
	 */
	#pending(key: Path.Valid, refused: Map<Path.Valid, Refused>): boolean {
		return refused.get(key) !== "never";
	}

	/**
	 * What a REQUEST_ERROR's retry interval, in milliseconds, says about re-offering the
	 * namespace: 0 means the peer does not want this again.
	 */
	#refusal(err: RequestError): Answer {
		return err.retryInterval === 0n
			? "never"
			: Instant.add(now(), Nano.fromMilli(Milli(Number(err.retryInterval))));
	}

	/**
	 * The extension parameters an advertisement of `route` carries. The epoch is forwarded
	 * as the route names it, and never invented for one without.
	 */
	#params(route: Route): AdvertParams {
		return { cluster: clusterFor(this.#advert, route), epoch: this.#epoch ? route.epoch : undefined };
	}

	/**
	 * Advertise one namespace on its own PUBLISH_NAMESPACE request. A declined request
	 * is logged and skipped: a peer that wants none of this rejects each one and stays
	 * connected.
	 */
	async #advertise(path: Path.Valid, requests: Requests, route: Route): Promise<Answer> {
		const params = this.#params(route);
		const auth = this.#carry(route.auth);
		const requestId = await this.#session.nextRequestId();
		if (requestId === undefined) return "dropped";

		// Opening is inside the try because it fails like any other advertisement does:
		// a peer out of stream credit rejects here, and letting that escape would unwind
		// the announce loop and take discovery down for the whole session, rather than
		// leaving this one namespace unadvertised.
		let request: RequestStream | undefined;
		try {
			const deadline = new Deadline();
			const answer = await deadline.wait(
				(async (): Promise<Answer> => {
					const stream = await deadline.open(this.#session.openBi(requestOptions));
					const opened = new RequestStream(stream, this.#session, this.#quic);
					request = opened;
					const version = this.#session.version;
					const token = this.#auth?.carried(auth);
					const msg = new PublishNamespace({ requestId, trackNamespace: path, params, token });
					await deadline.send(stream, async () => {
						await Message.write(opened.writer, PublishNamespace.id, msg, version);
					});

					// Read response
					const response = await opened.next();
					if (response === undefined) throw new Error("advertisement ended before its response");
					if (response.type === RequestError.id) {
						const err = await RequestError.decode(response.reader, this.#session.version);
						const why = requestReason(
							err.errorCode,
							err.reasonPhrase,
							"publish_namespace",
							this.#session.version,
						);
						console.warn(`announce refused: broadcast=${path} error=${why}`);
						opened.abort(new Error(`PublishNamespace rejected: ${why}`));
						return this.#refusal(err);
					}
					if (response.type !== RequestOk.id) {
						throw new Error(`PublishNamespace rejected: typeId=0x${response.type.toString(16)}`);
					}
					// Draft-14 sends PublishNamespaceOk (requestId only, no parameters)
					this.#answered(await RequestOk.decode(response.reader, this.#session.version));
					return "held";
				})(),
			);

			if (answer === "held" && request) {
				const held: NamespaceRequest = { path, requestId, stream: request, auth };
				requests.set(path, held);
				held.reply = this.#readAdvertisement(held, requests);
			}
			return answer;
		} catch (err: unknown) {
			// The peer never answered the advertisement: a control timeout, not late content.
			const e = err instanceof TimeoutError ? controlTimeout(err) : error(err);
			console.warn(`announce failed: broadcast=${path} error=${reason(e)}`);
			request?.abort(e);
			return e instanceof NoStreamCredit || (e instanceof StreamError && e.code === StreamCode.ControlTimeout)
				? "never"
				: "dropped";
		}
	}

	/**
	 * Keep reading while an advertisement is idle, so GOAWAY validation never waits for an
	 * update. A stream that ends while idle drops the request, and the announce loop re-offers it
	 * after a backoff.
	 */
	#readAdvertisement(request: NamespaceRequest, requests: Requests): Promise<Framed | undefined> {
		const reply = request.stream.next().then((framed) => {
			if (framed !== undefined && !request.updating)
				throw new ProtocolViolation("unexpected message on an idle advertisement");
			return framed;
		});
		// A later update consumes an answer; an end is acted on as soon as it arrives.
		void reply.then(
			(framed) => framed === undefined && this.#lapse(request, requests),
			(err: unknown) => {
				if (err instanceof ProtocolViolation) request.stream.abort(err);
				this.#lapse(request, requests);
			},
		);
		return reply;
	}

	/** Drop an idle advertisement whose stream ended, unless an update already took it over. */
	#lapse(request: NamespaceRequest, requests: Requests) {
		if (request.updating || requests.get(request.path) !== request) return;
		requests.delete(request.path);
		request.stream.abort(new StreamError(StreamCode.Cancel, { message: "advertisement ended" }));
		this.#lapsed.update((n) => n + 1);
	}

	/**
	 * Check a REQUEST_OK answering a PUBLISH_NAMESPACE. ACTIVE_COUNT only answers
	 * SUBSCRIBE_NAMESPACE (MoQ Active Count), and reading one too large to hold throws the
	 * same violation.
	 */
	#answered(ok: RequestOk) {
		if (ok.parameters.activeCount !== undefined)
			throw new ProtocolViolation("ACTIVE_COUNT on a PUBLISH_NAMESPACE answer");
	}

	/**
	 * Update a namespace the peer holds: REQUEST_UPDATE on the request that carries it,
	 * with only the parameters that changed, then its answer.
	 *
	 * Waiting for the answer keeps one update outstanding per stream, which satisfies any
	 * MAX_REQUEST_UPDATES the peer set without reading it; a change landing meanwhile waits
	 * for the next turn. The answer decides whether the peer still holds the namespace: a
	 * REQUEST_ERROR closes the peer's side and withdraws the advertisement, so ours is
	 * finished too, and an unanswered update is dropped, since a peer that
	 * ignored it cannot be assumed to hold either price. Either way the retry re-offers it fresh.
	 *
	 * A different original publisher is an update like any other: the receiver drains what
	 * it already serves from the old one and never splices the two.
	 */
	async #update(path: Path.Valid, requests: Requests, { from, to, token: auth }: Reprice): Promise<Answer> {
		const request = requests.get(path);
		if (!request) return "dropped";

		const update: Cluster.Update = {
			hops: from && to && !sameHops(from.hops, to.hops) ? to.hops : undefined,
			cost: from && to && from.cost !== to.cost ? to.cost : undefined,
		};
		const token = auth && this.#auth?.carried(auth);
		const version = this.#session.version;
		let answer: Answer;
		try {
			const requestId = await this.#session.nextRequestId();
			if (requestId === undefined) throw new Error("no request ID for the update");

			answer = await withTimeout(
				(async (): Promise<Answer> => {
					const writer = request.stream.writer;
					request.updating = true;
					const msg = new PublishNamespaceUpdate({ requestId, update, token });
					await Message.write(writer, PublishNamespaceUpdate.id, msg, version);

					const response = await (request.reply ?? request.stream.next());
					if (response === undefined) throw new Error("advertisement ended before its update response");
					if (response.type === RequestError.id) {
						const err = await RequestError.decode(response.reader, version);
						const why = requestReason(err.errorCode, err.reasonPhrase, "publish_namespace", version);
						console.warn(`publish_namespace update refused: broadcast=${path} error=${why}`);
						return this.#refusal(err);
					}
					if (response.type !== RequestOk.id) {
						throw new Error(`unexpected answer to the update: typeId=0x${response.type.toString(16)}`);
					}
					this.#answered(await RequestOk.decode(response.reader, version));
					request.updating = false;
					request.reply = this.#readAdvertisement(request, requests);
					return "held";
				})(),
				ADVERTISE_TIMEOUT,
				`update timed out after ${ADVERTISE_TIMEOUT_MS}ms waiting for the peer's answer`,
			);
		} catch (err: unknown) {
			const e = err instanceof TimeoutError ? controlTimeout(err) : error(err);
			console.warn(`publish_namespace update failed: broadcast=${path} error=${reason(e)}`);
			requests.delete(path);
			request.stream.abort(e);
			return "dropped";
		}

		if (answer === "held") {
			if (auth) request.auth = auth;
			return answer;
		}
		// The peer already closed out a refused request, and its STOP_SENDING can
		// reject the wait on our FIN. That is the withdrawal finishing, not a reason to end
		// the loop.
		await this.#withdraw(path, requests).catch(() => undefined);
		return answer;
	}

	/**
	 * Replace the PUBLISH_NAMESPACE request for `path` with one for another instance. The two
	 * streams are unordered, so the old one is withdrawn only once the peer answered the new
	 * one, refused or not: it serves another instance either way.
	 */
	async #replace(path: Path.Valid, requests: Requests, route: Route): Promise<Answer> {
		const old = requests.get(path);
		requests.delete(path);
		const answer = await this.#advertise(path, requests, route);
		if (old) await this.#end(old);
		return answer;
	}

	async #withdraw(path: Path.Valid, requests: Requests) {
		const request = requests.get(path);
		if (!request) return;
		requests.delete(path);
		await this.#end(request);
	}

	/**
	 * Withdraw one PUBLISH_NAMESPACE request, already out of its map, by cancelling it in both
	 * directions: a FIN is only allowed to end SUBSCRIBE_NAMESPACE and SUBSCRIBE_TRACKS.
	 */
	async #end(request: NamespaceRequest) {
		request.stream.abort(new StreamError(StreamCode.Cancel, { message: "withdrawn" }));
	}

	/**
	 * Answers an incoming TRACK_STATUS with what a SUBSCRIBE_OK for the track would carry,
	 * without subscribing.
	 *
	 * The track resolves as it would for a SUBSCRIBE, dynamic handlers included, and the
	 * answer waits on its info. The Largest Location is whatever the cache holds now.
	 *
	 * @internal
	 */
	runTrackStatusRequest(msg: TrackStatusRequest, stream: Stream): Promise<void> {
		const request = new RequestStream(stream, this.#session, this.#quic);
		return request.serve(this.#runTrackStatusRequest(msg, request));
	}

	async #runTrackStatusRequest(msg: TrackStatusRequest, request: RequestStream) {
		this.#pinned(msg.epoch);
		if (Namespace.reserved(msg.trackNamespace, this.#session.version)) {
			await refuseReserved(request);
			return;
		}
		const version = this.#session.version;
		const name = msg.trackNamespace;
		const denied: Refusal = { condition: "unauthorized", reasonPhrase: "not granted" };

		// Answer only for what our grant lets us publish. Checked before the track resolves,
		// so a denied request never reaches the origin.
		if (!this.#gate.allows(name)) {
			await request.refuse("track_status", denied);
			return;
		}

		// Holds the track wanted until the answer is out. The requester leaving is owed
		// nothing, and lets go of the broadcast and track lookups.
		const hold = new AbortController();
		const cancel = new StreamError(StreamCode.Cancel, { message: "cancel" });
		void request.ended.then(() => hold.abort(cancel));
		// Nothing may follow the request.
		void request.next().then(
			(framed) => framed && request.abort(new ProtocolViolation("unexpected request stream message")),
			// The request ended, which the hold already heard.
			() => undefined,
		);

		// A grant that narrows abandons the lookup, and resets an answer the transport has
		// not taken yet rather than delivering it.
		let answering = false;
		const watch = this.#gate.watch(name, () => {
			hold.abort();
			if (answering) request.abort(unauthorized(name));
		});

		try {
			let answer: TrackStatusOk;
			try {
				const broadcast = await this.#resolve(name, hold.signal, msg.epoch);
				const info = await wireOf(broadcast).resolveTrackInfo(msg.trackName, hold.signal);
				const largest = wireOf(broadcast).largest(msg.trackName);
				answer = new TrackStatusOk({
					largest: largest && { groupId: BigInt(largest.group), objectId: BigInt(largest.frame) },
					properties: trackProperties(info, msg.propertiesWanted),
				});
			} catch (err: unknown) {
				if (hold.signal.reason === cancel) {
					request.abort(cancel);
					return;
				}
				const refusal = !this.#gate.allows(name) ? denied : this.#failed(err);
				await request.refuse("track_status", refusal);
				return;
			}

			answering = true;
			await Message.write(request.writer, TrackStatusOk.id, answer, version);
			// The answer is all this stream carries.
			request.close();
			await request.writer.closed.catch(() => undefined);
		} finally {
			watch.dispose();
			hold.abort();
		}
	}

	/** Answer a request to discover tracks under a namespace prefix. @internal */
	runSubscribeTracks(msg: SubscribeTracks, stream: Stream): Promise<void> {
		const request = new RequestStream(stream, this.#session, this.#quic);
		return request.serve(this.#runSubscribeTracks(msg, request));
	}

	async #runSubscribeTracks(msg: SubscribeTracks, request: RequestStream) {
		const version = this.#session.version;
		if (Namespace.reserved(msg.prefix, version)) return refuseReserved(request);
		const params = msg.parameters;
		if (
			params.rangeFilters ||
			params.trackPropertyFilter ||
			(version === Version.DRAFT_18 && params.forward !== false)
		) {
			return request.refuse("subscribe_namespace", {
				condition: params.rangeFilters ? "invalid_filter" : "not_supported",
				reasonPhrase: params.rangeFilters
					? "Range Filters exceed MAX_FILTER_RANGES=0"
					: "SUBSCRIBE_TRACKS parameters not supported",
			});
		}
		if (
			[...this.#listings].some(
				(prefix) => Path.hasPrefix(prefix, msg.prefix) || Path.hasPrefix(msg.prefix, prefix),
			)
		) {
			await Message.write(
				request.writer,
				RequestError.id,
				new RequestError({ errorCode: 0x30, reasonPhrase: "overlapping SUBSCRIBE_TRACKS prefix" }),
				version,
			);
			return;
		}
		this.#listings.add(msg.prefix);
		const adding = new Signal(true);
		const skips = new Map<symbol, PublishSkipped>();
		const changed = new Signal(0);
		const tasks = new Set<Promise<void>>();
		const routes = new Map<Path.Valid, { snapshot: Advertised; hold: AbortController }>();
		const stop = () => {
			if (!adding.peek()) return;
			adding.set(false);
			this.#listings.delete(msg.prefix);
			skips.clear();
			changed.update((n) => n + 1);
		};
		void request.ended.then(stop);
		let refusal: Refusal | undefined;
		const closed = this.#quic.closed.then(
			() => "closed" as const,
			() => "closed" as const,
		);
		let updates: Promise<void> | undefined;
		let writing: Promise<void> | undefined;
		try {
			await Message.write(request.writer, RequestOk.id, new RequestOk({}), version);
			updates = (async () => {
				const framed = await request.next();
				if (!framed) return request.ended;
				if (framed.type !== 0x02) throw new ProtocolViolation("expected SUBSCRIBE_TRACKS update");
				await Message.decode(framed.reader, async (body) => {
					this.#session.checkRequestId(await body.u62());
					await Parameters.decode(body, version, "tracks-update");
				});
				refusal = { condition: "not_supported", reasonPhrase: "SUBSCRIBE_TRACKS updates are not supported" };
				stop();
			})().catch((err) => {
				if (err instanceof MalformedAuthToken) {
					refusal = { condition: "malformed_auth_token", reasonPhrase: err.message };
					stop();
				} else request.abort(malformed(err, "SUBSCRIBE_TRACKS update"));
			});
			writing = (async () => {
				while (adding.peek()) {
					const first = skips.entries().next().value;
					if (!first) {
						await Signal.race(changed, adding);
						continue;
					}
					skips.delete(first[0]);
					// A FORWARD=1 listing waits for this to read its next track.
					changed.update((n) => n + 1);
					await Message.write(request.writer, PublishSkipped.id, first[1], version);
				}
				if (refusal) await request.refuse("subscribe_namespace", refusal);
				else request.close();
			})().catch((err) => {
				request.abort(err);
			});
			await race([this.#ready, request.ended]);
			for (;;) {
				const table = this.#advertised.peek();
				const visible =
					table &&
					presented(
						msg.prefix,
						table,
						(path) => this.#gate.allows(path) && (!this.#hidden || !hiddenBelow(msg.prefix, path)),
					);
				for (const [suffix, route] of routes) {
					const next = visible?.get(suffix);
					if (!next || !sameInstance(route.snapshot, next)) {
						route.hold.abort();
						routes.delete(suffix);
					}
				}
				if (adding.peek() && visible)
					for (const [suffix, snapshot] of visible) {
						if (routes.has(suffix)) continue;
						const hold = new AbortController();
						const task = this.#listBroadcast({
							msg,
							suffix,
							epoch: snapshot.route.epoch,
							hold,
							adding,
							skips,
							changed,
						})
							.catch((err) => {
								if (err instanceof ProtocolViolation) request.abort(err);
							})
							.finally(() => {
								tasks.delete(task);
								changed.update((n) => n + 1);
							});
						tasks.add(task);
						routes.set(suffix, { snapshot, hold });
					}
				if (!adding.peek()) {
					// Sent PUBLISHes own their subscriptions, but still follow route withdrawal.
					if (!tasks.size) break;
					if ((await race([Signal.race(this.#advertised, this.#grant, changed), closed])) === "closed") break;
				} else {
					if ((await race([Signal.race(this.#advertised, this.#grant, adding), closed])) === "closed") break;
				}
				if (!table) break;
			}
		} finally {
			stop();
			for (const route of routes.values()) route.hold.abort();
			await writing;
			request.close();
			await Promise.all(tasks);
			await updates;
		}
	}

	/**
	 * List one broadcast under a SUBSCRIBE_TRACKS: PUBLISH or skip each of its tracks until its
	 * route goes or the broadcast ends, then let its PUBLISHes run to their end.
	 *
	 * A PUBLISH starts only once the peer's stream credit opens its stream, one staged at a time,
	 * so the PUBLISHes a peer holds are bounded by the credit it grants. The listing is still read
	 * meanwhile, since an unlisted track's PUBLISH_DONE is what returns that credit; the tracks
	 * read but not yet PUBLISHed wait in `pending`. A FORWARD=1 listing has nothing to unlist, so
	 * it reads its next track only once the request stream has taken the last skip.
	 */
	async #listBroadcast({
		msg,
		suffix,
		epoch,
		hold,
		adding,
		skips,
		changed,
	}: {
		msg: SubscribeTracks;
		suffix: Path.Valid;
		epoch?: Epoch.Valid;
		hold: AbortController;
		adding: Signal<boolean>;
		skips: Map<symbol, PublishSkipped>;
		changed: Signal<number>;
	}) {
		const forward = msg.parameters.forward !== false;
		const namespace = Path.join(msg.prefix, suffix);
		// The tracks waiting their turn for a PUBLISH, in listing order.
		const pending = new Map<string, Info>();
		// The next track to PUBLISH, waiting on the peer's stream credit.
		let staged: Staged | undefined;
		// The stream the next PUBLISH goes out on, once the peer's credit admits it.
		let opening: Promise<Woken> | undefined;
		// The listing's next event, read while anything else is awaited.
		let reading: Promise<Woken> | undefined;
		// Each sent PUBLISH's entry: setting it says the track is no longer listed.
		const listed = new Map<string, Once<Unlisted>>();
		// The tracks with a skip queued, so the broadcast's end cancels only its own.
		const skipped = new Map<string, symbol>();
		const tasks = new Set<Promise<void>>();
		const skip = (name: string) => {
			const key = Symbol();
			skipped.set(name, key);
			skips.set(key, new PublishSkipped(suffix, name));
			changed.update((n) => n + 1);
		};
		const unstage = () => {
			if (staged?.claim) this.#trackClaims.delete(staged.claim);
			staged = undefined;
		};
		// The PUBLISHes already sent still end when their track is unlisted, so the listing is
		// still read once cancelled, adding nothing.
		const cancel = adding.subscribe((active) => {
			if (active) return;
			pending.clear();
			unstage();
			if (!tasks.size) hold.abort();
		});
		let listing: broadcast.Tracks | undefined;
		try {
			if (!adding.peek()) return;
			const source = await this.#resolve(namespace, hold.signal, epoch);
			listing = source.tracks();
			for (;;) {
				// Stage the next pending track that will be PUBLISHed, skipping those that will
				// not, so a stream is opened only for a PUBLISH that goes out.
				for (const [name, info] of pending) {
					if (staged || !adding.peek()) break;
					pending.delete(name);
					// Draft-18 allows one subscription per track (section 5.1), so a track the peer
					// is subscribed to, by its SUBSCRIBE or a PUBLISH still alive, is skipped.
					// Claimed from now, so a SUBSCRIBE cannot slip in before the PUBLISH goes out.
					const claim =
						this.#session.version === Version.DRAFT_18 ? JSON.stringify([namespace, name]) : undefined;
					if (claim !== undefined) {
						if (this.#trackClaims.has(claim)) {
							skip(name);
							continue;
						}
						this.#trackClaims.set(claim, "publish");
					}
					staged = { name, info, claim };
				}
				if (staged && !opening)
					opening = Promise.resolve(this.#session.openBi({ timeout: null })).then(
						(stream): Woken => ({ kind: "opened", stream }),
						(err: unknown): Woken => ({ kind: "failed", err }),
					);
				const waits: PromiseLike<Woken>[] = [];
				if (opening) waits.push(opening);
				// A FORWARD=1 listing has nothing to unlist, so it reads its next track only once
				// the request stream has taken the last skip.
				if (forward && skips.size) waits.push(Signal.race(changed).then((): Woken => ({ kind: "changed" })));
				else {
					reading ??= listing.next().then((event): Woken => ({ kind: "track", event }));
					waits.push(reading);
				}
				let woken: Woken;
				try {
					woken = await untilAborted(race(waits), hold.signal);
				} catch (err) {
					// A closed broadcast's listing has already ended, after the tracks that left
					// before it, so it is read to that end whatever withdrew the broadcast: a
					// PUBLISH whose track finished ends with TRACK_ENDED. Nothing more goes out.
					if (source.closed.peek() === undefined) throw err;
					pending.clear();
					unstage();
					reading ??= listing.next().then((event): Woken => ({ kind: "track", event }));
					woken = await reading;
				}
				if (woken.kind === "changed") continue;
				if (woken.kind === "failed") {
					console.debug(`listed broadcast=${namespace}: PUBLISH stream did not open`, woken.err);
					break;
				}
				if (woken.kind === "opened") {
					opening = undefined;
					const next = staged;
					if (!next || !adding.peek()) {
						woken.stream.abort(new StreamError(StreamCode.Cancel, { message: "listing ended" }));
						continue;
					}
					staged = undefined;
					const unlisted = new Once<Unlisted>();
					listed.set(next.name, unlisted);
					const task = this.#runPublish({
						msg,
						namespace,
						source,
						stream: woken.stream,
						name: next.name,
						info: next.info,
						unlisted,
						adding,
					})
						.catch((err) => {
							if (err instanceof ProtocolViolation) throw err;
						})
						.finally(() => {
							if (next.claim) this.#trackClaims.delete(next.claim);
							tasks.delete(task);
							if (!adding.peek() && !tasks.size) hold.abort();
						});
					tasks.add(task);
					void task.catch(() => hold.abort());
					continue;
				}
				reading = undefined;
				const event = woken.event;
				if (!event) break;
				if (event.kind === "synced") continue;
				if (event.kind === "removed" || event.kind === "finished") {
					if (staged?.name === event.name) unstage();
					pending.delete(event.name);
					listed.get(event.name)?.set(event.kind);
					listed.delete(event.name);
					const key = skipped.get(event.name);
					if (key) skips.delete(key);
					skipped.delete(event.name);
					continue;
				}
				if (!adding.peek()) continue;
				if (forward) skip(event.name);
				else pending.set(event.name, event.info);
			}
		} finally {
			cancel();
			listing?.close();
			pending.clear();
			unstage();
			// A stream that opens from now on carries nothing.
			void opening?.then((woken) => {
				if (woken.kind === "opened")
					woken.stream.abort(new StreamError(StreamCode.Cancel, { message: "listing ended" }));
			});
			for (const entry of listed.values()) entry.set("removed");
			for (const key of skipped.values()) skips.delete(key);
			await Promise.all(tasks);
		}
	}

	async #runPublish({
		msg,
		namespace,
		source,
		stream,
		name,
		info,
		unlisted,
		adding,
	}: {
		msg: SubscribeTracks;
		namespace: Path.Valid;
		source: broadcast.Consumer;
		/** The PUBLISH's stream, opened once the peer's credit admitted it. */
		stream: Stream;
		name: string;
		info: Info;
		unlisted: Once<Unlisted>;
		adding: Signal<boolean>;
	}) {
		const version = this.#session.version;
		const hold = new AbortController();
		const stop = adding.subscribe((active) => {
			if (!active) hold.abort();
		});
		// The peer lists the track once its PUBLISH arrives, which may be before the write
		// returns, so a track that finishes from then on sees the PUBLISH through to its answer
		// and ends with TRACK_ENDED; a reset would fail the peer's listing.
		let publishing = false;
		const removed = unlisted.subscribe((how) => {
			if (how === "removed" || (how === "finished" && !publishing)) hold.abort();
		});
		const request = new RequestStream(stream, this.#session, this.#quic);
		const cancelled = () => request.abort(new StreamError(StreamCode.Cancel, { message: "listing ended" }));
		hold.signal.addEventListener("abort", cancelled);
		try {
			let peerEnded = false;
			const ended = request.ended.then(() => {
				peerEnded = true;
				hold.abort();
				return undefined;
			});
			const id = await this.#session.nextRequestId(hold.signal);
			if (id === undefined) return;
			const defaults = msg.parameters;
			const subscription = new Subscribe({
				requestId: id,
				trackNamespace: namespace,
				trackName: name,
				subscriberPriority: defaults.subscriberPriority ?? DEFAULT_WIRE,
				groupOrder: defaults.groupOrder === GroupOrder.Ascending ? "ascending" : "descending",
				filter: defaults.subscriptionFilter,
				propertiesWanted: defaults.includeProperties ?? true,
			});
			const parameters = new Parameters();
			if (Filter.isDraft22(version)) {
				parameters.subscriberPriority = subscription.subscriberPriority;
				parameters.groupOrder =
					subscription.groupOrder === "ascending" ? GroupOrder.Ascending : GroupOrder.Descending;
				parameters.subscriptionFilter = subscription.filter;
			}
			const largest = wireOf(source).largest(name);
			publishing = true;
			await Message.write(
				request.writer,
				Publish.id,
				new Publish({
					requestId: id,
					trackNamespace: namespace,
					trackName: name,
					trackAlias: id,
					groupOrder: GroupOrder.Descending,
					contentExists: !!largest,
					largest: largest && { groupId: BigInt(largest.group), objectId: BigInt(largest.frame) },
					forward: false,
					parameters,
					properties: subscription.propertiesWanted
						? {
								priority: toWire(info.priority),
								groupOrder: Properties.DESCENDING,
								timescale: info.timescale ?? undefined,
								maxCacheDuration: info.maxAge === undefined ? undefined : BigInt(info.maxAge),
							}
						: {},
				}),
				version,
			);
			hold.signal.removeEventListener("abort", cancelled);
			stop();
			// Once sent, the PUBLISH survives cancellation of its listing.
			const state: Published = { streams: { opened: 0 }, unlisted };
			const answer = await untilAborted(request.next(), hold.signal);
			publishing = false;
			if (!answer) throw new ProtocolViolation("PUBLISH ended without an answer");
			if (answer.type === RequestError.id) {
				await RequestError.decode(answer.reader, version).catch((err) => {
					throw malformed(err, "REQUEST_ERROR");
				});
				return;
			}
			if (answer.type !== RequestOk.id && !(version === Version.DRAFT_18 && answer.type === 0x1e))
				throw new ProtocolViolation("expected PUBLISH_OK");
			const params = await Message.decode(answer.reader, (body) =>
				Parameters.decode(body, version, "publish-ok"),
			).catch((err) => {
				throw malformed(err, "PUBLISH_OK");
			});
			if (params.vars.has(0x32n)) {
				await this.#publishFailed(request, state.streams);
				return;
			}
			this.#applyPublished(subscription, params);
			let forward = !Filter.isDraft22(version) && params.forward !== false;
			for (;;) {
				if (!this.#gate.allows(namespace) || unlisted.peek() !== undefined) {
					await Message.write(
						request.writer,
						PublishDone.id,
						new PublishDone({
							statusCode: !this.#gate.allows(namespace)
								? PublishDoneStatus.UNAUTHORIZED
								: unlisted.peek() === "finished"
									? PublishDoneStatus.TRACK_ENDED
									: PublishDoneStatus.INTERNAL_ERROR,
							streamCount: BigInt(state.streams.opened),
							reasonPhrase: "track no longer listed or granted",
						}),
						version,
					);
					return;
				}
				if (forward) {
					const fixed = subscribeRange(subscription, {}, version);
					const track = wireOf(source).subscribe(name, {
						priority: fromWire(subscription.subscriberPriority),
						order: subscription.groupOrder,
						maxDelay: Milli(subscription.maxAge ?? Varint.MAX_U53),
						groups: fixed.start ? rangeGroups(fixed) : undefined,
					});
					let ready: Info;
					try {
						ready = await untilAborted(track.info(), hold.signal);
					} catch (err) {
						track.close();
						if (peerEnded) return;
						if (state.answer) {
							await refuseUpdate(request.writer, version, "subscribe", this.#failed(err));
							await this.#publishFailed(request, state.streams);
						} else {
							await Message.write(
								request.writer,
								PublishDone.id,
								new PublishDone({
									statusCode: PublishDoneStatus.INTERNAL_ERROR,
									streamCount: BigInt(state.streams.opened),
									reasonPhrase: "resume failed",
								}),
								version,
							);
						}
						return;
					}
					const paused = await this.#forward({
						msg: subscription,
						request,
						track,
						info: ready,
						published: state,
					});
					if (!paused) return;
					await Message.write(request.writer, RequestOk.id, new RequestOk({}), version);
					forward = false;
				}
				const revoked = new Once<true>();
				const gate = this.#gate.watch(namespace, () => revoked.set(true));
				let next: Framed | undefined | true | Unlisted;
				try {
					next = await race([request.next(), unlisted, revoked, ended]);
				} finally {
					gate.dispose();
				}
				if (next === true || next === "finished" || next === "removed") {
					await Message.write(
						request.writer,
						PublishDone.id,
						new PublishDone({
							statusCode: revoked.peek()
								? PublishDoneStatus.UNAUTHORIZED
								: next === "finished"
									? PublishDoneStatus.TRACK_ENDED
									: PublishDoneStatus.INTERNAL_ERROR,
							streamCount: BigInt(state.streams.opened),
							reasonPhrase: "track no longer listed or granted",
						}),
						version,
					);
					return;
				}
				if (!next) return request.ended;
				if (next.type !== 0x02) throw new ProtocolViolation("expected PUBLISH REQUEST_UPDATE");
				let update: Parameters;
				let updateId: bigint;
				try {
					({ params: update, requestId: updateId } = await this.#subscriptionUpdate(next));
				} catch (err) {
					if (!(err instanceof MalformedAuthToken)) throw malformed(err, "REQUEST_UPDATE");
					await refuseUpdate(request.writer, version, "subscribe", {
						condition: "malformed_auth_token",
						reasonPhrase: err.message,
					});
					await this.#publishFailed(request, state.streams);
					return;
				}
				if (
					update.rangeFilters ||
					update.vars.has(0x32n) ||
					(update.fillParameters && update.forward !== true)
				) {
					await refuseUpdate(request.writer, version, "subscribe", {
						condition: update.rangeFilters ? "invalid_filter" : "not_supported",
						reasonPhrase: "PUBLISH update parameters not supported",
					});
					await this.#publishFailed(request, state.streams);
					return;
				}
				this.#applyPublished(subscription, update);
				forward = update.forward === true;
				state.answer = forward ? { requestId: updateId } : undefined;
				if (!forward) {
					await Message.write(request.writer, RequestOk.id, new RequestOk({}), version);
				}
			}
		} catch (err) {
			request.abort(err);
			throw err;
		} finally {
			stop();
			removed();
			hold.signal.removeEventListener("abort", cancelled);
			hold.abort();
			request.close();
		}
	}

	async #subscriptionUpdate(framed: Framed): Promise<{ params: Parameters; requestId: bigint }> {
		const version = this.#session.version;
		const update = await Message.decode(framed.reader, async (body) => {
			const requestId = await body.u62();
			this.#session.checkRequestId(requestId);
			const params = await Parameters.decode(body, version, "request-update");
			return { params, requestId };
		});
		if (update.params.trackPropertyFilter)
			throw new ProtocolViolation("TRACK_PROPERTY_FILTER is only allowed on SUBSCRIBE_TRACKS updates");
		return update;
	}

	/** End a subscription whose update failed. */
	async #publishFailed(request: RequestStream, streams: StreamCount) {
		await Message.write(
			request.writer,
			PublishDone.id,
			new PublishDone({
				statusCode: PublishDoneStatus.UPDATE_FAILED,
				streamCount: BigInt(streams.opened),
				reasonPhrase: "update failed",
			}),
			this.#session.version,
		);
	}

	#applyPublished(msg: Subscribe, params: Parameters) {
		msg.subscriberPriority = params.subscriberPriority ?? msg.subscriberPriority;
		msg.groupOrder =
			params.groupOrder === undefined
				? msg.groupOrder
				: params.groupOrder === GroupOrder.Ascending
					? "ascending"
					: "descending";
		msg.filter = params.subscriptionFilter ?? msg.filter;
		msg.maxAge = params.maxAge ?? msg.maxAge;
		try {
			msg.fill = params.fillParameters
				? Filter.decodeFill(params.fillParameters, this.#session.version)
				: undefined;
		} catch (err) {
			throw malformed(err, "FILL_PARAMETERS");
		}
	}

	/**
	 * Handles an incoming FETCH on a bidi stream: a range of the named track, or a joining
	 * FETCH's head of its subscription's group, answered from the cache.
	 *
	 * @internal
	 */
	runFetch(msg: Fetch, stream: Stream): Promise<void> {
		const request = new RequestStream(stream, this.#session, this.#quic);
		return request.serve(this.#runFetch(msg, request));
	}

	async #runFetch(msg: Fetch, request: RequestStream) {
		this.#pinned(msg.epoch);
		// FILL_TIMEOUT is accepted and ignored: a one-group read ends with the group.
		const version = this.#session.version;
		const type = msg.type;
		const named = type.kind === "standalone" || type.kind === "filtered" ? type : undefined;
		const joining = type.kind === "relativeJoining" || type.kind === "absoluteJoining" ? type : undefined;
		if (named && Namespace.reserved(named.trackNamespace, version)) {
			await refuseReserved(request);
			return;
		}
		// Serving a Range Filter unfiltered would deliver objects the requester excluded.
		if (msg.rangeFilters) {
			await request.refuse("fetch", {
				condition: "invalid_filter",
				reasonPhrase: "Range Filters exceed MAX_FILTER_RANGES=0",
			});
			return;
		}
		// A joining FETCH was authorized with its subscription; a standalone one is checked
		// here, so a denied request never reaches the origin.
		const denied: Refusal = { condition: "unauthorized", reasonPhrase: "not granted" };
		if (named && !this.#gate.allows(named.trackNamespace)) {
			await request.refuse("fetch", denied);
			return;
		}

		// Everything that ends the answer early: the requester leaving, our grant narrowing,
		// or the subscription a joining FETCH named ending before it resolved.
		const stop = new AbortController();
		const cancel = new StreamError(StreamCode.Cancel, { message: "cancel" });
		void request.ended.then(() => stop.abort(cancel));
		// Nothing may follow the request.
		void request.next().then(
			(framed) => framed && request.abort(new ProtocolViolation("unexpected message on a FETCH")),
			// The request ended, which the stop already heard.
			() => undefined,
		);

		let join: Joinable | undefined;
		if (joining) {
			let found: Joinable | Refusal;
			try {
				found = await this.#joinOf(joining.subscription, stop.signal);
			} catch (err: unknown) {
				if (stop.signal.reason !== cancel) throw err;
				request.abort(cancel);
				return;
			}
			if ("condition" in found) {
				await request.refuse("fetch", found);
				return;
			}
			join = found;
		}
		const namespace = named?.trackNamespace ?? (join?.kind === "join" ? join.namespace : undefined);
		// A resolved answer holds its head in memory, so a subscription that ends after it, as
		// a track finishing right after SUBSCRIBE_OK does, still gets the head it joined.
		let resolved = false;
		const lapsed = new Error("subscription ended");
		const lapse =
			joining &&
			this.#joinsChanged.subscribe(() => {
				if (!resolved && !this.#joins.has(joining.subscription)) stop.abort(lapsed);
			});
		const watch = namespace && this.#gate.watch(namespace, () => stop.abort(unauthorized(namespace)));
		if (namespace && watch?.revoked()) stop.abort(unauthorized(namespace));

		let answer: FetchAnswer | Refusal | undefined;
		try {
			try {
				if (named) answer = await this.#fetchRange(msg, named, stop.signal);
				else if (joining && join) answer = await this.#fetchJoin(joining, join, stop.signal);
				else throw new Error("unknown FETCH type");
			} catch (err: unknown) {
				if (stop.signal.reason === cancel) {
					request.abort(cancel);
					return;
				}
				answer =
					stop.signal.reason === lapsed
						? { condition: "invalid_joining_request_id", reasonPhrase: "subscription ended" }
						: stop.signal.aborted && namespace && !this.#gate.allows(namespace)
							? denied
							: this.#failed(err);
			}
			if ("condition" in answer) {
				await request.refuse("fetch", answer);
				return;
			}
			resolved = true;
			await this.#answerFetch(msg, request, answer, stop.signal);
		} finally {
			lapse?.();
			watch?.dispose();
			stop.abort();
			if (answer && "owned" in answer && answer.owned) answer.cache.close();
		}
	}

	/**
	 * Resolve a standalone or draft-22 FETCH against its track and read the highest group it
	 * holds, or the refusal to answer with.
	 *
	 * A whole-group request waits for the group to finish, so a downstream cache never
	 * mistakes a prefix for a complete group.
	 */
	async #fetchRange(
		msg: Fetch,
		named: Extract<FetchType, { kind: "standalone" | "filtered" }>,
		signal: AbortSignal,
	): Promise<FetchAnswer | Refusal> {
		const version = this.#session.version;
		// Every filter but a bounded absolute one ends at the Largest Object, which resolves
		// once the track does.
		let relative: Filter.Filter | undefined;
		let requested: FetchRange;
		if (named.kind === "standalone") {
			requested = { start: named.start, end: named.end };
		} else if (named.filter.kind === "absolute" && named.filter.endGroup !== undefined) {
			// The filter's End Object is inclusive, where a standalone one is the last object
			// plus one. Without one, both include the whole End Group.
			const { startGroup, startObject, endGroup, endObject } = named.filter;
			requested = {
				start: { group: startGroup, object: startObject },
				end: { group: endGroup, object: endObject === undefined ? 0n : endObject + 1n },
			};
		} else {
			relative = named.filter;
			// Placeholders until the filter resolves against the Largest Object.
			requested = { start: { group: 0n, object: 0n }, end: { group: 0n, object: 0n } };
		}
		// End Object 0 includes the entire End Group.
		const { start, end } = requested;
		if (
			!relative &&
			(start.group > end.group || (start.group === end.group && end.object !== 0n && start.object >= end.object))
		) {
			return { condition: "invalid_range", reasonPhrase: "empty range" };
		}

		if (!relative && start.group !== end.group)
			return { condition: "not_supported", reasonPhrase: "FETCH spans more than one group" };

		const broadcast = await this.#resolve(named.trackNamespace, signal, msg.epoch);
		const priority = fromWire(msg.subscriberPriority);
		const cache = broadcast.track(named.trackName).subscribe({ priority, maxDelay: Milli(Varint.MAX_U53) });
		let answer: FetchAnswer | undefined;
		try {
			const info = await untilAborted(cache.info(), signal);
			const position = hooks.objectLargest(cache);
			const largest = position && { group: BigInt(position.group), object: BigInt(position.frame) };
			const range = { ...requested };
			if (relative) {
				if (!largest) {
					return {
						condition: "not_supported",
						reasonPhrase: "FETCH relative to Largest Object needs a known publisher edge",
					};
				}
				range.start = fetchStart(relative, largest);
				range.end = { group: largest.group, object: 0n };
			}
			if (range.start.group !== range.end.group)
				return { condition: "not_supported", reasonPhrase: "FETCH spans more than one group" };

			if (largest) {
				// A start past Largest Object in a group the cache holds is refused INVALID_RANGE
				// (draft-22 section 9.11), rather than waiting on the live group. A group past
				// every one held does not exist yet.
				if (after(range.start, largest)) {
					return hooks.holdsGroup(cache, safe(range.start.group))
						? { condition: "invalid_range", reasonPhrase: "start past Largest Object" }
						: { condition: "does_not_exist", reasonPhrase: `group ${range.start.group}` };
				}
			}

			const selected = safe(range.start.group);
			const group = hooks.cachedGroup(cache, selected);
			if (!group) return { condition: "does_not_exist", reasonPhrase: `group ${range.start.group}` };
			let last: FetchedGroup;
			try {
				last = await readFetched(group, fetchSlice(range, selected), signal);
			} finally {
				group.close();
			}

			// Only a finished track's final object ends it, never a live track's Largest Object.
			const final = hooks.subscriberEnd(cache);
			const endOfTrack =
				last.complete &&
				final !== undefined &&
				(final.frame === 0 ? final.group : final.group + 1) === last.sequence + 1;
			const sequence = BigInt(last.sequence);
			const groupEnd = BigInt(last.end);
			const until = fetchSlice(range, last.sequence).until;
			let endLocation: Location;
			if (Filter.isDraft22(version)) {
				// Draft-22's End Location is inclusive. An empty answer still covers its start.
				const object =
					until === undefined
						? max(saturatingSub(groupEnd, 1n), BigInt(fetchSlice(range, last.sequence).skip))
						: endOfTrack
							? min(BigInt(until) - 1n, saturatingSub(groupEnd, 1n))
							: BigInt(until) - 1n;
				endLocation = { group: sequence, object };
			} else if (endOfTrack && range.end.object > 0n) {
				// A bounded response cannot promise objects past the track's end.
				endLocation = { group: sequence, object: groupEnd };
			} else {
				endLocation = range.end;
			}

			answer = {
				cache,
				owned: true,
				range,
				last,
				ok: new FetchOk({ endOfTrack, endLocation, properties: trackProperties(info, msg.propertiesWanted) }),
				// A Timestamp goes out only when FETCH_OK actually carries TIMESCALE.
				timescale: msg.propertiesWanted ? (info.timescale ?? undefined) : undefined,
				// Its own rank, so it competes with group streams by its priority alone.
				sendOrder: sendOrder({ priority }),
			};
			return answer;
		} finally {
			if (!answer) cache.close();
		}
	}

	/** Record what a joining FETCH finds for subscription `id`, or remove it when absent. */
	#setJoin(id: bigint, join: Joinable | "pending" | undefined) {
		if (join) this.#joins.set(id, join);
		else this.#joins.delete(id);
		this.#joinsChanged.update((n) => n + 1);
	}

	/**
	 * Wait for the subscription a joining FETCH names to resolve, or the refusal to answer
	 * with. One not yet arrived or still resolving is waited on, bounded like any request,
	 * since the FETCH's stream can beat its SUBSCRIBE's.
	 */
	async #joinOf(id: bigint, signal: AbortSignal): Promise<Joinable | Refusal> {
		const missing: Refusal = { condition: "invalid_joining_request_id", reasonPhrase: "no such subscription" };
		const done = new AbortController();
		const expired = sleep(JOIN_TIMEOUT, { signal: done.signal }).then(
			() => "expired" as const,
			() => "expired" as const,
		);
		let pending = false;
		try {
			for (;;) {
				const join = this.#joins.get(id);
				if (join === "pending") pending = true;
				else if (join) return join;
				// Seen pending and now gone: it ended, or was refused, before it resolved.
				else if (pending) return missing;
				if ((await untilAborted(race([Signal.race(this.#joinsChanged), expired]), signal)) === "expired")
					return pending ? { condition: "timeout", reasonPhrase: "subscription not ready" } : missing;
			}
		} finally {
			// Settles the deadline, which releases the change listener a cancelled wait left.
			done.abort();
		}
	}

	/**
	 * Read the head of the group a joining FETCH's subscription starts in, every object up to
	 * the Largest Object it saved, or the refusal to answer with.
	 */
	async #fetchJoin(
		type: Extract<FetchType, { kind: "relativeJoining" | "absoluteJoining" }>,
		join: Joinable,
		signal: AbortSignal,
	): Promise<FetchAnswer | Refusal> {
		if (join.kind === "unsupported")
			return { condition: "not_supported", reasonPhrase: "joining filter not supported" };
		const largest = join.largest;
		if (!largest) return { condition: "invalid_range", reasonPhrase: "no objects at subscription start" };
		if (type.kind === "relativeJoining" && type.offset !== 0n)
			return { condition: "not_supported", reasonPhrase: "joining FETCH reaches before the joined group" };
		const start = type.kind === "relativeJoining" ? largest.group : type.group;
		if (start > largest.group)
			return { condition: "invalid_range", reasonPhrase: "joining group past the subscription" };
		// The groups before the subscription's are a standalone FETCH's to ask for.
		if (start !== largest.group)
			return { condition: "not_supported", reasonPhrase: "joining FETCH spans more than one group" };

		const end = { group: largest.group, object: largest.object + 1n };
		const sequence = Number(end.group);
		const unavailable: Refusal = { condition: "does_not_exist", reasonPhrase: "joining prefix unavailable" };
		const group = hooks.cachedGroup(join.track, sequence);
		if (!group) return unavailable;
		let last: FetchedGroup;
		try {
			last = await readFetched(group, { skip: 0, until: Number(end.object) }, signal);
		} catch (err) {
			if (signal.aborted) throw err;
			return unavailable;
		} finally {
			group.close();
		}
		// The subscription starts right after the saved Largest Object, so the head has to be
		// here in full or the two leave a gap. Frames read are consecutive, so a full count
		// starts at object 0.
		if (last.frames.length !== Number(end.object)) return unavailable;
		return {
			cache: join.track,
			owned: false,
			range: { start: { group: end.group, object: 0n }, end },
			last,
			// Where the subscription starts, never the end of the track.
			ok: new FetchOk({ endOfTrack: false, endLocation: end, properties: join.properties }),
			timescale: join.timescale,
			// Ranked as the group it heads, ahead of that group's live rest.
			sendOrder: join.ranking.rank(sequence, 0),
			ranking: join.ranking,
		};
	}

	/**
	 * Write FETCH_OK, then the group on a fetch stream named by the FETCH's own request ID. Nothing past FETCH_OK can be refused, so a failure
	 * resets both streams instead.
	 */
	async #answerFetch(msg: Fetch, request: RequestStream, answer: FetchAnswer, stop: AbortSignal) {
		const version = this.#session.version;
		const { range, last, timescale, ranking } = answer;

		let stream: Writer | undefined;
		try {
			// FETCH_OK on every draft, never REQUEST_OK: section 5.2 allows exactly one FETCH_OK or
			// REQUEST_ERROR in answer to a FETCH.
			await untilAborted(Message.write(request.writer, FetchOk.id, answer.ok, version), stop);
			const stopped = new Promise<undefined>((resolve) => {
				if (stop.aborted) resolve(undefined);
				stop.addEventListener("abort", () => resolve(undefined), { once: true });
			});
			stream = await Writer.tryOpen(this.#quic, { version, sendOrder: answer.sendOrder, cancel: stopped });
			if (!stream) throw stop.reason ?? new Error("fetch stream failed to open");
			ranking?.add(stream, Number(range.start.group), Number(range.start.object));
			await untilAborted(new FetchHeader({ requestId: msg.requestId }).encode(stream, version), stop);

			const out = new FetchWriter({ stream, version, timescale });
			for (const frame of last.frames) await out.object(last.sequence, frame, stop);
			stream.close();
			// The request stream ends once the answer is delivered.
			await untilAborted(stream.closed, stop);
		} catch (err: unknown) {
			const e = stop.aborted ? error(stop.reason) : error(err);
			console.debug(`fetch failed: request=${msg.requestId} error=${reason(e)}`);
			stream?.reset(e);
			request.abort(e);
		} finally {
			// Ranked until it settles, like a group stream.
			if (stream && ranking) {
				const opened = stream;
				void opened.closed.catch(() => {}).then(() => ranking.remove(opened));
			}
		}
	}

	/** What a request that failed to resolve is refused with. */
	#failed(err: unknown, wait = false): Refusal {
		const e = error(err);
		const condition =
			e instanceof StreamError && (e.code === StreamCode.NotFound || e.code === StreamCode.Unroutable)
				? wait
					? "timeout"
					: "does_not_exist"
				: "internal";
		return { condition, reasonPhrase: reason(e) };
	}

	/**
	 * Resolve the broadcast a request names for serving: a local publish, or else the
	 * origin's routes and dynamic handlers. Rejects with the abort once `signal` aborts.
	 *
	 * A request naming an `epoch` is served only by that instance: one now serving another
	 * epoch, or none, refuses it as gone rather than answering with other content.
	 */
	async #resolve(name: Path.Valid, signal: AbortSignal, epoch: Epoch.Valid | undefined): Promise<broadcast.Consumer> {
		const broadcast =
			this.#publish &&
			(wireOf(this.#publish).local(name, epoch) ?? (await wireOf(this.#publish).demand(name, epoch, signal)));
		if (!broadcast) throw new NotFound(`broadcast ${name}`);
		return broadcast;
	}

	/** Break a session whose peer names an epoch it was never told about: MoQ Epoch was not negotiated. */
	#pinned(epoch: Epoch.Valid | undefined) {
		if (epoch !== undefined && !this.#epoch) throw new ProtocolViolation("EPOCH without MoQ Epoch");
	}
}

/** A `{group, object}` position, in the Location Filter's own units. */
interface Location {
	/** The group's sequence number. */
	group: bigint;
	/** The object's id within that group. */
	object: bigint;
}

/** Where a range ends, inclusive. An absent `object` includes the whole end group. */
interface EndLocation {
	/** The last group to serve. */
	group: bigint;
	/** The last object within it, or the whole group when absent. */
	object?: bigint;
}

/**
 * The live edge a SUBSCRIBE resolves against, snapshotted once so the subscription floor,
 * the fill cap, and the advertised LARGEST_OBJECT all agree on where it is.
 */
interface LiveEdge {
	/** The newest group sequence, absent before any group exists. */
	latest?: bigint;

	/**
	 * The precise Largest Object. Absent when the track has no readable frame, in which case
	 * nothing is advertised and no fill is servable.
	 */
	largest?: Location;

	/**
	 * One past the Largest Object, which is where a Next Object subscription begins. With no
	 * readable object anywhere this falls back to the newest group's start, which excludes
	 * nothing the cache can still name.
	 */
	next?: Location;
}

/** The Locations a SUBSCRIBE's Location Filter selects, resolved against the live edge. */
interface ServeRange {
	/** The first Location to serve, or the start of the latest group when absent. */
	start?: Location;

	/**
	 * Where the range ends, inclusive, or open ended when absent. The subscription stays open
	 * once the range is exhausted; draft-22 has no notion of a filter ending one.
	 */
	end?: EndLocation;
}

/** What a draft-18 joining FETCH inherits from the SUBSCRIBE it names. */
type Joinable =
	/** The subscription's filter starts nowhere a joining FETCH could end. */
	| { kind: "unsupported" }
	| {
			kind: "join";
			/** The subscription's view of the track, whose cache holds the head. */
			track: TrackSubscriber;
			/** The Largest Object the subscription starts right after, absent when there was none. */
			largest?: Location;
			/** The subscription's namespace, which the fetch is held to the grant on. */
			namespace: Path.Valid;
			/** The Track Properties the subscription was answered with. */
			properties: Properties.Properties;
			/** The units the subscription stamps objects in, if any; see {@link RunGroup.timescale}. */
			timescale?: Timescale;
			/** The subscription's ranking, which the fetch stream joins as the group it heads. */
			ranking: Priority;
	  };

/** The span a FETCH answers, as a standalone FETCH spells it. */
interface FetchRange {
	/** The first Location. */
	start: Location;
	/** The exclusive end: End Object 0 includes the whole End Group. */
	end: Location;
}

/** A FETCH resolved against its track, ready to answer. */
interface FetchAnswer {
	/** The track whose cache holds the answer's groups. */
	cache: TrackSubscriber;
	/** Whether the answer subscribed `cache` itself, and so closes it. */
	owned: boolean;
	range: FetchRange;
	/** The highest group the answer holds, read ahead so FETCH_OK can say where it ends. */
	last: FetchedGroup;
	ok: FetchOk;
	/** The units objects are stamped in, if any; see {@link RunGroup.timescale}. */
	timescale?: Timescale;
	/** The fetch stream's send order. */
	sendOrder: number;
	/** The subscription ranking the fetch stream joins, if any. */
	ranking?: Priority;
}

/** A group read for a FETCH, so a later eviction cannot truncate what FETCH_OK promised. */
interface FetchedGroup {
	sequence: number;
	frames: ({ sequence: number } & group.Frame)[];
	/** One past the last object read, or the slice's start when none was. */
	end: number;
	/** Whether the read reached the group's own end rather than stopping at the slice's. */
	complete: boolean;
}

/** A wire sequence as a model one, saturating past what the model can name. */
function safe(sequence: bigint): number {
	return sequence > BigInt(Number.MAX_SAFE_INTEGER) ? Number.MAX_SAFE_INTEGER : Number(sequence);
}

function min(a: bigint, b: bigint): bigint {
	return a < b ? a : b;
}

function max(a: bigint, b: bigint): bigint {
	return a > b ? a : b;
}

/** Whether `a` is past `b`. */
function after(a: Location, b: Location): boolean {
	return a.group > b.group || (a.group === b.group && a.object > b.object);
}

/** Where a draft-22 FETCH's filter starts, resolved against Largest Object. */
function fetchStart(filter: Filter.Filter, largest: Location): Location {
	switch (filter.kind) {
		case "unfiltered":
			return { group: 0n, object: 0n };
		case "nextObject":
			return { group: largest.group, object: largest.object + 1n };
		case "relative":
			return { group: saturatingSub(largest.group + 1n, filter.groups), object: 0n };
		case "absolute":
			return { group: filter.startGroup, object: filter.startObject };
	}
}

/** The slice of one group a {@link FetchRange} selects. */
function fetchSlice(range: FetchRange, sequence: number): GroupSlice {
	const at = BigInt(sequence);
	return {
		skip: range.start.group === at ? safe(range.start.object) : 0,
		until: range.end.group === at && range.end.object > 0n ? safe(range.end.object) : undefined,
	};
}

/** Read a group's slice, waiting for its end when no object bound is set. */
async function readFetched(group: group.Consumer, slice: GroupSlice, signal: AbortSignal): Promise<FetchedGroup> {
	const frames: FetchedGroup["frames"] = [];
	let end = slice.skip;
	const done = (complete: boolean) => ({ sequence: group.sequence, frames, end, complete });
	for (;;) {
		if (slice.until !== undefined && end >= slice.until)
			return done(group.isClosed && group.frameCount === end && !(group.closed.peek() instanceof Error));
		const frame = await nextFrame(group, slice.skip, signal);
		if (!frame) return done(true);
		if (slice.until !== undefined && frame.sequence >= slice.until) return done(false);
		frames.push(frame);
		end = frame.sequence + 1;
	}
}

/** Read the next selected frame until the requester leaves or the stream closes. */
async function nextFrame(
	group: group.Consumer,
	from: number,
	signal: AbortSignal,
	closed?: Promise<void>,
): Promise<({ sequence: number } & group.Frame) | undefined> {
	const next = group.readFrameSequence({ from });
	return untilAborted(closed === undefined ? next : race([next, closed]).then((frame) => frame ?? undefined), signal);
}

/** Write one group's fetch objects, inheriting the first object's priority. */
class FetchWriter {
	readonly stream: Writer;
	readonly #version: IetfVersion;
	readonly #timescale: Timescale | undefined;
	#written = false;

	constructor({
		stream,
		version,
		timescale,
	}: { stream: Writer; version: IetfVersion; timescale: Timescale | undefined }) {
		this.stream = stream;
		this.#version = version;
		this.#timescale = timescale;
	}

	/** Write `frame` of group `sequence`, following the prior object of that group, if any. */
	async object(sequence: number, frame: { sequence: number } & group.Frame, stop: AbortSignal): Promise<void> {
		const obj = new FetchFrame({ payload: frame.payload, timestamp: frame.timestamp });
		const position = {
			group: sequence,
			object: frame.sequence,
			first: !this.#written,
		};
		await untilAborted(obj.encode(this.stream, position, this.#timescale, this.#version), stop);
		this.#written = true;
	}
}

/** The Track Properties a SUBSCRIBE_OK, TRACK_STATUS, or FETCH_OK answers with. */
function trackProperties(info: Info, wanted: boolean): Properties.Properties {
	// INCLUDE_PROPERTIES=0. The block stays present but empty, which also means the track opts
	// out of timestamps for this requester.
	if (!wanted) return {};
	// TIMESCALE is what opts the track into timestamps, on drafts that can send it.
	return {
		// An untimed track declares none, so it stays untimed downstream.
		timescale: info.timescale ?? undefined,
		priority: toWire(info.priority),
		// The publisher's default, newest first.
		groupOrder: Properties.DESCENDING,
		maxCacheDuration: info.maxAge === undefined ? undefined : BigInt(info.maxAge),
	};
}

/** Read within a fixed filter, ending at its cap once the track itself finishes. */
async function servingGroup(track: TrackSubscriber): Promise<group.Consumer | undefined> {
	for (;;) {
		const recv = hooks.tryRecvGroup(track, false);
		if (recv.kind === "group") return recv.group;
		if (recv.kind === "error") throw recv.error;
		// A boundary only follows the source's real end. Its cached excluded groups
		// cannot delay PUBLISH_DONE; while the source is live the cursor reports idle.
		if (recv.kind === "done" || recv.kind === "boundary") return undefined;
		if (recv.kind === "drop") continue;
		await new Promise<void>((resolve) => {
			const dispose = hooks.groupChanged(track, () => {
				dispose();
				resolve();
			});
		});
	}
}

/** The model's group bounds for a {@link ServeRange}: an absent start joins at the latest group. */
function rangeGroups(range: ServeRange): Groups {
	return {
		start: range.start ? { included: Number(range.start.group) } : undefined,
		end: range.end ? { included: Number(range.end.group) } : undefined,
	};
}

/** The slice of one group a subscription's {@link ServeRange} selects. */
interface GroupSlice {
	/** Objects dropped from the front; also the first written object's absolute id. */
	skip: number;

	/** One past the last object to write, when the filter ends inside this group. */
	until?: number;
}

/** The stream serving a subscription's highest group, as it was opened. */
interface LiveGroup {
	sequence: number;
	slice: GroupSlice;
	/** Set once a narrowing gave the stream up before its FIN. */
	retired: boolean;
}

/** What a draft-22 fill request resolves to. */
type FillServe =
	/** The range is empty, so no fetch stream is opened at all. */
	| { kind: "empty" }
	| {
			kind: "group";
			start: Location;
			end: EndLocation;
	  }
	/** Range Filters require object properties this publisher cannot select. */
	| { kind: "unsupported" };

/** `a - b`, saturating at zero the way the draft's unsigned arithmetic does. */
function saturatingSub(a: bigint, b: bigint): bigint {
	return a > b ? a - b : 0n;
}

/** Snapshot the live edge of a track. */
function liveEdge(track: TrackSubscriber): LiveEdge {
	const latest = hooks.latestSequence(track);
	const largest = hooks.objectLargest(track);
	if (!largest)
		return latest === undefined ? {} : { latest: BigInt(latest), next: { group: BigInt(latest), object: 0n } };

	const group = BigInt(largest.group);
	const object = BigInt(largest.frame);
	return {
		latest: latest === undefined ? undefined : BigInt(latest),
		largest: { group, object },
		next: { group, object: object + 1n },
	};
}

/**
 * Resolve a SUBSCRIBE's Location Filter into the range to serve.
 *
 * Draft-22 is honored whole. Earlier drafts honor only Largest Object, which a joining
 * FETCH pairs with to recover the group's head: their Filter Type tag has absolute forms
 * we never served, and starting to interpret them now would change what an existing peer
 * receives.
 */
function subscribeRange(msg: Subscribe, edge: LiveEdge, version: IetfVersion): ServeRange {
	if (!Filter.isDraft22(version) && msg.filter.kind !== "nextObject") {
		if (msg.filter.kind !== "unfiltered") {
			console.warn(`filter not supported before draft-22, ignoring: ${msg.filter.kind}`);
		}
		return {};
	}

	return filterRange(msg.filter, edge);
}

/** The Locations a single Location Filter selects, resolved against the live edge. */
function filterRange(filter: Filter.Filter, edge: LiveEdge): ServeRange {
	switch (filter.kind) {
		// No restriction. moq-lite starts at the beginning of the latest group, which is the
		// join point it is built around; a subscription passes objects as they are published,
		// so an absent filter is not a request to replay history.
		case "unfiltered":
			return {};
		// `{Largest.Group, Largest.Object + 1}`. Everything below it, including the already
		// published head of the current group, is outside the requested range, so the join is
		// mid-group by construction. The draft pairs this with a fill, or a joining FETCH
		// before draft-22, when the subscriber wants the head; see {@link Publisher.runFetch}.
		case "nextObject":
			return { start: edge.next };
		// `{Largest.Group + 1 - groups, 0}`: 0 is the next group and 1 is the current one.
		// Counted from `Largest.Group`, which sits below the newest group while that group has
		// no objects yet; only with no largest at all does the newest group stand in for it.
		case "relative": {
			const base = edge.largest?.group ?? edge.latest;
			if (base === undefined) return {};
			return { start: { group: saturatingSub(base + 1n, filter.groups), object: 0n } };
		}
		case "absolute":
			return {
				start: { group: filter.startGroup, object: filter.startObject },
				end: filter.endGroup === undefined ? undefined : { group: filter.endGroup, object: filter.endObject },
			};
	}
}

/**
 * Trim a group to the filter's object bounds, so nothing outside the requested range is
 * sent. Interior groups are served whole.
 */
function groupSlice(range: ServeRange, sequence: number): GroupSlice {
	const at = BigInt(sequence);
	return {
		skip: range.start?.group === at ? Number(range.start.object) : 0,
		until: range.end?.group === at && range.end.object !== undefined ? Number(range.end.object) + 1 : undefined,
	};
}

/** Whether `range` drops objects a stream serving `slice` of group `sequence` would send. */
function excludes(range: ServeRange, sequence: number, slice: GroupSlice): boolean {
	const at = BigInt(sequence);
	const ours = groupSlice(range, sequence);
	return (
		(range.start !== undefined && at < range.start.group) ||
		(range.end !== undefined && at > range.end.group) ||
		ours.skip > slice.skip ||
		(ours.until !== undefined && (slice.until === undefined || ours.until < slice.until))
	);
}

/** A fill covering the whole requested prefix owns it; the live cursor starts after its snapshot. */
function afterFill(range: ServeRange, fill: FillServe | undefined, edge: LiveEdge): ServeRange {
	if (fill?.kind !== "group" || !edge.largest || !edge.next) return range;
	const start = range.start ?? { group: edge.latest ?? 0n, object: 0n };
	if (start.group > edge.next.group || (start.group === edge.next.group && start.object >= edge.next.object))
		return range;
	const coversStart =
		fill.start.group < start.group || (fill.start.group === start.group && fill.start.object <= start.object);
	const coversEnd =
		fill.end.group === edge.largest.group &&
		(fill.end.object === undefined || fill.end.object >= edge.largest.object);
	return coversStart && coversEnd ? { ...range, start: edge.next } : range;
}

/**
 * Resolve a fill request using the Fetch rules: relative to Largest Object and never
 * extending beyond it. An omitted Location Filter inherits the subscription's.
 */
function fillRange(fill: Filter.Fill, subscription: Filter.Filter, largest: Location | undefined): FillServe {
	// A Range Filter narrows which objects pass, which we do not implement; serving the
	// unfiltered range instead would deliver objects the peer excluded, so refuse it.
	if (fill.rangeFilters) return { kind: "unsupported" };
	const filter = fill.filter ?? subscription;

	// Nothing published (or no precise edge to cap at) means no fill is servable; an empty
	// range opens no stream.
	if (!largest) return { kind: "empty" };

	let start: Location;
	switch (filter.kind) {
		// A Fetch without a filter is the whole track up to Largest Object.
		case "unfiltered":
			start = { group: 0n, object: 0n };
			break;
		// One past the edge, which for a Fetch is always empty.
		case "nextObject":
			return { kind: "empty" };
		case "relative":
			start = { group: saturatingSub(largest.group + 1n, filter.groups), object: 0n };
			break;
		case "absolute":
			start = { group: filter.startGroup, object: filter.startObject };
			break;
	}

	// Cap the requested end at Largest Object.
	let end: EndLocation = { group: largest.group, object: largest.object };
	if (filter.kind === "absolute" && filter.endGroup !== undefined) {
		const below =
			filter.endGroup < largest.group ||
			(filter.endGroup === largest.group && filter.endObject !== undefined && filter.endObject < largest.object);
		if (below) end = { group: filter.endGroup, object: filter.endObject };
	}

	if (
		start.group > end.group ||
		(start.group === end.group && end.object !== undefined && end.object < start.object)
	) {
		return { kind: "empty" };
	}
	if (start.group !== largest.group || end.group !== largest.group) return { kind: "unsupported" };

	return {
		kind: "group",
		start,
		end,
	};
}

class UpdateFailed extends Error {
	constructor() {
		super("subscription update failed");
	}
}
