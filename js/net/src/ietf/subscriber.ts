import * as announce from "../announced.ts";
import * as broadcast from "../broadcast.ts";
import { BroadcastCache } from "../consume.ts";
import { error, ProtocolViolation, reason } from "../error.ts";
import * as netGroup from "../group.ts";
import * as Path from "../path.ts";
import type { Reader, Stream } from "../stream.ts";
import { type Timescale, Timestamp } from "../time.ts";
import type * as track from "../track.ts";
import { withTimeout } from "../util/timeout.ts";
import type { Session } from "./adapter.ts";
import { TrackAliases } from "./aliases.ts";
import * as Cluster from "./cluster.ts";
import { Frame, type Group as GroupMessage } from "./object.ts";
import { toWire } from "./priority.ts";
import { type Publish, PublishError } from "./publish.ts";
import {
	PublishNamespace,
	PublishNamespaceDone,
	PublishNamespaceError,
	PublishNamespaceOk,
} from "./publish_namespace.ts";
import { RequestError, RequestOk } from "./request.ts";
import { Subscribe, SubscribeError, SubscribeOk, Unsubscribe } from "./subscribe.ts";
import {
	PublishBlocked,
	SubscribeNamespace,
	SubscribeNamespaceEntry,
	SubscribeNamespaceEntryDone,
	SubscribeNamespaceLegacy,
	SubscribeNamespaceOk,
	UnsubscribeNamespace,
} from "./subscribe_namespace.ts";
import { Version } from "./version.ts";

// Bound on how long stream-open plus SUBSCRIBE_OK may take. Browsers cap
// concurrent QUIC streams (Chrome ~100); past the cap openBi() silently
// blocks. The timeout turns that into a clear error.
const SUBSCRIBE_OK_TIMEOUT_MS = 10_000;

// Out-parameter for #openSubscribe: lets the caller observe partial progress
// (stream opened, trackAlias registered) so it can clean up on timeout even
// before the setup promise settles.
type SubscribeSetupState = {
	stream?: Stream;
	registeredAlias?: bigint;
};

/**
 * Handles subscribing to broadcasts using moq-transport protocol.
 * Uses the stream-per-request pattern (real bidi streams for v17, virtual for v14-v16).
 *
 * @internal
 */
export class Subscriber {
	#session: Session;

	// The Hop IDs this session declared; see {@link Cluster}. What the peer declared is what
	// says whether an advertisement carries a hop path, and ours is what a path looping back
	// to us contains.
	#cluster?: Cluster.Hops;

	// Publisher-chosen aliases used by incoming group streams.
	#aliases = new TrackAliases<track.Producer>();

	// Units for each track's object Timestamps, from the TIMESCALE Track Property in
	// SUBSCRIBE_OK. A track missing from this map declared no timeline, so the publisher
	// opted out of timestamps and its frames are stamped on arrival instead.
	#timescales = new Map<bigint, Timescale>();

	// Dedup consumed broadcasts per path: repeat consume() calls share one subscription.
	#consumes = new BroadcastCache();

	// Paths with a legacy PUBLISH_NAMESPACE request in flight, reserved synchronously.
	// The count below is only taken once the OK is written, and two requests that both
	// got past the duplicate check before either attached would both take one.
	#legacyRequests = new Set<Path.Valid>();

	// Every announced path, counted by how many live advertisements reference it.
	//
	// A peer may advertise one namespace twice on a session: an unsolicited
	// PUBLISH_NAMESPACE and an inline NAMESPACE answering our own SUBSCRIBE_NAMESPACE are
	// two messages about one source, which the MoQ Solicit draft requires us to tolerate.
	// Counting them is what keeps the second from duplicating the announce and the first
	// to end from retracting what the other still holds.
	#announced = new Map<Path.Valid, number>();

	// Any consumers that want each new announcement.
	#announcedConsumers = new Set<announce.Producer>();

