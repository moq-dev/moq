import { Instant, Milli, Nano, now, sleep } from "@moq/clock";
import { type Dispose, type Getter, race, Signal } from "@moq/signals";
import type { Auth, Grant } from "../auth.ts";
import { Gate } from "../auth_gate.ts";
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
import { Priority } from "../priority.ts";
import { type Stream, Writer } from "../stream.ts";
import { Timescale } from "../time.ts";
import type { Location as TrackLocation, Subscriber as TrackSubscriber } from "../track.ts";
import { untilAborted } from "../util/abort.ts";
import { Deadline, REQUEST_TIMEOUT_MS, requestOptions } from "../util/deadline.ts";
import { LIMITS, type Limits, Slots } from "../util/limits.ts";
import { TimeoutError, withTimeout } from "../util/timeout.ts";
import * as Varint from "../varint.ts";
import { type Advertised, type Advertisements, sameInstance, wireOf } from "../wire.ts";
import type { Session } from "./adapter.ts";
import * as Cluster from "./cluster.ts";
import { encodeDatagram } from "./datagram.ts";
import { type RequestKind, requestReason, toRequestCode } from "./error.ts";
import { type Fetch, FetchError, FetchHeader } from "./fetch.ts";
import * as Filter from "./filter.ts";
import * as Message from "./message.ts";
import * as Namespace from "./namespace.ts";
import { FetchFrame, Frame, Group as GroupMessage } from "./object.ts";
import { Parameters } from "./parameters.ts";
import { fromWire, toWire } from "./priority.ts";
import * as Properties from "./properties.ts";
import { PublishDone, PublishDoneStatus } from "./publish.ts";
import {
	type AdvertParams,
	PublishNamespace,
	PublishNamespaceDone,
	PublishNamespaceOk,
	PublishNamespaceUpdate,
} from "./publish_namespace.ts";
import { RequestError, RequestOk } from "./request.ts";
import { cancelled, finCancels, malformed, namespaceUpdate, nextMessage, refuseReserved } from "./request_stream.ts";
import { GroupOrder, type Subscribe, SubscribeError, SubscribeOk, SubscribeUpdate } from "./subscribe.ts";
import {
	type SubscribeNamespace,
	SubscribeNamespaceEntry,
	SubscribeNamespaceEntryDone,
	SubscribeNamespaceError,
	SubscribeNamespaceOk,
} from "./subscribe_namespace.ts";
import { MalformedAuthToken } from "./token.ts";
import { TRACK_STATUS_ERROR_14, TrackStatusOk, type TrackStatusRequest } from "./track.ts";
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
	token?: Auth;
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
type NamespaceRequest = { path: Path.Valid; requestId: bigint; stream: Stream; auth?: Auth };

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
	carried(token: Auth | undefined): Uint8Array | undefined;
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
	held(key: Path.Valid): Auth | undefined;

	/** The token an advertisement naming `auth` carries: only a request can carry one. */
	carry(auth: Auth | undefined): Auth | undefined;
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
	 * subscriber sent INCLUDE_PROPERTIES=0, or the draft (14-16) cannot send the property. A
	 * timestamp whose units were never declared is worse than none. Our own reader discards
	 * it; another may read it as some default and time the media wrong.
	 */
	timescale?: Timescale;

	/** The publisher's tie-break priority, already converted to the IETF wire convention. */
	publisherPriority: number;

	/** The subscription's ranking, which orders its group streams by priority and GROUP_ORDER. */
	ranking: Priority;

	/** The objects of this group the subscription's filter selects. */
	slice: GroupSlice;

	/** Settles when the subscriber leaves, dropping a group still queued for a stream slot. */
	unsubscribed: Promise<void>;

	/** The subscription's data stream count, which PUBLISH_DONE reports. */
	streams: StreamCount;
}

/** How many data streams a subscription opened, fill streams included. */
type StreamCount = { opened: number; terminalSent?: boolean };

