import { Duration, type Instant, now, sleep } from "@moq/clock";
import { type Dispose, type Getter, Once, race, Signal } from "@moq/signals";
import type { Credential, Grant } from "../auth.ts";
import { Gate } from "../auth_gate.ts";
import type { AuthSession } from "../auth_session.ts";
import { enforceGrant } from "../auth_session.ts";
import type * as broadcast from "../broadcast.ts";
import { Withdrawal } from "../connection/withdrawal.ts";
import type * as datagram from "../datagram.ts";
import * as DatagramStream from "../datagram_stream.ts";
import {
	controlTimeout,
	error,
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
import type { Hop, WireRoute } from "../hop.ts";
import { datagramTrackInfo, firstEligible, hiddenBelow, hooks, presented, type ReadGroupFrame } from "../internal.ts";
import type { Consumer as OriginConsumer } from "../origin.ts";
import * as Path from "../path.ts";
import { sendPayload } from "../payload.ts";
import { Priority, sendOrder } from "../priority.ts";
import { routeFailed } from "../resume.ts";
import { type Stream, Writer } from "../stream.ts";
import { Timescale } from "../time.ts";
import {
	type Groups,
	type Info,
	type Range,
	type Position as TrackPosition,
	Producer as TrackProducer,
	type Subscriber as TrackSubscriber,
} from "../track.ts";
import { untilAborted } from "../util/abort.ts";
import { toNumber } from "../util/bigint.ts";
import { nextChange } from "../util/change.ts";
import { Deadline, REQUEST_TIMEOUT_MS } from "../util/deadline.ts";
import { GivenUp } from "../util/given_up.ts";
import { max, saturatingSub } from "../util/sequence.ts";
import { TimeoutError, withTimeout } from "../util/timeout.ts";
import * as Varint from "../varint.ts";
import { type Advertised, type Advertisements, sameInstance, wireOf } from "../wire.ts";
import * as Cluster from "./cluster.ts";
import { encodeDatagram } from "./datagram.ts";
import { type RequestCondition, requestReason, SubscribeRefused } from "./error.ts";
import { FetchHeader } from "./fetch.ts";
import * as Filter from "./filter.ts";
import { MAX_AGE_PARAMETER } from "./max_age.ts";
import * as Message from "./message.ts";
import * as Namespace from "./namespace.ts";
import { FetchFrame, Frame, Group as GroupMessage } from "./object.ts";
import { Parameters } from "./parameters.ts";
import { fromWire, toWire } from "./priority.ts";
import * as Properties from "./properties.ts";
import { PublishDone, PublishDoneStatus } from "./publish.ts";
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
import { GroupOrder, type Subscribe, SubscribeOk } from "./subscribe.ts";
import {
	type SubscribeNamespace,
	SubscribeNamespaceEntry,
	SubscribeNamespaceEntryDone,
} from "./subscribe_namespace.ts";
import { MalformedAuthToken } from "./token.ts";
import { TrackStatusOk, type TrackStatusRequest } from "./track.ts";
import type { IetfVersion } from "./version.ts";

/** First wait before re-offering a namespace the peer refused or we couldn't open for. */
const RETRY_BASE = 100;

/** Ceiling on that wait. The loop retries for the life of the session, so it must not spin. */
const RETRY_MAX = 5000;

/** Cluster parameters for one snapshot: the advertised hops, then our own id last. */
function clusterFor(base: Cluster.Advert | undefined, route: WireRoute): Cluster.Advert | undefined {
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
const ADVERTISE_TIMEOUT = Duration.millis(ADVERTISE_TIMEOUT_MS);

/** Sleep `delay`, jittered, so a relay's namespaces don't all retry on the same tick. */
function retryAfter(delay: number): Promise<void> {
	return sleep(Duration.millis(delay * (0.5 + Math.random() / 2)));
}

/**
 * What prevents re-offering the namespace.
 *
 * A peer answers a request it declines with a retry interval, and ignoring it is how a
 * permanent refusal (unauthorized, uninterested) turns into a request every few seconds
 * for the life of the session. `"never"` is an interval of 0 or a terminal local failure;
 * an instant is when we may come back.
 */
type Refused = "never" | Instant;

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

	/** The track's Publisher Priority on the wire, which the fill's first object states. */
	publisherPriority: number;

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

// The condition a request that failed for `err` is refused with, mirroring Rust's `to_code`: a
// verdict stays one, so the peer is never invited to retry it.
function refusedAs(err: Error): RequestCondition {
	if (err instanceof SubscribeRefused) return err.condition;
	if (err instanceof SessionError) {
		switch (err.code) {
			case SessionCode.Unauthorized:
				return "unauthorized";
			case SessionCode.Expired:
				return "expired";
			case SessionCode.Timeout:
				return "timeout";
			case SessionCode.Version:
			case SessionCode.Unsupported:
				return "not_supported";
		}
	} else if (err instanceof StreamError) {
		switch (err.code) {
			case StreamCode.Unauthorized:
				return "unauthorized";
			case StreamCode.DeliveryTimeout:
				return "timeout";
			case StreamCode.MalformedTrack:
				return "malformed_track";
			case StreamCode.GoingAway:
				return "going_away";
			case StreamCode.NotFound:
			case StreamCode.Unroutable:
				return "does_not_exist";
		}
	}
	return "internal";
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
	#datagramWriter?: ReturnType<typeof DatagramStream.datagramWriter>;
	#session: Session;
	#requiresSolicitation: boolean;

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

	// Our grant: only what it lets us publish is advertised and served, and a
	// shrink withdraws what it no longer covers. Undefined until the peer answers, which
	// allows everything.
	#grant: Getter<Grant | undefined>;
	// Holds every incoming request to that grant from dispatch to its end.
	#gate: Gate;

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
		grant = new Signal<Grant | undefined>(undefined),
		auth,
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
		/** The union of our tokens' grants, which bounds what we publish. */
		grant?: Getter<Grant | undefined>;
		/** This session's tokens, which a route names for its request to carry. */
		auth?: Carrier;
	}) {
		this.#quic = quic;
		this.#datagramWriter = DatagramStream.datagramWriter(quic);
		this.#grant = grant;
		this.#gate = new Gate(grant);
		this.#auth = auth;
		this.#session = session;
		const origin = publish && wireOf(publish);
		this.#advertised = origin?.advertised ?? new Signal(new Map());
		this.#publish = publish;
		this.#requiresSolicitation = requiresSolicitation;
		this.#hidden = hidden;
		this.#activeCount = activeCount;
		this.#advert = Cluster.advertise(cluster);
	}

	/** The origin owns broadcasts; transport teardown ends this publisher's served requests. @internal */
	close(): void {
		this.#datagramWriter?.close();
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
		if (Namespace.reserved(msg.trackNamespace, this.#session.version)) {
			await refuseReserved(request);
			return;
		}
		await this.#serveSubscribe(msg, request);
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
		const rendezvous = msg.rendezvous;

		if (msg.rangeFilters) {
			refusal = { condition: "invalid_filter", reasonPhrase: "range filters not supported" };
		} else if (!this.#gate.allows(name)) {
			// Serve only what our grant lets us publish. Checked before resolving, so a denied
			// request never reaches the origin.
			refusal = { condition: "unauthorized", reasonPhrase: "not granted" };
		} else {
			try {
				broadcast = await this.#resolve(name, left.signal);
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
			const fixed = filterRange(msg.filter, {}, this.#session.version);
			// Parked past a bounded window's end: the peer reads this stream's end as the track's.
			return hooks.parked(
				source.track(msg.trackName).subscribe({
					priority: fromWire(msg.subscriberPriority),
					order: msg.groupOrder,
					// An absent budget preserves everything retained by the producer.
					// moq-transport has no field for a minimum.
					delay: { max: Duration.millis(msg.maxAge ?? Varint.MAX_U53), min: null },
					ranges: fixed.start ? serveRanges(fixed) : undefined,
				}),
			);
		};
		// Before any await, so a subscriber waiting on this priority is not stuck behind setup.
		// A transport without datagrams never asks: the lookup would open an upstream
		// subscription nothing here can carry.
		const datagramOf = () => {
			if (DatagramStream.maxDatagramSize(this.#quic) === 0) return;
			const local = wireOf(broadcast).localDatagrams;
			const consumer = local
				? broadcast.datagrams(msg.trackName)
				: wireOf(broadcast).liveDatagrams(msg.trackName);
			if (local) consumer?.setPriority(fromWire(msg.subscriberPriority));
			return consumer;
		};
		let datagrams = datagramOf();
		let track = subscribe(broadcast);

		let datagramTrack: TrackProducer | undefined;
		const resolve = async () => {
			// A relay learns both halves from the same stream response. Only local
			// application handlers can answer either kind independently.
			const streamInfo = track.info().then((info) => ({ kind: "stream" as const, info }));
			const consumer = datagrams;
			const selected = await untilAborted(
				consumer && wireOf(broadcast).localDatagrams
					? Promise.any([
							streamInfo,
							consumer.info().then((info) => ({ kind: "datagram" as const, info })),
						]).catch(() => streamInfo)
					: streamInfo,
				left.signal,
			);
			if (selected.kind === "datagram") {
				track.close();
				datagramTrack = new TrackProducer(msg.trackName).accept(selected.info);
				track = hooks.parked(datagramTrack.subscribe({ priority: fromWire(msg.subscriberPriority) }));
				return datagramTrackInfo(selected.info);
			}
			return selected.info;
		};
		try {
			const info = await resolve().catch(async (err) => {
				if (left.signal.aborted) request.abort(cancel);
				else {
					const refusal = this.#failed(err, rendezvous, true);
					// An upstream's refusal goes on with the Retry Interval it asked for.
					if (err instanceof SubscribeRefused) refusal.retryInterval = err.retryInterval;
					await request.refuse("subscribe", refusal);
				}
				return undefined;
			});
			if (!info) return;
			datagrams ??= datagramOf();
			// Every subscription a resume makes counts toward the same PUBLISH_DONE.
			const streams: StreamCount = { opened: 0 };
			let paused = await this.#forward({
				msg,
				request,
				track,
				info,
				streams,
				datagrams,
				datagramDemand: wireOf(broadcast).localDatagrams,
				datagramTrack,
			});
			// A pause drops the subscription, and its demand with it; a resume subscribes again as
			// a new SUBSCRIBE would, so nothing carries across but the request.
			while (paused) {
				track.close();
				datagramTrack?.abort(cancel);
				datagramTrack = undefined;
				datagrams?.close();
				datagrams = undefined;
				if (paused.answer) await Message.write(request.writer, RequestOk.id, new RequestOk(), version);
				const answer = await this.#pausedSubscribe(msg, request, streams);
				if (!answer) return;
				track = subscribe(broadcast);
				datagrams = datagramOf();
				let resumed: Info;
				try {
					resumed = await resolve();
					datagrams ??= datagramOf();
					// A resume answers with the live edge, which only a granted subscriber learns.
					if (!this.#gate.allows(name)) throw unauthorized(name);
				} catch (err) {
					if (left.signal.aborted) return;
					const refusal = this.#failed(err);
					if (err instanceof SubscribeRefused) refusal.retryInterval = err.retryInterval;
					await refuseUpdate(request.writer, version, "subscribe", refusal);
					await this.#publishFailed(request, streams);
					request.close();
					return;
				}
				paused = await this.#forward({
					msg,
					request,
					track,
					info: resumed,
					streams,
					answer,
					datagrams,
					datagramDemand: wireOf(broadcast).localDatagrams,
					datagramTrack,
				});
			}
		} finally {
			track.close();
			datagramTrack?.abort(cancel);
			datagrams?.close();
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
		streams = { opened: 0 },
		answer,
		datagrams,
		datagramDemand,
		datagramTrack,
	}: {
		msg: Subscribe;
		request: RequestStream;
		track: TrackSubscriber;
		info: Awaited<ReturnType<TrackSubscriber["info"]>>;
		/** The data streams opened over every time the subscription forwarded. */
		streams?: StreamCount;
		/** The update a resumed SUBSCRIBE answers with REQUEST_OK instead of SUBSCRIBE_OK. */
		answer?: { requestId: bigint };
		/** A datagram track of the same name, forwarded as OBJECT_DATAGRAM beside the groups. */
		datagrams?: datagram.Consumer;
		datagramDemand: boolean;
		/** Metadata-only track whose lifetime follows the datagram sender. */
		datagramTrack?: TrackProducer;
	}): Promise<Paused | undefined> {
		const version = this.#session.version;
		const name = msg.trackNamespace;
		const priority = fromWire(msg.subscriberPriority);
		const datagramPriority = new Signal(priority);
		const order = msg.groupOrder;
		let cache: TrackSubscriber | undefined;
		let ranking: Priority | undefined;
		// Assigned once forwarding starts. An early pause returns before that, and the finally
		// still waits so a task begun on another path cannot outlive the subscription.
		let datagramTask: Promise<void> = Promise.resolve();
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
				msg.fill && msg.forward && Filter.isDraft20(version)
					? fillRange(msg.fill, msg.filter, edge.largest)
					: undefined;
			const requested = filterRange(msg.filter, edge, version);
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
				// moq-transport has no field for a minimum.
				delay: { max: Duration.millis(msg.maxAge ?? Varint.MAX_U53), min: null },
				ranges: serveRanges(range),
			});
			const startGroup = range.start ? range.start.group : hooks.latestSequence(track);
			if (startGroup !== undefined) track.withRange({ ...rangeGroups(range), start: { included: startGroup } });
			// The live cursor's lowest group, which a fill-only update never lowers.
			let floor = startGroup ?? 0n;

			// A fill reads the group cache through its own consumer, independent of the
			// subscription's cursor. Forked from this subscriber rather than resolved through
			// the broadcast again: a dynamic serve is one request per peer subscription, so
			// asking the broadcast would mint a second producer nobody has accepted.
			cache =
				fill && fill.kind !== "empty"
					? track.fork({ priority, order, delay: { max: Duration.millis(Varint.MAX_U53), min: null } })
					: undefined;

			const ranked = new Priority(track, info.priority);
			ranking = ranked;
			const properties = trackProperties(info, msg.propertiesWanted);

			if (answer) {
				// The resuming update learns where the subscription now joins.
				const parameters = new Parameters();
				if (edge.largest) parameters.largest = { groupId: edge.largest.group, objectId: edge.largest.object };
				await Message.write(request.writer, RequestOk.id, new RequestOk({ parameters }), version);
			} else {
				// Send SUBSCRIBE_OK
				const ok = new SubscribeOk({
					trackAlias: msg.requestId,
					// Required once the track has content. A Next Object subscription starts right
					// after it and recovers the group's head with a FILL from draft 20, which ends
					// here. On draft 18 it carries the group whole from object 0 instead.
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

			// Groups are served below; datagrams ride beside them and must not fail this subscription.
			// A burst stays behind the group's send order, so it cannot starve the groups.
			datagramTask = datagrams
				? this.#sendDatagrams(datagrams, {
						alias: msg.requestId,
						timescale,
						priority: datagramPriority,
					}).then(
						() => {
							datagramTrack?.finish();
						},
						(err: unknown) => {
							if (datagramTrack) datagramTrack.abort(error(err));
							else console.debug(`stopping datagrams on this subscription: ${reason(err)}`);
						},
					)
				: Promise.resolve();

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
			// The fill in flight. It never rejects.
			let filling: Promise<void> | undefined;
			const startFill = (requestId: bigint, selected: FillServe, source: TrackSubscriber) => {
				const fill: Promise<void> = this.#runFill({
					requestId,
					priority: fromWire(msg.subscriberPriority),
					publisherPriority,
					ranking: ranked,
					fill: selected,
					cache: source,
					timescale,
					unsubscribed,
					cancelled: fillAbort.signal,
					streams,
				}).finally(() => {
					if (filling === fill) filling = undefined;
				});
				filling = fill;
			};
			// The update being applied, from its decode until it is answered or fails.
			let updating: Promise<void> | undefined;
			if (fill && cache) startFill(answer?.requestId ?? msg.requestId, fill, cache);

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
					// A drained track waits this out, decode included, since the update may reopen its range.
					const applied = Promise.withResolvers<void>();
					updating = applied.promise;
					try {
						const { params, requestId: ownRequestId } = await this.#decodeUpdate(request, framed);
						// update() replaces every option, so carry the rest over as Rust does.
						if (params.subscriberPriority !== undefined) {
							const priority = fromWire(params.subscriberPriority);
							track.update({ ...track.subscription.peek(), priority });
							datagramPriority.set(priority);
							if (datagramDemand) datagrams?.setPriority(priority);
						}
						const maxAge = params.maxAge;
						if (maxAge !== undefined) {
							const current = track.subscription.peek();
							track.update({ ...current, delay: { ...current?.delay, max: Duration.millis(maxAge) } });
						}
						// A fill holds one cache view. At most this decoded update waits behind
						// it; request parsing then backpressures any further fill requests.
						if (params.fillParameters) await filling;
						const snapshot = liveEdge(track);
						this.#applyPublished(msg, params);
						const selected =
							msg.fill && Filter.isDraft20(version)
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
										: filterRange(msg.filter, snapshot, version);
							const tail = afterFill(requested, selected, snapshot);
							range.start = tail.start;
							range.end = tail.end;
							bounds.set(requested, true);
							track.update({ ...track.subscription.peek(), ranges: serveRanges(range) });
							// Below its floor the live cursor already passed every group or never owned it,
							// so lowering it for a fill-only update would hand those groups out a second time.
							let start = range.start ? range.start.group : 0n;
							if (params.subscriptionFilter === undefined) start = max(start, floor);
							floor = start;
							track.withRange({ ...rangeGroups(range), start: { included: start } });
							// The fill took the current group's head. The cursor never hands a group out
							// twice, so what its stream no longer carries resumes from the cache.
							const next = tail !== requested ? tail.start : undefined;
							const at = next?.group;
							if (next && at !== undefined && hooks.handedOut(track, at)) {
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
										delay: { max: Duration.millis(Varint.MAX_U53), min: null },
									})
								: undefined;
						try {
							await Message.write(request.writer, RequestOk.id, new RequestOk({ parameters }), version);
						} catch (err) {
							source?.close();
							throw err;
						}
						if (selected && source) startFill(ownRequestId, selected, source);
					} finally {
						updating = undefined;
						applied.resolve();
					}
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

			// Serve groups until the track ends or the requester cancels. Serving ends with the
			// track only once no update or fill is in flight: an update may reopen the range, and
			// a reopened group the cursor never handed out is still the cursor's to serve.
			const serving = (async () => {
				for (;;) {
					const group = await servingGroup(track);
					if (group) {
						serveGroup(group, groupSlice(range, group.sequence));
						continue;
					}
					const pending = updating ?? filling;
					if (!pending) return;
					await pending;
				}
			})();

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
				const end = await race([serving.then(() => served), requestEnded, revoked]);
				if (end === "revoked") {
					console.info(`subscription no longer authorized: broadcast=${name} track=${track.name}`);
					unauthorized = true;
					unsubscribe();
				}
				ended = end === served;
			} catch (err: unknown) {
				publishError = error(err);
				// Stop forwarding at once: a pause answers only after every data stream closed.
				if (publishError instanceof Paused) {
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
			if (!datagramTrack && ended && !publishError && final !== undefined && !streams.terminalSent) {
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
			// A malformed upstream, such as a moq-lite MALFORMED_TRACK reset, is passed on as one.
			const malformed = publishError instanceof StreamError && publishError.code === StreamCode.MalformedTrack;
			try {
				const done = new PublishDone({
					statusCode: unauthorized
						? PublishDoneStatus.UNAUTHORIZED
						: updateFailed
							? PublishDoneStatus.UPDATE_FAILED
							: malformed
								? PublishDoneStatus.MALFORMED_TRACK
								: publishError
									? PublishDoneStatus.INTERNAL_ERROR
									: PublishDoneStatus.TRACK_ENDED,
					streamCount: BigInt(streams.opened),
					reasonPhrase: unauthorized
						? "not granted"
						: updateFailed
							? "update failed"
							: malformed
								? "malformed track"
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
			datagrams?.close();
			await datagramTask;
			track.close();
			// Idempotent, and the backstop for a throw between the fork and the fill starting:
			// #runFill closes it itself on every path it reaches.
			cache?.close();
		}
		return undefined;
	}

	/**
	 * Forward one datagram track as OBJECT_DATAGRAM beside a stream subscription.
	 *
	 * Best-effort: a missing track, an oversized payload, or a datagram the draft cannot
	 * encode is dropped. The datagram track ending does not end the stream subscription.
	 */
	async #sendDatagrams(
		datagrams: datagram.Consumer,
		options: { alias: bigint; timescale?: Timescale; priority: Signal<number> },
	): Promise<void> {
		let info: datagram.Info;
		try {
			info = await datagrams.info();
		} catch (err) {
			console.debug(`serving the stream track without datagrams: ${reason(err)}`);
			return;
		}
		const opened = this.#datagramWriter;
		if (!opened) return;
		const publisher = info.priority;
		const timescale = options.timescale;
		const rank = () => sendOrder({ priority: options.priority.peek(), publisher, position: 2 ** 36 - 1 });
		const sender = opened.open(rank());
		const dispose = options.priority.subscribe(() => sender.setPriority(rank()));
		const version = this.#session.version;
		const max = DatagramStream.maxDatagramSize(this.#quic);
		const left = hooks.datagramClosed(datagrams);
		try {
			for (;;) {
				const datagram = await datagrams.recv();
				if (!datagram) return;
				if (timescale !== undefined && datagram.timestamp.scale === undefined) {
					console.debug(`dropping a datagram without a timestamp: sequence=${datagram.sequence}`);
					continue;
				}
				let body: Uint8Array;
				try {
					body = await encodeDatagram(
						datagram,
						{
							trackAlias: options.alias,
							publisherPriority: toWire(publisher),
							timescale,
						},
						version,
					);
				} catch (err) {
					console.debug(`dropping a datagram this draft cannot encode: ${reason(err)}`);
					continue;
				}
				if (body.byteLength > max) {
					console.debug(
						`dropping datagram larger than the transport limit: size=${body.byteLength} max=${max}`,
					);
					continue;
				}
				// A paused subscription must release its consumer even when transport writes stall.
				await race([sender.writer.ready, left]);
				if (left.peek() !== undefined) return;
				await race([sender.writer.write(body), left]);
				if (left.peek() !== undefined) return;
			}
		} finally {
			dispose();
			await race([sender.close(), left]);
		}
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
		// The subscription's budget, or an eviction of what is left to read, may give up on the
		// group mid-write, which abandons the stream. A group already given up never starts the write.
		const givenUp = new GivenUp(group);
		try {
			// Opens wait for stream credit in send order, and the place ranks this one against
			// the subscription's other groups while it waits.
			// Watch the group while the open waits, and release that watch once it settles.
			const place = ranking.reserve(group.sequence, slice.skip);
			const opened = Promise.withResolvers<void>();
			let failure: Error | undefined;
			const abandoned = givenUp
				.write(() => opened.promise)
				.then((stop) => {
					failure = stop;
				});
			let stream: Writer | undefined;
			try {
				stream = await Writer.tryOpen(this.#quic, {
					version: this.#session.version,
					sendOrder: place.rank,
					cancel: race([unsubscribed, abandoned, excluded.promise]),
					group: true,
				});
			} finally {
				opened.resolve();
				place.release();
			}
			if (!stream) {
				hooks.releaseGroupReader(group, failure ?? new Error("unsubscribed while waiting for a stream"));
				return;
			}
			streams.opened += 1;
			// A REQUEST_UPDATE re-ranks the subscription, so a group already on the wire follows it.
			ranking.add(stream, group.sequence, slice.skip);

			const header = new GroupMessage({
				trackAlias: requestId,
				groupId: group.sequence,
				subGroupId: 0n,
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
			});
			// Either ends serving the group: made once, so each read and write races one promise for both.
			const stopped = race([cancelled, excluded.promise]).then(() => false as const);
			const write = async (op: () => Promise<unknown>) => {
				const stop = await givenUp.write(op);
				if (stop) throw stop;
			};
			// Writing the group stops for the pause too; the drain below answers it without a reset.
			// One race covers the budget and both, rather than racing the budget's race again.
			const send = async (op: () => Promise<unknown>) => {
				const stop = await givenUp.write(op, stopped);
				if (stop) throw stop;
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
					const read = await race([hooks.readGroupFrame(group, false, slice.skip), stream.closed, stopped]);
					if (left) throw left;
					if (rangeError) throw rangeError;
					if (!read) break;
					next = read.sequence + 1;
					if (slice.until !== undefined && read.sequence >= slice.until) break;

					// A frame written in pieces streams its payload after the header.
					const chunks = read.chunks;
					const obj = chunks?.size
						? new Frame({ streamed: chunks.size, timestamp: chunks.timestamp })
						: new Frame({
								payload: chunks ? new Uint8Array(0) : read.frame.payload,
								timestamp: read.frame.timestamp,
							});
					const delta = first ? read.sequence : 0;
					first = false;
					await send(() => obj.encode(stream, header.flags, timescale, this.#session.version, delta));
					// Even an empty one: it is an object only once committed, and may still be aborted.
					if (chunks) await send(() => sendPayload(chunks, (chunk) => stream.write(chunk), stream.closed));
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
			givenUp.close();
			group.close();
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
				subGroupId: 0n,
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
		const { requestId, ranking, fill, cache, timescale, publisherPriority, unsubscribed, cancelled, streams } =
			options;
		const version = this.#session.version;

		// Everything is inside the try so the cache fork is released on every path out,
		// including a transport failure while opening the stream.
		let stream: Writer | undefined;
		try {
			if (fill.kind === "empty") return;

			// A fill we cannot serve is reset right after its header, so it needs no rank.
			const sequence = fill.kind === "group" ? fill.start.group : undefined;
			const object = fill.kind === "group" ? frameBound(fill.start.object) : 0;
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

			const out = new FetchWriter({ stream, version, timescale, priority: publisherPriority });
			const group = hooks.cachedGroup(cache, fill.start.group);
			if (!group) throw new NotFound("fill group unavailable");
			const given = untilGivenUp(group, cancelled);
			try {
				await this.#writeFetchGroup({
					out,
					group,
					slice: groupSlice(fill, group.sequence),
					cancelled: given.signal,
				});
			} finally {
				given.release();
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
	 * Stream one group's selected objects for a FETCH or subscription fill.
	 * A bound ends the response without waiting for the live group's end.
	 */
	async #writeFetchGroup({
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
			const frame = await untilAborted(
				race([hooks.readGroupFrame(group, false, slice.skip), out.stream.closed]),
				cancelled,
			);
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

			// Inline entries always land, and the receiver treats a repeated NAMESPACE as a
			// replacement, so repricing one is just sending it again, a new original
			// publisher included, as Rust does.
			const namespace = async (suffix: Path.Valid, snap: Advertised): Promise<Answer> => {
				// MoQ Cluster puts the Parameters field on NAMESPACE.
				const params = this.#advert !== undefined ? this.#params(snap.route) : undefined;
				const entry = new SubscribeNamespaceEntry({ suffix, params });
				await Message.write(request.writer, SubscribeNamespaceEntry.id, entry, version);
				return "held";
			};
			const target: Target = {
				advertise: namespace,
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
					ns.refused.set(key, now().saturatingAdd(Duration.millis(wait)));
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
					// nothing our grant does not cover reaches the peer at all.
					if ((this.#hidden && hiddenBelow(Path.empty(), covered)) || !this.#gate.allows(covered)) continue;
					const best = firstEligible(candidates);
					if (best) updated.set(covered, best);
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
	 * Nothing the peer cannot see is sent, a reprice updates the namespace in place, and
	 * a changed instance is withdrawn before it is advertised again.
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
			if (this.#change(key, snap, next, target) !== "restart") continue;
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
		return entry !== "never" && now().ge(entry);
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
	 * namespace: 0, or an interval past the clock's range, means the peer does not want this again.
	 */
	#refusal(err: RequestError): Answer {
		if (err.retryInterval === 0n) return "never";
		return now().checkedAdd(Duration.millis(toNumber(err.retryInterval))) ?? "never";
	}

	/**
	 * The extension parameters an advertisement of `route` carries.
	 */
	#params(route: WireRoute): AdvertParams {
		return { cluster: clusterFor(this.#advert, route) };
	}

	/**
	 * Advertise one namespace on its own PUBLISH_NAMESPACE request. A declined request
	 * is logged and skipped: a peer that wants none of this rejects each one and stays
	 * connected.
	 */
	async #advertise(path: Path.Valid, requests: Requests, route: WireRoute): Promise<Answer> {
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
			const deadline = new Deadline(Duration.millis(ADVERTISE_TIMEOUT_MS));
			const answer = await deadline.wait(
				(async (): Promise<Answer> => {
					const opened = await RequestStream.open(this.#session, deadline, { quic: this.#quic });
					request = opened;
					const version = this.#session.version;
					const token = this.#auth?.carried(auth);
					const msg = new PublishNamespace({ requestId, trackNamespace: path, params, token });
					await deadline.send(opened, async () => {
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
			return e instanceof StreamError && e.code === StreamCode.ControlTimeout ? "never" : "dropped";
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
		// Nothing can follow a TRACK_STATUS, so a FIN only ends the requester's input; a reset or
		// STOP_SENDING withdraws it.
		const request = new RequestStream(stream, this.#session, this.#quic, { finCancels: false });
		return request.serve(this.#runTrackStatusRequest(msg, request));
	}

	async #runTrackStatusRequest(msg: TrackStatusRequest, request: RequestStream) {
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
				const broadcast = await this.#resolve(name, hold.signal);
				const info = await wireOf(broadcast).resolveTrackInfo(msg.trackName, hold.signal);
				const largest = wireOf(broadcast).largest(msg.trackName);
				answer = new TrackStatusOk({
					largest: largest && { groupId: largest.group, objectId: BigInt(largest.frame) },
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
	 * What a request that failed to resolve is refused with. A missing publisher a rendezvous
	 * waited for is a timeout, which, like Rust's `rendezvous_error`, invites no retry.
	 */
	#failed(err: unknown, wait = false, resolved = false): Refusal {
		const e = error(err);
		const reasonPhrase = reason(e);
		const missing =
			e instanceof StreamError &&
			(e.code === StreamCode.Unroutable || (e.code === StreamCode.NotFound && !resolved));
		if (wait && missing) return { condition: "timeout", reasonPhrase };
		return { condition: refusedAs(e), reasonPhrase, retryable: routeFailed(e) };
	}

	/**
	 * Resolve the broadcast a request names for serving: a local publish, or else the
	 * origin's routes and dynamic handlers. Rejects with the abort once `signal` aborts.
	 */
	async #resolve(name: Path.Valid, signal: AbortSignal): Promise<broadcast.Consumer> {
		const broadcast =
			this.#publish &&
			(wireOf(this.#publish).local(name) ?? (await wireOf(this.#publish).demand(name, undefined, signal)));
		if (!broadcast) throw new NotFound(`broadcast ${name}`);
		return broadcast;
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

// A filter keeps its peer-chosen Object IDs as bigint. Its derived frame selection
// bound can stop at the model's count ceiling: no object exists at or above it,
// so clamping either a skip or an exclusive end selects exactly the same frames.
function frameBound(object: bigint): number {
	return object > BigInt(Number.MAX_SAFE_INTEGER) ? Number.MAX_SAFE_INTEGER : Number(object);
}

/** Write one group's fetch objects, inheriting the first object's priority. */
class FetchWriter {
	readonly stream: Writer;
	readonly #version: IetfVersion;
	readonly #timescale: Timescale | undefined;
	/** The track's Publisher Priority on the wire, which the first object states for all. */
	readonly #priority: number;
	#written = false;

	constructor({
		stream,
		version,
		timescale,
		priority,
	}: { stream: Writer; version: IetfVersion; timescale: Timescale | undefined; priority: number }) {
		this.stream = stream;
		this.#version = version;
		this.#timescale = timescale;
		this.#priority = priority;
	}

	/** Write `read` of group `sequence`, following the prior object of that group, if any. */
	async object(sequence: bigint, read: ReadGroupFrame, stop: AbortSignal): Promise<void> {
		const chunks = read.chunks;
		const obj = new FetchFrame({
			payload: chunks ? undefined : read.frame.payload,
			streamed: chunks?.size || undefined,
			timestamp: read.frame.timestamp,
			priority: this.#priority,
		});
		const position = {
			group: sequence,
			object: read.sequence,
			first: !this.#written,
		};
		await untilAborted(obj.encode(this.stream, position, this.#timescale, this.#version), stop);
		// Even an empty payload: the object exists only once committed, and may still be aborted.
		if (chunks)
			await untilAborted(
				sendPayload(chunks, (chunk) => this.stream.write(chunk), this.stream.closed),
				stop,
			);
		this.#written = true;
	}
}

/**
 * `signal`, also aborted with why `group` was given up: its copy failed with no route to continue
 * it, it was evicted, or it expired. A write parked on flow control never reads the group, so
 * without this a FETCH would hold its reader until the request ends. Release once nothing is
 * left to write.
 */
function untilGivenUp(group: group.Consumer, signal: AbortSignal): { signal: AbortSignal; release(): void } {
	const givenUp = new GivenUp(group);
	const failed = new AbortController();
	const released = Promise.withResolvers<void>();
	void givenUp
		.write(() => released.promise)
		.then((reason) => {
			if (reason) failed.abort(reason);
		});
	return {
		signal: AbortSignal.any([signal, failed.signal]),
		release: () => {
			givenUp.close();
			released.resolve();
		},
	};
}

/**
 * The Track Properties a SUBSCRIBE_OK, TRACK_STATUS, or FETCH_OK answers with.
 *
 * @internal Exported for tests.
 */
export function trackProperties(info: Info, wanted: boolean): Properties.Properties {
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
		// No MAX_CACHE_DURATION: our max age expires only superseded groups, while that
		// property expires every object, the newest included, after the given time.
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
/** The model ranges a {@link ServeRange} asks for, whole groups through its last one. */
function serveRanges(range: ServeRange): Range[] {
	const out: Range = {};
	if (range.start) out.start = { absolute: range.start.group };
	if (range.end) out.end = { absolute: range.end.group + 1n };
	return [out];
}

function rangeGroups(range: ServeRange): Groups {
	return {
		start: range.start ? { included: range.start.group } : undefined,
		end: range.end ? { included: range.end.group } : undefined,
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
	sequence: bigint;
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

/** Snapshot the live edge of a track. */
function liveEdge(track: TrackSubscriber): LiveEdge {
	const latest = hooks.latestSequence(track);
	const largest = hooks.objectLargest(track);
	if (!largest) return latest === undefined ? {} : { latest, next: { group: latest, object: 0n } };

	const group = largest.group;
	const object = BigInt(largest.frame);
	return {
		latest,
		largest: { group, object },
		next: { group, object: object + 1n },
	};
}

/** The Locations a single Location Filter selects, resolved against the live edge. */
function filterRange(filter: Filter.Filter, edge: LiveEdge, version: IetfVersion): ServeRange {
	switch (filter.kind) {
		// No restriction. moq-lite starts at the beginning of the latest group, which is the
		// join point it is built around; a subscription passes objects as they are published,
		// so an absent filter is not a request to replay history. A subscriber that wants the
		// track from its start fetches the groups before the live one.
		case "unfiltered":
			return {};
		// `{Largest.Group, Largest.Object + 1}`. Everything below it, including the already
		// published head of the current group, is outside the requested range, so the join is
		// mid-group by construction. From draft-20 the draft pairs this with a fill when the
		// subscriber wants the head. Before it, the head comes from a joining FETCH, which we
		// refuse, so the subscription carries the current group whole instead: a subscriber
		// starting at object 0 takes it as the whole group, and the head still reaches one that
		// sends no joining FETCH.
		case "nextObject":
			if (!edge.next || Filter.isDraft20(version)) return { start: edge.next };
			return { start: { group: edge.next.group, object: 0n } };
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
function groupSlice(range: ServeRange, sequence: bigint): GroupSlice {
	return {
		skip: range.start?.group === sequence ? frameBound(range.start.object) : 0,
		until:
			range.end?.group === sequence && range.end.object !== undefined
				? frameBound(range.end.object + 1n)
				: undefined,
	};
}

/** Whether `range` drops objects a stream serving `slice` of group `sequence` would send. */
function excludes(range: ServeRange, sequence: bigint, slice: GroupSlice): boolean {
	const ours = groupSlice(range, sequence);
	return (
		(range.start !== undefined && sequence < range.start.group) ||
		(range.end !== undefined && sequence > range.end.group) ||
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