	/**
	 * Creates a new Subscriber instance.
	 *
	 * @internal
	 */
	constructor({
		session,
		cluster,
	}: {
		/** The session abstraction for bidi streams and request IDs. */
		session: Session;
		/** The Hop IDs the SETUP exchange settled (MoQ Cluster). */
		cluster?: Cluster.Hops;
	}) {
		this.#session = session;
		this.#cluster = cluster;
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
	 * Gets an announced reader for the specified prefix.
	 *
	 * The peer is asked with SUBSCRIBE_NAMESPACE regardless of what it declared, and an
	 * unsolicited PUBLISH_NAMESPACE lands here too, so a peer that only tells and one
	 * that only answers are both discovered.
	 */
	announced(prefix = Path.empty()): announce.Consumer {
		const announced = new announce.Producer(prefix);
		for (const active of this.#announced.keys()) {
			const suffix = Path.stripPrefix(prefix, active);
			if (suffix === null) continue;
			announced.append({ path: suffix, active: true });
		}
		this.#announcedConsumers.add(announced);

		void this.#runAnnounced(announced, prefix).finally(() => {
			this.#announcedConsumers.delete(announced);
			announced.close();
		});

		return announced.consume();
	}

	/**
	 * Record one more advertisement for a path, telling consumers only when it is the
	 * first. A second one is the same namespace said twice, not news.
	 */
	#attachAnnounce(path: Path.Valid) {
		const count = this.#announced.get(path) ?? 0;
		this.#announced.set(path, count + 1);
		if (count > 0) return;

		console.debug(`announced: broadcast=${path} active=true`);
		for (const consumer of this.#announcedConsumers) {
			const suffix = Path.stripPrefix(consumer.prefix, path);
			if (suffix === null) continue;
			consumer.append({ path: suffix, active: true });
		}
	}

	/**
	 * Drop one advertisement for a path, retracting it only once the last one goes.
	 */
	#detachAnnounce(path: Path.Valid) {
		const count = this.#announced.get(path);
		if (count === undefined) return;
		if (count > 1) {
			this.#announced.set(path, count - 1);
			return;
		}

		this.#announced.delete(path);

		// The path is gone, so stop sharing its broadcast: a holder outliving the publisher
		// would otherwise hand the dead generation to whoever consumes the path next.
		this.#consumes.evict(path);
		console.debug(`announced: broadcast=${path} active=false`);

		for (const consumer of this.#announcedConsumers) {
			const suffix = Path.stripPrefix(consumer.prefix, path);
			if (suffix === null) continue;
			try {
				consumer.append({ path: suffix, active: false });
			} catch {
				// Consumer already closed, will be cleaned up
			}
		}
	}

	async #runAnnounced(announced: announce.Producer, prefix: Path.Valid) {
		const version = this.#session.version;

		// Suffixes live on this stream, so a repeat is recognized as an update to the
		// advertisement rather than a second one, which would leak the count.
		const live = new Set<Path.Valid>();

		// Set once the teardown below has given back everything this stream held. The read
		// loop is not awaited when the local consumer closes first, and closing the stream
		// cancels the transport without discarding what the reader already buffered, so a
		// fully buffered entry can still decode afterwards. Attaching one then would take a
		// reference nobody is left to release, pinning the path for the session.
		let released = false;

		// v14/v15: SubscribeNamespace on control stream (via adapter virtual stream)
		// v16+: SubscribeNamespace on its own real bidi stream

		const requestId = await this.#session.nextRequestId();
		if (requestId === undefined) return;

		try {
			// v16: use a real bidi stream (not virtual control stream)
			const stream =
				version === Version.DRAFT_16 && this.#session.openNativeBi
					? await this.#session.openNativeBi()
					: await this.#session.openBi();

			try {
				// Draft-18+ uses SUBSCRIBE_NAMESPACE (0x50); earlier drafts use the
				// legacy 0x11 message with a Subscribe Options field.
				if (
					version === Version.DRAFT_14 ||
					version === Version.DRAFT_15 ||
					version === Version.DRAFT_16 ||
					version === Version.DRAFT_17
				) {
					await stream.writer.u53(SubscribeNamespaceLegacy.id);
					await new SubscribeNamespaceLegacy({ namespace: prefix, requestId }).encode(stream.writer, version);
				} else {
					await stream.writer.u53(SubscribeNamespace.id);
					await new SubscribeNamespace({ namespace: prefix, requestId }).encode(stream.writer, version);
				}
				console.debug(`subscribe_namespace written: requestId=${requestId}`);

				// Read response
				const respTypeId = await stream.reader.u53();
				if (respTypeId === RequestOk.id) {
					await RequestOk.decode(stream.reader, version);
				} else if (respTypeId === SubscribeNamespaceOk.id) {
					// v14: SubscribeNamespaceOk
					const size = await stream.reader.u16();
					await stream.reader.read(size);
				} else {
					throw new Error(`SubscribeNamespace rejected: typeId=0x${respTypeId.toString(16)}`);
				}

				// Loop reading Namespace/NamespaceDone entries
				const readLoop = (async () => {
					for (;;) {
						const done = await stream.reader.done();
						if (done) break;

						const msgType = await stream.reader.u53();
						if (msgType === SubscribeNamespaceEntry.id) {
							const entry = await SubscribeNamespaceEntry.decode(
								stream.reader,
								version,
								Cluster.negotiated(this.#cluster),
							);
							if (released) break;
							const path = Path.join(prefix, entry.suffix);

							// A repeat updates the advertisement in place, so one that now
							// loops back through us has taken a route we can't subscribe
							// over: the path is gone even though the message says active.
							if (this.#reflected(entry.cluster)) {
								console.debug(`dropping reflected namespace: broadcast=${path}`);
								if (live.delete(path)) this.#detachAnnounce(path);
								continue;
							}

							// A repeat updates the advertisement; only the first is news.
							if (!live.has(path)) {
								live.add(path);
								this.#attachAnnounce(path);
							}
						} else if (msgType === SubscribeNamespaceEntryDone.id) {
							const entry = await SubscribeNamespaceEntryDone.decode(stream.reader, version);
							if (released) break;
							const path = Path.join(prefix, entry.suffix);

							if (live.delete(path)) {
								this.#detachAnnounce(path);
							}
						} else if (msgType === PublishBlocked.id && version === Version.DRAFT_17) {
							const blocked = await PublishBlocked.decode(stream.reader, version);
							console.debug(`publish_blocked: suffix=${blocked.suffix} track=${blocked.trackName}`);
						} else {
							throw new Error(
								`unexpected message on subscribe_namespace stream: 0x${msgType.toString(16)}`,
							);
						}
					}
				})();

				// The race below can end through the consumer's close while a message that
				// already arrived is still decoding, which leaves the loop with nothing
				// awaiting it: a violation decoded after that would land nowhere, and its
				// rejection would go unhandled. Idempotent with the catch below, which is
				// what runs when the loop is the one that ends the race.
				readLoop.catch((err: unknown) => {
					if (err instanceof ProtocolViolation) this.#session.close();
				});

				// Wait for either the read loop or the announced to close
				await Promise.race([readLoop, announced.closed]);

				// For v14/v15: send UnsubscribeNamespace before closing
				if (version === Version.DRAFT_14 || version === Version.DRAFT_15) {
					try {
						await stream.writer.u53(UnsubscribeNamespace.id);
						const unsub = new UnsubscribeNamespace({ requestId });
						await unsub.encode(stream.writer, version);
					} catch {
						// Stream might already be closed
					}
				}

				stream.close();
			} catch (err) {
				stream.abort(error(err));
				throw err;
			}
		} catch (err: unknown) {
			const e = error(err);
			console.warn(`subscribe_namespace error: ${reason(e)}`);

			// An advertisement is decoded here rather than in the session dispatch, so this
			// is the only place a malformed one surfaces. The cluster draft requires closing
			// the session over those, and the stream alone is not enough: the peer would
			// just repeat it on the next SUBSCRIBE_NAMESPACE.
			if (e instanceof ProtocolViolation) this.#session.close();

			// Abort the stream rather than letting the caller's `finally` close it cleanly.
			// A rejected namespace subscription is a failure, and a consumer that can't tell
			// it from "nothing is published under this prefix" waits forever on a broadcast
			// that will never be announced. Matches the lite subscriber.
			announced.close(e);
		} finally {
			// The stream owns every advertisement it carried, so release them however it
			// ends: a clean close, a decode error, or the peer resetting it. Without this
			// each namespace keeps its count and the source never detaches, which would
			// pin the path for the session even after the other source withdrew.
			released = true;
			for (const path of live) {
				this.#detachAnnounce(path);
			}
			live.clear();
		}
	}

	/**
	 * Consumes a broadcast from the connection.
	 *
	 * Deduplicated per path: repeat calls for the same still-live path share one reference-counted
	 * broadcast (and one upstream subscription). The shared broadcast closes once every caller has
	 * closed its handle, so callers close normally.
	 */
	consume(path: Path.Valid): broadcast.Consumer {
		return this.#consumes.get(path) ?? this.#consumes.insert(path, this.#createConsume(path));
	}

	#createConsume(path: Path.Valid): broadcast.Consumer {
		// moq-transport has no one-shot group fetch; ConsumeBroadcast rejects it. Track info
		// is resolved by the subscribe path (the inherited resolveTrackInfo).
		const consumer = new ConsumeBroadcast();

		void (async () => {
			for (;;) {
				const request = await consumer.requested();
				if (!request) break;
				void this.#runSubscribe(path, request);
			}
		})();

		return consumer;
	}

	async #runSubscribe(broadcast: Path.Valid, request: track.Request) {
		const version = this.#session.version;
		const requestId = await this.#session.nextRequestId();
		if (requestId === undefined) {
			request.reject(new Error("session closed"));
			return;
		}

		console.debug(`subscribe start: id=${requestId} broadcast=${broadcast} track=${request.name}`);

		// IETF negotiates group order in SUBSCRIBE_OK; this implementation only
		// supports descending (newest-first), so commit ordered: false. (There's no
		// per-frame timescale, so the rest stay at their defaults.) This
		// resolves the consumer's track.info() and gives us the write side that
		// incoming object streams are routed into.
		const producer = request.accept({ ordered: false });

		// Open the stream and wait for SUBSCRIBE_OK under a timeout. State
		// flows back via `state` so the timeout path can clean up the stream
		// and any registration if setup eventually finishes.
		const state: SubscribeSetupState = {};
		const setup = this.#openSubscribe(state, broadcast, request, producer, requestId);

		let stream: Stream;
		let trackAlias: bigint;
		try {
			const result = await withTimeout(
				setup,
				SUBSCRIBE_OK_TIMEOUT_MS,
				`subscribe timed out after ${SUBSCRIBE_OK_TIMEOUT_MS}ms waiting for SUBSCRIBE_OK (browser stream limit reached?)`,
			);
			stream = result.stream;
			trackAlias = result.alias;
			console.debug(`subscribe ok: id=${requestId} broadcast=${broadcast} track=${request.name}`);
		} catch (err) {
			const e = error(err);
			producer.close(e);
			console.warn(
				`subscribe error: id=${requestId} broadcast=${broadcast} track=${request.name} error=${reason(e)}`,
			);
			// If setup eventually settles after the timeout, abort the stream
			// and drop any registration so we don't leak. Cover both branches:
			// setup may resolve late, or reject (e.g. SUBSCRIBE error) after the
			// stream is already open.
			const cleanup = () => {
				if (state.registeredAlias !== undefined) this.#aliases.delete(state.registeredAlias, producer);
				state.stream?.abort(e);
			};
			setup.then(cleanup, cleanup);
			return;
		}

		try {
			// Terminal conditions settle at most once (stream close = PublishDone, track close =
			// local unsubscribe); race them once so the demand loop doesn't re-subscribe each pass.
			const done = Promise.race([stream.reader.closed, producer.closed]);

			// Serve until a terminal condition fires or the last local subscriber leaves. The unused
			// wake is level-triggered: re-check demand so a subscriber that returns before we tear
			// down resumes on the same stream.
			const idle = Symbol("idle");
			for (;;) {
				const reason = await Promise.race([done, producer.unused().then(() => idle)]);
				if (reason === idle && producer.closed.peek() === undefined && producer.used.peek()) continue;
				break;
			}

			// For v14-v16: send Unsubscribe before closing (removed in v17+)
			if (version === Version.DRAFT_14 || version === Version.DRAFT_15 || version === Version.DRAFT_16) {
				try {
					await stream.writer.u53(Unsubscribe.id);
					const unsub = new Unsubscribe({ requestId });
					await unsub.encode(stream.writer, version);
				} catch {
					// Stream might already be closed
				}
			}

			producer.close();
			stream.close();
			console.debug(`subscribe close: id=${requestId} broadcast=${broadcast} track=${request.name}`);
		} catch (err) {
			const e = error(err);
			producer.close(e);
			stream.abort(e);
			console.warn(
				`subscribe error: id=${requestId} broadcast=${broadcast} track=${request.name} error=${reason(e)}`,
			);
		} finally {
			this.#aliases.delete(trackAlias, producer);
			this.#timescales.delete(trackAlias);
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
		producer: track.Producer,
		requestId: bigint,
	): Promise<{ stream: Stream; alias: bigint }> {
		const version = this.#session.version;

		state.stream = await this.#session.openBi();

		await state.stream.writer.u53(Subscribe.id);
		const msg = new Subscribe({
			requestId,
			trackNamespace: broadcast,
			trackName: request.name,
			subscriberPriority: toWire(request.priority),
		});
		await msg.encode(state.stream.writer, version);
		console.debug(`subscribe written: id=${requestId} broadcast=${broadcast} track=${request.name}`);

		const respTypeId = await state.stream.reader.u53();
		if (respTypeId !== SubscribeOk.id) {
			let reasonPhrase = "unknown error";
			try {
				if (respTypeId === RequestError.id) {
					const err =
						version === Version.DRAFT_14
							? await SubscribeError.decode(state.stream.reader, version)
							: await RequestError.decode(state.stream.reader, version);
					reasonPhrase = `code=${err.errorCode} reason=${err.reasonPhrase}`;
				}
			} catch {
				// Decoding error response failed, use default message
			}
			throw new Error(`SUBSCRIBE error: ${reasonPhrase}`);
		}

		const ok = await SubscribeOk.decode(state.stream.reader, version);
		try {
			this.#aliases.set(ok.trackAlias, producer);
			if (ok.timescale !== undefined) {
				this.#timescales.set(ok.trackAlias, ok.timescale);
			}
		} catch (err) {
			this.#session.close();
			throw err;
		}
		state.registeredAlias = ok.trackAlias;
		return { stream: state.stream, alias: ok.trackAlias };
	}

	/**
	 * Handles an incoming PUBLISH_NAMESPACE on a bidi stream.
	 * Tracks announced broadcasts and notifies consumers.
	 *
	 * @internal
	 */
	async runPublishNamespace(msg: PublishNamespace, stream: Stream) {
		const version = this.#session.version;
		const path = msg.trackNamespace;

		// A path that already ran through us looped back. Refuse it rather than holding an
		// advertisement we could never subscribe through. The peer knows our Hop ID, so a
		// conforming one never offers it; 0 as the retry interval says not to come back.
		if (this.#reflected(msg.cluster)) {
			console.debug(`dropping reflected publish_namespace: broadcast=${path}`);
			await stream.writer.u53(RequestError.id);
			await new RequestError({
				requestId: msg.requestId,
				errorCode: 400,
				reasonPhrase: "route loops back",
			}).encode(stream.writer, version);
			stream.close();
			return;
		}

		// Draft-14/15 key their namespace-scoped messages by name, not request ID, so the
		// adapter can hold only one request per namespace: a second would overwrite the
		// first, and the withdrawals would then close the wrong stream and fail to find
		// the other. Nothing is lost by refusing it, because the case that makes a second
		// reference legitimate (an inline NAMESPACE for a path a PUBLISH_NAMESPACE already
		// carried) needs a message those drafts do not have.
		const legacy = version === Version.DRAFT_14 || version === Version.DRAFT_15;
		if (legacy && (this.#announced.has(path) || this.#legacyRequests.has(path))) {
			console.warn("duplicate PublishNamespace");
			if (version === Version.DRAFT_14) {
				await stream.writer.u53(PublishNamespaceError.id);
				await new PublishNamespaceError({
					requestId: msg.requestId,
					errorCode: 409,
					reasonPhrase: "duplicate namespace",
				}).encode(stream.writer, version);
			} else {
				await stream.writer.u53(RequestError.id);
				await new RequestError({
					requestId: msg.requestId,
					errorCode: 409,
					reasonPhrase: "duplicate namespace",
				}).encode(stream.writer, version);
			}
			stream.close();
			return;
		}

		// Everywhere else a path this session already knows is not refused: the same
		// namespace can reach us twice, and the count is what tells the second apart from
		// news. This request owns exactly one of those references and gives it back when
		// the stream ends.
		if (legacy) this.#legacyRequests.add(path);
		let attached = false;

		try {
			// Send OK first. This must complete before notifying consumers,
			// because consumers may trigger Subscribe writes that would
			// interleave with our OK on the control stream.
			if (version === Version.DRAFT_14) {
				await stream.writer.u53(PublishNamespaceOk.id);
				const ok = new PublishNamespaceOk({ requestId: msg.requestId });
				await ok.encode(stream.writer, version);
			} else {
				await stream.writer.u53(RequestOk.id);
				const ok = new RequestOk({
					requestId: version === Version.DRAFT_15 || version === Version.DRAFT_16 ? msg.requestId : undefined,
				});
				await ok.encode(stream.writer, version);
			}

			// Only now is the advertisement ours to announce, for the reason above: a
			// consumer reacting with a SUBSCRIBE must not interleave with that OK.
			attached = true;
			this.#attachAnnounce(path);

			// An advertisement is updated in place, by repeating PUBLISH_NAMESPACE on the
			// stream that already carries it, so read until the stream ends rather than
			// waiting on the close. Nothing else would deliver a re-parented route.
			const done = version === Version.DRAFT_16 || legacy;
			for (;;) {
				if (await stream.reader.done()) break;

				const typeId = await stream.reader.u53();
				if (done && typeId === PublishNamespaceDone.id) {
					await PublishNamespaceDone.decode(stream.reader, version);
					break;
				}
				if (typeId !== PublishNamespace.id) {
					throw new ProtocolViolation(
						`unexpected message on publish_namespace stream: 0x${typeId.toString(16)}`,
					);
				}

				const update = await PublishNamespace.decode(stream.reader, version, Cluster.negotiated(this.#cluster));

				// The stream is the advertisement, so an update on it must name the same
				// one. Applying a mismatched update would retarget this path with metadata
				// meant for a different request.
				if (update.requestId !== msg.requestId || update.trackNamespace !== path) {
					throw new ProtocolViolation("publish_namespace update does not match its stream");
				}

				// A path that now runs through us is unusable, so give it back. Keep
				// reading: this stream is the advertisement's only channel, so a later
				// clean path arrives here or nowhere.
				if (this.#reflected(update.cluster)) {
					if (attached) {
						attached = false;
						console.debug(`publish_namespace now loops back, detaching: broadcast=${path}`);
						this.#detachAnnounce(path);
					}
					continue;
				}

				// Re-attach: a clean path replaced the reflected one we detached from.
				if (!attached) {
					attached = true;
					this.#attachAnnounce(path);
				}
			}
		} finally {
			if (legacy) this.#legacyRequests.delete(path);

			// Give back exactly what was taken: a request that never got its OK out never
			// referenced the path, and retracting there would drop someone else's count.
			if (attached) {
				this.#detachAnnounce(path);
			}
		}
	}

	/**
	 * Handles an incoming PUBLISH on a bidi stream.
	 * We don't support reverse publish, so send error.
	 *
	 * @internal
	 */
	async runPublish(msg: Publish, stream: Stream) {
		const version = this.#session.version;

		if (version === Version.DRAFT_14) {
			await stream.writer.u53(PublishError.id);
			const err = new PublishError({
				requestId: msg.requestId,
				errorCode: 500,
				reasonPhrase: "publish not supported",
			});
			await err.encode(stream.writer, version);
		} else {
			await stream.writer.u53(RequestError.id);
			const err = new RequestError({
				requestId: version === Version.DRAFT_15 || version === Version.DRAFT_16 ? msg.requestId : undefined,
				errorCode: 500,
				reasonPhrase: "publish not supported",
			});
			await err.encode(stream.writer, version);
		}
		stream.close();
	}

	/**
	 * Handles an ObjectStream message (group + frames on uni stream).
	 *
	 * @internal
	 */
	async handleGroup(group: GroupMessage, stream: Reader) {
		const producer = new netGroup.Producer(group.groupId);

		if (group.subGroupId !== 0) {
			throw new Error("subgroups are not supported");
		}

		try {
			// The control message establishing this alias can arrive after the data stream.
			const track = await this.#aliases.get(group.trackAlias);

			track.writeGroup(producer);

			for (;;) {
				const done = await Promise.race([stream.done(), producer.closed, track.closed]);
				if (done !== false) break;

				const frame = await Frame.decode(
					stream,
					group.flags,
					this.#timescales.get(group.trackAlias),
					this.#session.version,
				);
				if (frame.payload === undefined) break;

				producer.writeFrame({ payload: frame.payload, timestamp: frame.timestamp ?? Timestamp.now() });
			}

			producer.close();
		} catch (err: unknown) {
			const e = error(err);
			producer.close(e);
			stream.stop(e);
		}
	}
}

/**
 * A broadcast consumed from a moq-transport session. Track info is resolved by the
 * subscribe path (the inherited `resolveTrackInfo`), but the protocol has no one-shot
 * group fetch, so `track.Consumer.fetchGroup()` is rejected.
 */
class ConsumeBroadcast extends broadcast.Consumer {
	// biome-ignore lint/complexity/noUselessConstructor: widens the protected base constructor to public
	constructor(state?: never) {
		super(state);
	}

	// Preserve the subclass when the consume cache shares this broadcast across callers.
	override clone(): ConsumeBroadcast {
		return new ConsumeBroadcast(this.shareState());
	}

	override fetchGroup(): Promise<netGroup.Consumer> {
		return Promise.reject(new Error("fetch group is not supported for moq-transport"));
	}
}