/** What {@link Publisher.runFill} needs to serve one subscription's backfill. */
interface RunFill {
	/** The subscription's request ID, which the fetch stream names. */
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
	// Subscriptions the peer holds live on this session.
	#subscriptions: Slots;
	#fetches: Slots;

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
		limits = LIMITS,
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
		/** How many live requests the peer may hold. */
		limits?: Limits;
	}) {
		this.#quic = quic;
		this.#subscriptions = new Slots(limits.subscriptions, { quic, reason: "too many subscriptions" });
		this.#fetches = new Slots(limits.fetches, { quic, reason: "too many fetches" });
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
	async runSubscribe(msg: Subscribe, stream: Stream) {
		this.#pinned(msg.epoch);
		if (Namespace.reserved(msg.trackNamespace, this.#session.version)) {
			await refuseReserved(stream, this.#session.version);
			return;
		}
		// Only a subscription we would serve holds a slot, as in Rust: a refused one ends at
		// once. A peer past the cap loses the session.
		const charged = msg.forward && !msg.rangeFilters && this.#gate.allows(msg.trackNamespace);
		if (charged) this.#subscriptions.acquire();
		try {
			await this.#serveSubscribe(msg, stream);
		} finally {
			if (charged) this.#subscriptions.release();
		}
	}

	async #serveSubscribe(msg: Subscribe, stream: Stream) {
		const version = this.#session.version;
		const name = msg.trackNamespace;
		let broadcast: broadcast.Consumer | undefined;
		let refusal: Refusal | undefined;

		// The requester leaving while the broadcast and track resolve ends the request, so a
		// cancelled subscription gives back its slot instead of holding it until the track
		// is answered, which may be never.
		const left = new AbortController();
		const cancel = new StreamError(StreamCode.Cancel, { message: "cancel" });
		void cancelled(stream, version).then(() => left.abort(cancel));

		// Legal requests we can't honor are refused one at a time. A subscription that
		// forwards nothing is only useful to a subscriber that later turns forwarding on,
		// and serving a Range Filter unfiltered would deliver objects it excluded.
		// An instance never comes back, so a request pinned to a gone one is missing, not
		// worth waiting for.
		const rendezvous = msg.rendezvous && msg.epoch === undefined;

		const unsupported = !msg.forward
			? "FORWARD=0 not supported"
			: msg.rangeFilters
				? "range filters not supported"
				: undefined;

		if (unsupported) {
			refusal = {
				errorCode: toRequestCode(
					msg.rangeFilters && msg.forward ? "invalid_filter" : "not_supported",
					"subscribe",
					version,
				),
				reasonPhrase: unsupported,
			};
		} else if (!this.#gate.allows(name)) {
			// Serve only what our grant lets us publish. Checked before resolving, so a denied
			// request never reaches the origin.
			refusal = { errorCode: toRequestCode("unauthorized", "subscribe", version), reasonPhrase: "not granted" };
		} else {
			try {
				broadcast = await this.#resolve(name, left.signal, msg.epoch);
			} catch (err: unknown) {
				// Nobody is left to refuse.
				if (left.signal.aborted) {
					stream.abort(cancel);
					return;
				}
				refusal = this.#failed(err, "subscribe", rendezvous);
			}
		}

		if (refusal) {
			await this.#refuse(stream, msg.requestId, SubscribeError.id, refusal);
			return;
		}
		if (!broadcast) return;

		const priority = fromWire(msg.subscriberPriority);
		const order = msg.groupOrder;
		const track = wireOf(broadcast).subscribe(msg.trackName, {
			priority,
			order,
			// moq-transport has no subscriber latency parameter. Keep everything the
			// producer retained and let the receiving subscriber enforce its own budget.
			// Keep the sentinel encodable if this demand crosses a Lite hop before the
			// producer's retention bound is known.
			maxDelay: Milli(Varint.MAX_U53),
		});

		let cache: TrackSubscriber | undefined;
		let ranking: Priority | undefined;

		try {
			let info: Awaited<ReturnType<TrackSubscriber["info"]>>;
			try {
				info = await untilAborted(track.info(), left.signal);
			} catch (err) {
				const failed = error(err);
				if (
					!(failed instanceof StreamError) ||
					(failed.code !== StreamCode.NotFound && failed.code !== StreamCode.Unroutable)
				)
					throw err;
				// The broadcast resolved, so only a vanished route still means a missing publisher;
				// a missing track is the publisher's answer.
				await this.#refuse(
					stream,
					msg.requestId,
					SubscribeError.id,
					this.#failed(err, "subscribe", rendezvous && failed.code === StreamCode.Unroutable),
				);
				return;
			}
			// A Timestamp goes out only when this SUBSCRIBE_OK actually carries TIMESCALE.
			// Drafts 14-16 never write that property, so their objects stay unstamped.
			const timescale =
				msg.propertiesWanted && Properties.sendsTimescale(version) ? (info.timescale ?? undefined) : undefined;
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
			const range = subscribeRange(msg, edge, version);

			// The wire request tells an upstream what we need; the cursor is what actually
			// trims this subscriber, since the producer fans every cached group out to every
			// sink regardless. An absent start joins at the latest group, which is what
			// moq-lite means by joining a live track. The range's end is the inclusive last
			// group; the model's `endGroup` is exclusive.
			track.update({
				priority,
				order,
				maxDelay: Milli(Varint.MAX_U53),
				groups: {
					start: range.start ? { included: Number(range.start.group) } : undefined,
					end: range.end ? { included: Number(range.end.group) } : undefined,
				},
			});
			const startGroup = range.start ? Number(range.start.group) : hooks.latestSequence(track);
			if (startGroup !== undefined) track.setGroups({ start: { included: startGroup } });

			// A fill reads the group cache through its own consumer, independent of the
			// subscription's cursor. Forked from this subscriber rather than resolved through
			// the broadcast again: a dynamic serve is one request per peer subscription, so
			// asking the broadcast would mint a second producer nobody has accepted.
			const fill =
				msg.fill && Filter.isDraft20(version) ? fillRange(msg.fill, msg.filter, edge.largest) : undefined;
			cache =
				fill && fill.kind !== "empty"
					? track.fork({ priority, order, maxDelay: Milli(Varint.MAX_U53) })
					: undefined;

			// Send SUBSCRIBE_OK
			await stream.writer.u53(SubscribeOk.id);
			const ok = new SubscribeOk({
				requestId:
					version === Version.DRAFT_14 || version === Version.DRAFT_15 || version === Version.DRAFT_16
						? msg.requestId
						: undefined,
				trackAlias: msg.requestId,
				// Required once the track has content; a fill-requesting subscriber sizes its
				// backfill against this. Below draft-20 round it down to the start of its
				// group, matching what the subscription serves there: a draft-20 subscriber
				// recovers a group's head with a FILL, which we serve, while older drafts
				// recover it with a joining FETCH, which we do not, so naming a mid-group
				// start would strand them.
				largest:
					edge.largest &&
					(Filter.isDraft20(version)
						? { groupId: edge.largest.group, objectId: edge.largest.object }
						: { groupId: edge.largest.group, objectId: 0n }),
				properties: msg.propertiesWanted
					? // TIMESCALE is what opts the track into timestamps, on drafts that can
						// send it.
						{
							// An untimed track declares none, so it stays untimed downstream.
							timescale: info.timescale ?? undefined,
							priority: publisherPriority,
							// Through draft-15 this restates the order the subscription is served
							// in. Later drafts carry the publisher's default instead, newest first.
							groupOrder:
								(version === Version.DRAFT_14 || version === Version.DRAFT_15) && order === "ascending"
									? GroupOrder.Ascending
									: Properties.DESCENDING,
							maxCacheDuration: info.maxAge === undefined ? undefined : BigInt(info.maxAge),
						}
					: // INCLUDE_PROPERTIES=0. The block stays present but empty, which also means
						// the track opts out of timestamps for this subscriber.
						{},
			});
			await ok.encode(stream.writer, version);
			console.debug(`publish ok: broadcast=${name} track=${track.name}`);

			// Cancels groups still queued for a stream slot. Only the subscriber leaving counts:
			// a track that ran out of groups still has to flush the ones already queued, and we
			// close the stream ourselves below to say so.
			let finished = false;
			let unsubscribe!: () => void;
			const unsubscribed = new Promise<void>((resolve) => {
				unsubscribe = resolve;
			});
			const legacy = version === Version.DRAFT_14 || version === Version.DRAFT_15 || version === Version.DRAFT_16;
			const goaway = { client: this.#session.client, seen: false };
			const updates = (async () => {
				for (;;) {
					const type = await nextMessage(stream.reader, version, goaway).catch((err: unknown) => {
						throw malformed(err, "subscription message");
					});
					if (type === undefined) {
						if (legacy || finCancels(version)) return;
						return stream.writer.closed;
					}
					if (type !== 0x02)
						throw new ProtocolViolation(`unexpected message on a subscription: 0x${type.toString(16)}`);
					// Draft 14 answers no update. Drafts 15 and 16 name the update's own Request ID;
					// later drafts name none.
					const refuse = async (
						condition: "not_supported" | "malformed_auth_token",
						reasonPhrase: string,
						requestId: bigint | undefined,
					) => {
						if (version === Version.DRAFT_14) return new UpdateFailed();
						await stream.writer.u53(RequestError.id);
						await new RequestError({
							requestId: legacy ? requestId : undefined,
							errorCode: toRequestCode(condition, "subscribe", version),
							reasonPhrase,
						}).encode(stream.writer, version);
						return new UpdateFailed();
					};
					let ownRequestId: bigint | undefined;
					let params: Parameters;
					try {
						if (legacy) {
							// The adapter already spent the update's own Request ID.
							const update = await SubscribeUpdate.decode(stream.reader, version);
							ownRequestId = update.ownRequestId;
							params = update.parameters;
						} else {
							params = await Message.decode(stream.reader, async (r) => {
								this.#session.spendRequest(await r.u62());
								if (version === Version.DRAFT_17) await r.u62();
								return Parameters.decode(r, version, "request-update");
							});
						}
					} catch (err) {
						// Section 8.9 rejects the update, which says more than that we do not support it.
						if (err instanceof MalformedAuthToken)
							throw await refuse("malformed_auth_token", err.message, err.requestId);
						throw malformed(err, "REQUEST_UPDATE");
					}
					if (params.trackPropertyFilter)
						throw new ProtocolViolation(
							"TRACK_PROPERTY_FILTER is only allowed on SUBSCRIBE_TRACKS updates",
						);
					const unsupported =
						params.forward === false ||
						params.subscriptionFilter !== undefined ||
						params.bytes.size > 0 ||
						params.rangeFilters ||
						[...params.vars.keys()].some(
							(key) => key !== 0x02n && key !== 0x06n && key !== 0x10n && key !== 0x20n,
						);
					if (unsupported)
						throw await refuse("not_supported", "REQUEST_UPDATE parameters not supported", ownRequestId);
					// update() replaces every option, so carry the rest over as Rust does.
					if (params.subscriberPriority !== undefined)
						track.update({ ...track.subscription.peek(), priority: fromWire(params.subscriberPriority) });
					if (version === Version.DRAFT_14) continue;
					await stream.writer.u53(RequestOk.id);
					await new RequestOk({ requestId: ownRequestId }).encode(stream.writer, version);
				}
			})();
			const requestEnded = legacy ? updates : race([updates, stream.writer.closed]);
			void requestEnded.then(
				() => {
					if (!finished) unsubscribe();
				},
				// A reset is always the peer.
				() => unsubscribe(),
			);

			// Every group started, until its stream finishes or resets, and the data streams
			// opened for PUBLISH_DONE to report.
			const groups = new Set<Promise<void>>();
			const streams: StreamCount = { opened: 0 };
			// One ranking for the whole subscription, shared by every group and the fill.
			const ranked = new Priority(track);
			ranking = ranked;

			// Serve groups until the track ends or the requester cancels.
			const serving = (async () => {
				for (;;) {
					const group = await track.recvGroup();
					if (!group) return;

					// Past the filter's end. Dropped here rather than through `setGroups`, which
					// parks a capped group instead: this range is fixed for the life of the
					// subscription, so a group above it is never coming back in, and holding
					// one keeps the loop from ever ending. A producer that publishes beyond
					// the end and then closes would otherwise strand PUBLISH_DONE.
					if (range.end !== undefined && BigInt(group.sequence) > range.end.group) {
						group.close();
						continue;
					}

					const task = this.#runGroup({
						track,
						requestId: msg.requestId,
						group,
						timescale,
						publisherPriority,
						ranking: ranked,
						slice: groupSlice(range, group.sequence),
						unsubscribed,
						streams,
					});
					groups.add(task);
					void task.finally(() => groups.delete(task));
				}
			})();

			// Datagrams count toward no stream, so PUBLISH_DONE never waits on them; closing
			// the track below ends the loop.
			void this.#runDatagrams({ requestId: msg.requestId, track, timescale, publisherPriority });

			// The fill (when one was requested) runs alongside on its own fetch stream; its
			// failures reset that stream and never touch the subscription.
			const filling =
				fill && cache
					? this.#runFill({
							requestId: msg.requestId,
							priority,
							ranking: ranked,
							fill,
							cache,
							timescale,
							unsubscribed,
							streams,
						})
					: Promise.resolve();

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
				const end = await race([Promise.all([serving, filling]).then(() => served), requestEnded, revoked]);
				if (end === "revoked") {
					console.info(`subscription no longer authorized: broadcast=${name} track=${track.name}`);
					unauthorized = true;
					unsubscribe();
				}
				ended = end === served;
			} catch (err: unknown) {
				publishError = error(err);
			} finally {
				watch.dispose();
			}
			// An update that broke the protocol, such as one whose AUTHORIZATION TOKEN names an
			// alias, closes the session: the connection answers a thrown violation with its code.
			if (publishError instanceof ProtocolViolation) throw publishError;

			// PUBLISH_DONE waits until every stream this subscription will open is closed, as
			// the draft requires, so its count is final. The subscriber leaving cancels the
			// ones still queued instead.
			await race([Promise.all(groups), unsubscribed]);

			// Draft 14 on has no end location in PUBLISH_DONE: an END_OF_TRACK object is what
			// tells the subscriber where the track ended.
			const final = track.end();
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
			// Draft 14 registers no UPDATE_FAILED, so a failed update ends there as any error does.
			const updateFailed = publishError instanceof UpdateFailed && version !== Version.DRAFT_14;
			try {
				await stream.writer.u53(PublishDone.id);
				const done = new PublishDone({
					requestId:
						version === Version.DRAFT_14 || version === Version.DRAFT_15 || version === Version.DRAFT_16
							? msg.requestId
							: undefined,
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
				await done.encode(stream.writer, version);
			} catch {
				// Stream might already be closed by peer.
			}

			// Our close follows PUBLISH_DONE; it must not cancel data still queued for delivery.
			finished = true;
			stream.close();
		} catch (err: unknown) {
			if (err instanceof ProtocolViolation) throw err;
			const e = error(err);
			if (e === cancel) console.debug(`publish cancelled: broadcast=${name} track=${track.name}`);
			else console.warn(`publish error: broadcast=${name} track=${track.name} error=${reason(e)}`);
			stream.abort(e);
		} finally {
			// Groups still draining keep being ranked until they finish.
			ranking?.close();
			track.close();
			// Idempotent, and the backstop for a throw between the fork and the fill starting:
			// #runFill closes it itself on every path it reaches.
			cache?.close();
		}
	}

	/**
	 * Runs a group and sends its frames using ObjectStream (Subgroup delivery mode).
	 */
	async #runGroup(options: RunGroup) {
		const { requestId, track, group, timescale, publisherPriority, ranking, slice, unsubscribed, streams } =
			options;
		try {
			// One stream per group is faster than a peer at its limit can retire them, so this
			// is the one path that doesn't wait for a slot: the transport would serve the opens
			// in the order we asked, not the subscription's order. Failing here drops the group
			// and lets the next one compete for the next slot.
			const stream = await Writer.tryOpen(this.#quic, {
				cancel: unsubscribed,
				version: this.#session.version,
				sendOrder: ranking.rank(group.sequence, slice.skip),
				waitUntilAvailable: false,
			});
			if (!stream) {
				group.close(new Error("no stream slot"));
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

			try {
				await hooks.guardGroup(group, () => header.encode(stream, this.#session.version));
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
					const read = await race([hooks.readGroupFrame(group, slice.skip), stream.closed]);
					if (!read) break;
					next = read.sequence + 1;
					if (slice.until !== undefined && read.sequence >= slice.until) {
						read.complete();
						break;
					}

					try {
						const obj = new Frame({ payload: read.frame.payload, timestamp: read.frame.timestamp });
						const delta = first ? read.sequence : 0;
						first = false;
						await hooks.guardGroup(group, () =>
							obj.encode(stream, header.flags, timescale, this.#session.version, delta),
						);
					} finally {
						read.complete();
					}
				}

				// The track ends right after this group's last object, so the marker rides this
				// stream rather than a separate one.
				const end = track.end();
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
				// Only a stream that closed cleanly delivered the marker: one reset after its FIN
				// may not have, which leaves the standalone fallback in place, as in Rust.
				if (marked) {
					const delivered = stream.closed.then(
						() => true,
						() => false,
					);
					streams.terminalSent = (await race([delivered, unsubscribed.then(() => false)])) === true;
				}
			} catch (err: unknown) {
				stream.reset(error(err));
			} finally {
				// A closed stream may still have data queued behind its FIN, so it keeps
				// competing at its rank until the stream settles.
				void stream.closed.catch(() => {}).then(() => ranking.remove(stream));
			}
		} finally {
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
	}) {
		const writer = this.#datagramWriter;
		if (!writer) return;
		const { requestId, track, timescale, publisherPriority } = options;
		const version = this.#session.version;

		try {
			for (;;) {
				const datagram = await track.recvDatagram();
				if (!datagram) return;

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
		final: TrackLocation;
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
	 * Serve a draft-20 fill on its own fetch stream: the requested range, read from the
	 * group cache, capped at the Largest Object snapshot.
	 *
	 * A fill is a promise once requested. An empty range opens no stream, but a range we
	 * cannot serve still opens one and resets it right after the FETCH_HEADER, the draft's
	 * fill-failure signal. Nothing here touches the subscription either way.
	 */
	async #runFill(options: RunFill) {
		const { requestId, ranking, fill, cache, timescale, unsubscribed, streams } = options;
		const version = this.#session.version;

		// Everything is inside the try so the cache fork is released on every path out,
		// including a transport failure while opening the stream.
		let stream: Writer | undefined;
		try {
			if (fill.kind === "empty") return;

			// A fill we cannot serve is reset right after its header, so it needs no rank.
			const sequence = fill.kind === "group" ? Number(fill.sequence) : undefined;
			const object = fill.kind === "group" ? Number(fill.skip) : 0;
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

			await stream.u53(FetchHeader.type);
			await new FetchHeader({ requestId }).encode(stream, version);

			if (fill.kind !== "group") {
				throw new Error("a fill spanning several groups is not supported");
			}

			const group = takeGroup(cache, Number(fill.sequence));
			try {
				await this.#writeFillGroup(stream, group, fill, timescale, unsubscribed);
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
	 * Write one cached group's frames as draft-20 fetch objects.
	 *
	 * The cap is the Largest Object snapshot: the group may keep growing, but everything
	 * past the snapshot belongs to the subscription, not the fill.
	 */
	async #writeFillGroup(
		stream: Writer,
		group: group.Consumer,
		fill: FillGroup,
		timescale: Timescale | undefined,
		unsubscribed: Promise<void>,
	) {
		let first = true;
		let next = 0n;

		// The subscriber leaving has to end the fill too. An absolute filter ending below the
		// edge with no end object leaves `until` unset, so this reads until the group closes,
		// which a still-open past group may never do. Watching only the fetch stream would
		// then pin the group and the cache subscription behind it for the life of the track.
		let left = false;
		const cancelled = unsubscribed.then(() => {
			left = true;
			return undefined;
		});

		for (;;) {
			// The group may keep growing, but everything at `until` and beyond belongs to the
			// subscription, so stop without waiting for the group's end.
			if (fill.until !== undefined && next >= fill.until) break;

			// Reading from the fill's start drops everything below it; see the same read in #runGroup.
			const frame = await race([group.readFrameSequence({ from: Number(fill.skip) }), stream.closed, cancelled]);
			if (left) throw new Error("unsubscribed before the fill finished");
			if (!frame) break;
			next = BigInt(frame.sequence) + 1n;
			if (fill.until !== undefined && BigInt(frame.sequence) >= fill.until) break;

			const obj = new FetchFrame({ payload: frame.payload, timestamp: frame.timestamp });
			await obj.encode(
				stream,
				{ group: Number(fill.sequence), object: frame.sequence, first },
				timescale,
				this.#session.version,
			);
			first = false;
		}
	}

	/**
	 * Close the session when our origin publishes a broadcast our grant does not cover;
	 * see {@link enforceGrant}.
	 *
	 * @internal
	 */
	async runEnforce(setupAnswered: Promise<void>): Promise<void> {
		await enforceGrant({ quic: this.#quic, advertised: this.#advertised, grant: this.#grant, setupAnswered });
	}

	/**
	 * Handles an incoming SUBSCRIBE_NAMESPACE on a bidi stream.
	 *
	 * On draft-16 and later every visible match is a NAMESPACE on this stream, whatever
	 * the peer's SETUP said. A NAMESPACE is discovery, not a route, so a peer that also
	 * hears the unsolicited PUBLISH_NAMESPACE still gets the match here. Draft-14/15
	 * predate those messages, so each advertisement is a PUBLISH_NAMESPACE request, and
	 * only for what {@link runPublishNamespaces} does not already say (the whole set when
	 * the peer asked to be told on request, otherwise the hidden remainder).
	 *
	 * @internal
	 */
	runSubscribeNamespace(msg: SubscribeNamespace, stream: Stream): Promise<void> {
		return this.#withdrawal.track(this.#runSubscribeNamespace(msg, stream));
	}

	async #runSubscribeNamespace(msg: SubscribeNamespace, stream: Stream) {
		if (Namespace.reserved(msg.namespace, this.#session.version)) {
			await refuseReserved(stream, this.#session.version);
			return;
		}
		if (this.#withdrawal.closing.peek()) return;
		const version = this.#session.version;
		let prefix = msg.namespace;
		const legacy = version === Version.DRAFT_14 || version === Version.DRAFT_15;

		// Draft-14/15: the open PUBLISH_NAMESPACE request per advertised suffix.
		const requests: Requests = new Map();
		// Drains the requests map, so calling it twice is safe and the second is a no-op.
		const withdrawAll = async () => {
			for (const path of [...requests.keys()]) {
				await this.#withdraw(path, requests);
			}
		};

		const ending = cancelled(stream, version);
		const goaway = { client: this.#session.client, seen: false };
		const readUpdate = () =>
			namespaceUpdate(stream.reader, version, goaway).then(
				(update) => ({ update }),
				(err) => ({ error: error(err) }),
			);
		// Drafts 14-16 route an update onto the stream too, which stays open behind one left unread.
		let updating = version !== Version.DRAFT_17 ? readUpdate() : undefined;
		try {
			const answer = async (activeCount?: number) => {
				if (version === Version.DRAFT_14) {
					await stream.writer.u53(SubscribeNamespaceOk.id);
					const ok = new SubscribeNamespaceOk({ requestId: msg.requestId });
					await ok.encode(stream.writer, version);
					return;
				}
				const parameters = new Parameters();
				parameters.activeCount = activeCount;
				await stream.writer.u53(RequestOk.id);
				const ok = new RequestOk({
					requestId: version === Version.DRAFT_15 || version === Version.DRAFT_16 ? msg.requestId : undefined,
					parameters,
				});
				await ok.encode(stream.writer, version);
			};

			// MoQ Active Count: the OK counts what the first pass below sends, so it waits for
			// that pass. Otherwise answer now.
			let answered = !this.#activeCount;
			if (answered) await answer();

			// Hidden namespaces are left out unless the peer opted in (MoQ Hidden). Draft-16
			// and later carry every remaining match. Draft-14/15 only add what the
			// unsolicited loop cannot say.
			const visible = (covered: Path.Valid) => !this.#hidden || msg.hidden || !hiddenBelow(prefix, covered);
			const carries = (covered: Path.Valid) =>
				visible(covered) &&
				(legacy ? this.#requiresSolicitation || (this.#hidden && hiddenBelow(Path.empty(), covered)) : true) &&
				this.#gate.allows(covered);

			// Nothing is advertised before the grant it would be checked against.
			await this.#ready;

			// Inline entries always land, and the receiver treats a repeated NAMESPACE as a
			// replacement, so repricing one is just sending it again, a new original
			// publisher included, as Rust does. Only a legacy PUBLISH_NAMESPACE request can
			// be declined.
			const namespace = async (suffix: Path.Valid, snap: Advertised): Promise<Answer> => {
				await stream.writer.u53(SubscribeNamespaceEntry.id);
				// Either extension puts the Parameters field on NAMESPACE, even an empty one.
				const params = this.#advert !== undefined || this.#epoch ? this.#params(snap.route) : undefined;
				await new SubscribeNamespaceEntry({ suffix, params }).encode(stream.writer, version);
				return "held";
			};
			const target: Target = legacy
				? {
						advertise: (suffix, snap) => this.#advertise(Path.join(prefix, suffix), requests, snap.route),
						replace: (suffix, snap) => this.#replace(Path.join(prefix, suffix), requests, snap.route),
						update: (suffix, _snap, reprice) => this.#update(Path.join(prefix, suffix), requests, reprice),
						withdraw: (suffix) => this.#withdraw(Path.join(prefix, suffix), requests),
						held: (suffix) => requests.get(Path.join(prefix, suffix))?.auth,
						carry: (auth) => this.#carry(auth),
					}
				: {
						advertise: namespace,
						// A repeat under another epoch replaces the instance in place.
						replace: namespace,
						update: namespace,
						held: () => undefined,
						carry: () => undefined,
						withdraw: async (suffix) => {
							await stream.writer.u53(SubscribeNamespaceEntryDone.id);
							await new SubscribeNamespaceEntryDone({ suffix }).encode(stream.writer, version);
						},
					};

			// Keyed by suffix, as the entries name them.
			const ns: Namespaces = { active: new Map(), refused: new Map(), offered: new Map() };
			let retry = 0;

			for (;;) {
				// Subscribe BEFORE reconciling, for the same reason as
				// {@link runPublishNamespaces}: a publish landing while an advertisement
				// waits for its reply only notifies listeners already registered.
				// TODO Make a better helper within Signals.
				let dispose!: Dispose;
				const changed = new Promise<"changed">((resolve) => {
					const table = this.#advertised.changed(() => resolve("changed"));
					// A grant change re-diffs the same way, withdrawing what it no longer covers.
					// The loop top re-reads the table, so an ended origin still stops it there.
					const grant = this.#grant.changed(() => resolve("changed"));
					// A token that ended leaves the request carrying it to be made again without.
					const ended = this.#auth?.ended.changed(() => resolve("changed"));
					dispose = () => {
						table();
						grant();
						ended?.();
					};
				});

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
					// Drafts 14-16 spend the update's own Request ID in the adapter and name it in
					// the answer. They define no namespace update, so refusing one leaves the
					// subscription standing, and draft 14 answers no update at all.
					const keyed = version <= Version.DRAFT_16;
					if (!keyed) this.#session.spendRequest(update.requestId);
					if (update.refused || (update.prefix !== undefined && Namespace.reserved(update.prefix, version))) {
						if (version !== Version.DRAFT_14) {
							await stream.writer.u53(RequestError.id);
							await new RequestError({
								requestId: keyed ? update.requestId : undefined,
								errorCode: toRequestCode(
									update.refused ?? "does_not_exist",
									"subscribe_namespace",
									version,
								),
								reasonPhrase: "namespace update refused",
							}).encode(stream.writer, version);
						}
						if (!keyed) break;
						updating = readUpdate();
						continue;
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
					await stream.writer.u53(RequestOk.id);
					await new RequestOk({ requestId: undefined }).encode(stream.writer, version);
					updating = readUpdate();
				}
			}

			// Withdraw before the ask stream FINs, as Rust does: a peer that reads that
			// FIN as the subscription ending can drop the DONEs still in flight.
			await withdrawAll();
			stream.close();
			await stream.writer.closed;
		} catch (err: unknown) {
			const e = error(err);
			console.debug(`subscribe_namespace stream error: ${reason(e)}`);
			stream.abort(e);
			if (e instanceof ProtocolViolation || this.#withdrawal.closing.peek()) throw e;
		} finally {
			// This subscription's advertisements die with it.
			await withdrawAll();
			if (updating !== undefined) {
				stream.reader.stop(new Error("namespace subscription ended"));
				await updating;
			}
		}
	}

	/**
	 * Advertise every published broadcast with an unsolicited PUBLISH_NAMESPACE, until
	 * the publisher is closed.
	 *
	 * The peers that never send SUBSCRIBE_NAMESPACE are exactly the ones expecting a
	 * publisher to announce itself, so announcing is the default. A peer that would
	 * rather ask says so in its SETUP (MoQ Solicit) and this does nothing. On draft-16
	 * and later {@link runSubscribeNamespace} also carries every match, so a peer that
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
			let retry = 0;

			for (;;) {
				// Subscribe BEFORE reconciling. Each advertisement below waits a round trip
				// for the peer's reply, and a publish that lands in that window notifies
				// only the listeners already registered; one created afterwards would sleep
				// through it and leave the namespace unadvertised until something unrelated
				// changed.
				// TODO Make a better helper within Signals.
				const changed = new Promise<"changed">((resolve) => {
					const table = this.#advertised.changed(() => resolve("changed"));
					// A grant change re-diffs the same way, withdrawing what it no longer covers.
					// The loop top re-reads the table, so an ended origin still stops it there.
					const grant = this.#grant.changed(() => resolve("changed"));
					// A token that ended leaves the request carrying it to be made again without.
					const ended = this.#auth?.ended.changed(() => resolve("changed"));
					dispose = () => {
						table();
						grant();
						ended?.();
					};
				});

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

				const outstanding = await this.#reconcile(ns, updated, target);
				retry = outstanding ? Math.min(retry ? retry * 2 : RETRY_BASE, RETRY_MAX) : 0;

				// Wait for the next change, which has already fired if one landed above.
				const next = await (retry
					? race([changed, this.#withdrawal.closing, closed, retryAfter(retry).then(() => advertised)])
					: race([changed, this.#withdrawal.closing, closed]));
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
	 * request that had none, nor drop one, and before draft-17 cannot update a namespace at
	 * all: those withdraw the request and make it again.
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
		const version = this.#session.version;
		const legacy = version === Version.DRAFT_14 || version === Version.DRAFT_15 || version === Version.DRAFT_16;
		if (from === undefined || to === undefined || legacy) return "restart";
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
	#carry(auth: Auth | undefined): Auth | undefined {
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
	 * namespace.
	 *
	 * Draft-14/15 carry no such field, so a 0 there says nothing and our own backoff
	 * stands; everywhere else 0 means the peer does not want this again.
	 */
	#refusal(err: RequestError): Answer {
		const version = this.#session.version;
		if (version === Version.DRAFT_14 || version === Version.DRAFT_15) return "dropped";
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
		let request: Stream | undefined;
		try {
			const deadline = new Deadline();
			const answer = await deadline.wait(
				(async (): Promise<Answer> => {
					request = await deadline.open(this.#session.openBi(requestOptions));
					const stream = request;
					await stream.writer.u53(PublishNamespace.id);
					const token = this.#auth?.carried(auth);
					const msg = new PublishNamespace({ requestId, trackNamespace: path, params, token });
					await msg.encode(stream.writer, this.#session.version);

					// Read response (RequestOk and PublishNamespaceOk share 0x07)
					const respTypeId = await stream.reader.u53();
					if (respTypeId === RequestError.id) {
						const err = await RequestError.decode(stream.reader, this.#session.version);
						const why = requestReason(
							err.errorCode,
							err.reasonPhrase,
							"publish_namespace",
							this.#session.version,
						);
						console.warn(`announce refused: broadcast=${path} error=${why}`);
						stream.abort(new Error(`PublishNamespace rejected: ${why}`));
						return this.#refusal(err);
					}
					if (respTypeId !== RequestOk.id) {
						throw new Error(`PublishNamespace rejected: typeId=0x${respTypeId.toString(16)}`);
					}
					// Draft-14 sends PublishNamespaceOk (requestId only, no parameters)
					if (this.#session.version === Version.DRAFT_14) {
						await PublishNamespaceOk.decode(stream.reader, this.#session.version);
					} else {
						this.#answered(await RequestOk.decode(stream.reader, this.#session.version));
					}
					return "held";
				})(),
			);

			if (answer === "held" && request) requests.set(path, { path, requestId, stream: request, auth });
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
	 * Check a REQUEST_OK answering a PUBLISH_NAMESPACE. ACTIVE_COUNT only answers
	 * SUBSCRIBE_NAMESPACE (MoQ Active Count), and the session closes over it here, with the
	 * violation's code, since the caller only drops the advertisement.
	 */
	#answered(ok: RequestOk) {
		try {
			// Reading it throws on a count too large to hold, a violation all the same.
			if (ok.parameters.activeCount === undefined) return;
			throw new ProtocolViolation("ACTIVE_COUNT on a PUBLISH_NAMESPACE answer");
		} catch (err) {
			if (err instanceof ProtocolViolation) {
				this.#quic.close({ closeCode: err.code, reason: err.message });
				this.#session.close();
			}
			throw err;
		}
	}

	/**
	 * Update a namespace the peer holds: REQUEST_UPDATE on the request that carries it,
	 * with only the parameters that changed, then its answer.
	 *
	 * Waiting for the answer keeps one update outstanding per stream, which satisfies any
	 * MAX_REQUEST_UPDATES the peer set without reading it; a change landing meanwhile waits
	 * for the next turn. The answer decides whether the peer still holds the namespace: a
	 * REQUEST_ERROR closes the peer's side and withdraws the advertisement, so ours is
	 * finished too, and an unanswered update is dropped abruptly, since a peer that ignored
	 * it cannot be assumed to hold either price. Either way the retry re-offers it fresh.
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
					const { writer, reader } = request.stream;
					await writer.u53(PublishNamespaceUpdate.id);
					await new PublishNamespaceUpdate({ requestId, update, token }).encode(writer, version);

					const typeId = await reader.u53();
					if (typeId === RequestError.id) {
						const err = await RequestError.decode(reader, version);
						const why = requestReason(err.errorCode, err.reasonPhrase, "publish_namespace", version);
						console.warn(`publish_namespace update refused: broadcast=${path} error=${why}`);
						return this.#refusal(err);
					}
					if (typeId !== RequestOk.id) {
						throw new Error(`unexpected answer to the update: typeId=0x${typeId.toString(16)}`);
					}
					this.#answered(await RequestOk.decode(reader, version));
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

		if (answer === "held" && auth) request.auth = auth;
		// Before draft-17 the peer already closed out a refused request, and its STOP_SENDING
		// can reject the wait on our FIN. That is the withdrawal finishing, not a reason to end
		// the loop.
		if (answer !== "held") await this.#withdraw(path, requests).catch(() => undefined);
		return answer;
	}

	/**
	 * End a namespace's PUBLISH_NAMESPACE request: PUBLISH_NAMESPACE_DONE then FIN before
	 * draft-17, a cancel in both directions after.
	 */
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

	/** Withdraw one PUBLISH_NAMESPACE request, already out of its map. */
	async #end(request: NamespaceRequest) {
		// Draft-17+ withdraws by cancelling the request in both directions; a FIN is only
		// allowed to end SUBSCRIBE_NAMESPACE and SUBSCRIBE_TRACKS.
		const version = this.#session.version;
		if (version !== Version.DRAFT_14 && version !== Version.DRAFT_15 && version !== Version.DRAFT_16) {
			request.stream.abort(new StreamError(StreamCode.Cancel, { message: "withdrawn" }));
			return;
		}

		try {
			await request.stream.writer.u53(PublishNamespaceDone.id);
			const done = new PublishNamespaceDone({ trackNamespace: request.path, requestId: request.requestId });
			await done.encode(request.stream.writer, version);
		} catch {
			// Stream might already be closed
		}
		request.stream.close();
		await request.stream.writer.closed;
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
	async runTrackStatusRequest(msg: TrackStatusRequest, stream: Stream) {
		this.#pinned(msg.epoch);
		if (Namespace.reserved(msg.trackNamespace, this.#session.version)) {
			await refuseReserved(stream, this.#session.version);
			return;
		}
		const version = this.#session.version;
		const name = msg.trackNamespace;
		const denied: Refusal = {
			errorCode: toRequestCode("unauthorized", "track_status", version),
			reasonPhrase: "not granted",
		};

		// Answer only for what our grant lets us publish. Checked before the track resolves,
		// so a denied request never reaches the origin.
		if (!this.#gate.allows(name)) {
			await this.#refuse(stream, msg.requestId, TRACK_STATUS_ERROR_14, denied);
			return;
		}

		// Holds the track wanted until the answer is out. The requester leaving is owed
		// nothing, and lets go of the broadcast and track lookups.
		const hold = new AbortController();
		const cancel = new StreamError(StreamCode.Cancel, { message: "cancel" });
		void cancelled(stream, version).then(() => hold.abort(cancel));

		// A grant that narrows abandons the lookup, and resets an answer the transport has
		// not taken yet rather than delivering it.
		let answering = false;
		const watch = this.#gate.watch(name, () => {
			hold.abort();
			if (answering) stream.abort(unauthorized(name));
		});

		try {
			let answer: TrackStatusOk;
			try {
				const broadcast = await this.#resolve(name, hold.signal, msg.epoch);
				const info = await wireOf(broadcast).resolveTrackInfo(msg.trackName, hold.signal);
				const largest = wireOf(broadcast).largest(msg.trackName);
				answer = new TrackStatusOk({
					requestId:
						version === Version.DRAFT_14 || version === Version.DRAFT_15 || version === Version.DRAFT_16
							? msg.requestId
							: undefined,
					largest: largest && { groupId: BigInt(largest.group), objectId: BigInt(largest.frame) },
					properties: msg.propertiesWanted
						? {
								timescale: info.timescale ?? undefined,
								priority: toWire(info.priority),
								groupOrder: Properties.DESCENDING,
								maxCacheDuration: info.maxAge === undefined ? undefined : BigInt(info.maxAge),
							}
						: {},
				});
			} catch (err: unknown) {
				if (hold.signal.reason === cancel) {
					stream.abort(cancel);
					return;
				}
				const refusal = !this.#gate.allows(name) ? denied : this.#failed(err, "track_status");
				await this.#refuse(stream, msg.requestId, TRACK_STATUS_ERROR_14, refusal);
				return;
			}

			answering = true;
			await stream.writer.u53(TrackStatusOk.id(version));
			await answer.encode(stream.writer, version);
			// The answer is all this stream carries.
			stream.close();
			await stream.writer.closed.catch(() => undefined);
		} finally {
			watch.dispose();
			hold.abort();
		}
	}

	/**
	 * Refuses a draft-16/17 SUBSCRIBE_NAMESPACE that asks for PUBLISH alone: we never send PUBLISH.
	 *
	 * @internal
	 */
	async refuseSubscribeNamespace(requestId: bigint, stream: Stream) {
		await this.#refuseUnsupported(
			stream,
			requestId,
			"subscribe_namespace",
			SubscribeNamespaceError.id,
			"SUBSCRIBE_NAMESPACE for PUBLISH is not supported",
		);
	}

	/**
	 * Refuses an incoming SUBSCRIBE_TRACKS (draft-18+): we never send the PUBLISH it asks for.
	 * The reply carries no Request ID, so the message body is left unread.
	 *
	 * @internal
	 */
	async runSubscribeTracks(stream: Stream, namespace?: Path.Valid) {
		if (namespace !== undefined && Namespace.reserved(namespace, this.#session.version)) {
			await refuseReserved(stream, this.#session.version);
			return;
		}
		const version = this.#session.version;
		await stream.writer.u53(RequestError.id);
		await new RequestError({
			errorCode: toRequestCode("not_supported", "subscribe_namespace", version),
			reasonPhrase: "SUBSCRIBE_TRACKS is not supported",
		}).encode(stream.writer, version);
		stream.close();
	}

	/**
	 * Handles an incoming FETCH on a bidi stream. We serve none.
	 *
	 * @internal
	 */
	async runFetch(msg: Fetch, stream: Stream) {
		this.#pinned(msg.epoch);
		this.#fetches.acquire();
		try {
			if (msg.trackNamespace !== undefined && Namespace.reserved(msg.trackNamespace, this.#session.version)) {
				await refuseReserved(stream, this.#session.version);
				return;
			}
			if (msg.rangeFilters) {
				await this.#refuse(stream, msg.requestId, FetchError.id, {
					errorCode: toRequestCode("invalid_filter", "fetch", this.#session.version),
					reasonPhrase: "Range Filters exceed MAX_FILTER_RANGES=0",
				});
				return;
			}
			await this.#refuseUnsupported(stream, msg.requestId, "fetch", FetchError.id, "FETCH is not supported");
		} finally {
			this.#fetches.release();
		}
	}

	/** Refuse a request NOT_SUPPORTED. */
	async #refuseUnsupported(
		stream: Stream,
		requestId: bigint,
		kind: RequestKind,
		errorId14: number,
		reasonPhrase: string,
	) {
		const errorCode = toRequestCode("not_supported", kind, this.#session.version);
		await this.#refuse(stream, requestId, errorId14, { errorCode, reasonPhrase });
	}

	/**
	 * Refuse a request and end its stream. Draft-14 gives each request its own error
	 * message, `errorId14`, with the SUBSCRIBE_ERROR body; later drafts use REQUEST_ERROR.
	 */
	async #refuse(stream: Stream, requestId: bigint, errorId14: number, { errorCode, reasonPhrase }: Refusal) {
		const version = this.#session.version;
		if (version === Version.DRAFT_14) {
			await stream.writer.u53(errorId14);
			await new SubscribeError({ requestId, errorCode, reasonPhrase }).encode(stream.writer, version);
		} else {
			await stream.writer.u53(RequestError.id);
			await new RequestError({
				requestId: version === Version.DRAFT_15 || version === Version.DRAFT_16 ? requestId : undefined,
				errorCode,
				reasonPhrase,
			}).encode(stream.writer, version);
		}
		stream.close();
	}

	/** What a request that failed to resolve is refused with. */
	#failed(err: unknown, kind: RequestKind, wait = false): Refusal {
		const e = error(err);
		const condition =
			e instanceof StreamError && (e.code === StreamCode.NotFound || e.code === StreamCode.Unroutable)
				? wait
					? "timeout"
					: "does_not_exist"
				: "internal";
		return { errorCode: toRequestCode(condition, kind, this.#session.version), reasonPhrase: reason(e) };
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

/** A request's refusal: its code on this draft, and why. */
interface Refusal {
	errorCode: number;
	reasonPhrase: string;
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
	 * once the range is exhausted; draft-20 removed the notion of a filter ending one.
	 */
	end?: EndLocation;
}

/** The slice of one group a subscription's {@link ServeRange} selects. */
interface GroupSlice {
	/** Objects dropped from the front; also the first written object's absolute id. */
	skip: number;

	/** One past the last object to write, when the filter ends inside this group. */
	until?: number;
}

/** A fill resolved to a single group of the cache. */
interface FillGroup {
	kind: "group";
	/** The group to serve. */
	sequence: bigint;
	/** Objects dropped from the front. */
	skip: bigint;
	/** One past the last object to write, capping the fill at the Largest Object snapshot. */
	until?: bigint;
}

/** What a draft-20 fill request resolves to. */
type FillServe =
	/** The range is empty, so no fetch stream is opened at all. */
	| { kind: "empty" }
	| FillGroup
	/**
	 * A range spanning several groups, which we do not serve: multi-group fetch
	 * serialization depends on a negotiated group order we do not implement, so the stream is
	 * reset instead, the draft's fill-failure signal.
	 */
	| { kind: "unsupported" };

/**
 * Take one group out of a cache view, without waiting.
 *
 * A fill's group is at or below the Largest Object snapshot, so it is either still in the
 * retained window or gone for good: nothing republishes an old sequence, and waiting for one
 * would park for the life of the track. Scanning past it, or running dry, is a cache miss,
 * which the caller turns into the draft's fill-failure reset.
 */
function takeGroup(cache: TrackSubscriber, sequence: number): group.Consumer {
	for (;;) {
		const group = cache.tryRecvGroup();
		if (!group) throw new Error(`group not found: ${sequence}`);
		if (group.sequence === sequence) return group;

		group.close();
		if (group.sequence > sequence) throw new Error(`group not found: ${sequence}`);
	}
}

/** `a - b`, saturating at zero the way the draft's unsigned arithmetic does. */
function saturatingSub(a: bigint, b: bigint): bigint {
	return a > b ? a - b : 0n;
}

/** Snapshot the live edge of a track. */
function liveEdge(track: TrackSubscriber): LiveEdge {
	const latest = hooks.latestSequence(track);
	if (latest === undefined) return {};

	const largest = hooks.bufferedLargest(track);
	if (!largest) return { latest: BigInt(latest), next: { group: BigInt(latest), object: 0n } };

	const group = BigInt(largest.group);
	const object = BigInt(largest.frame);
	return { latest: BigInt(latest), largest: { group, object }, next: { group, object: object + 1n } };
}

/**
 * Resolve a SUBSCRIBE's Location Filter into the range to serve.
 *
 * Only draft-20 is honored. Earlier drafts have a Filter Type tag whose absolute forms we
 * never served, and starting to interpret them now would change what an existing peer
 * receives; draft-20 is also the first version whose relative forms can name a past group
 * without the subscriber knowing Largest Object.
 */
function subscribeRange(msg: Subscribe, edge: LiveEdge, version: IetfVersion): ServeRange {
	if (!Filter.isDraft20(version)) {
		if (msg.filter.kind !== "nextObject" && msg.filter.kind !== "unfiltered") {
			console.warn(`filter not supported before draft-20, ignoring: ${msg.filter.kind}`);
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
		// mid-group by construction. The draft pairs this with a fill when the subscriber
		// wants the head; see {@link Publisher.runFill}.
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

/**
 * Resolve a fill request using the Fetch rules: relative to Largest Object and never
 * extending beyond it. An omitted Location Filter inherits the subscription's.
 */
function fillRange(fill: Filter.Fill, subscription: Filter.Filter, largest?: Location): FillServe {
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
	if (start.group !== end.group) return { kind: "unsupported" };

	return {
		kind: "group",
		sequence: start.group,
		skip: start.object,
		until: end.object === undefined ? undefined : end.object + 1n,
	};
}

class UpdateFailed extends Error {
	constructor() {
		super("subscription update failed");
	}
}
