import { race, Signal } from "@moq/signals";
import * as announce from "../announced.ts";
import * as broadcast from "../broadcast.ts";
import type { Probe as ProbeStats } from "../connection/stats.ts";
import { BroadcastCache } from "../consume.ts";
import { controlTimeout, error, ProtocolViolation, reason, StreamCode, StreamError, sessionCause } from "../error.ts";
import * as netGroup from "../group.ts";
import { Cost, type Hop, MAX_HOPS, type Route, routesEqual, UNKNOWN_HOP } from "../hop.ts";
import { groupBounds, hiddenBelow, scopeCaptures, scopeHead, scopeOverlaps } from "../internal.ts";
import * as Path from "../path.ts";
import { type Reader, Stream } from "../stream.ts";
import { TAIL_GRACE_MS, Tail } from "../tail.ts";
import * as Time from "../time.ts";
import type * as track from "../track.ts";
import { TimeoutError, withTimeout } from "../util/timeout.ts";
import { overrideBroadcastWire, wireOf } from "../wire.ts";
import {
	AnnounceHistory,
	AnnounceInit,
	AnnounceOk,
	AnnounceRequest,
	decodeAnnounceBroadcastMaybe,
} from "./announce.ts";
import { Datagram as DatagramMessage } from "./datagram.ts";
import * as DatagramStream from "./datagram_stream.ts";
import { Fetch as FetchMessage } from "./fetch.ts";
import type { Group as GroupMessage } from "./group.ts";
import { sendOrder } from "./priority.ts";
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
import {
	hasAnnounceId,
	hasAnnounceOk,
	hasDatagrams,
	hasProbeRtt,
	hasStreamCount,
	restartSupported,
	Version,
} from "./version.ts";

// Bound on how long stream-open plus the first response (SUBSCRIBE_OK on older
// drafts, or TRACK_INFO on lite-05+) may take. Browsers cap concurrent QUIC streams
// (Chrome ~100) and we open with waitUntilAvailable, so past the cap the open blocks
// until the peer frees a slot. The timeout turns a stall into a clear error.
const SUBSCRIBE_SETUP_TIMEOUT_MS = 10_000;

/** Decode an unsigned zigzag varint back to a signed delta (mirrors Rust `VarInt::to_zigzag`). */
function unzigzag(v: bigint): bigint {
	return (v >> 1n) ^ -(v & 1n);
}

// The TRACK stream and implicit SUBSCRIBE acceptance are lite-05+.
function supportsTrackStream(version: Version): boolean {
	switch (version) {
		case Version.DRAFT_01:
		case Version.DRAFT_02:
		case Version.DRAFT_03:
		case Version.DRAFT_04:
			return false;
		default:
			return true;
	}
}

interface SubscribeEntry {
	// The write side: incoming GROUP streams are routed here. The application reads
	// the matching track.Subscriber it got from broadcast.Consumer.subscribe.
	track: track.Producer;
	// Per-frame timestamp scale (0 = none). undefined until it's known (from TRACK_INFO
	// on lite-05+, or implicit defaults on older drafts). A non-zero value means each
	// frame on the group stream is prefixed with a zigzag-delta timestamp varint that
	// runGroup must consume to stay in sync; group streams block on it before decoding,
	// since a group's QUIC stream can race ahead of the subscribe stream.
	timescale: Signal<number | undefined>;
	// The group streams received, so the subscription can wait for the ones still owed
	// after the publisher ends it.
	tail: Tail;
	// The first group the publisher serves (SUBSCRIBE_START) and the track's exclusive end
	// (SUBSCRIBE_END), once it declares them.
	start?: number;
	end?: number;
	// Group streams opened by the publisher, when SUBSCRIBE_END carries the count.
	streams?: number;
}

/**
 * Handles subscribing to broadcasts and managing their lifecycle.
 *
 * @internal
 */
// What we close a session with on a protocol violation.
//
// The draft names the condition but assigns no numbers, so this is the Rust
// implementation's code for `Error::ProtocolViolation`: matching it is what makes the
// two report the same thing, where the default 0 would tell the peer it closed cleanly.
const PROTOCOL_VIOLATION_CODE = 15;

// WebTransport rejects a close reason over 1024 bytes of UTF-8 by throwing, so a reason
// built from peer-supplied data has to be bounded before it gets there. A broadcast path
// is peer-supplied and long enough to reach this on its own.
const MAX_CLOSE_REASON = 1024;

// The longest prefix of `text` that fits a close reason. `encodeInto` stops on a whole
// code point, so `read` never lands mid-character the way slicing bytes would.
function closeReason(text: string): string {
	const encoder = new TextEncoder();
	const buf = new Uint8Array(MAX_CLOSE_REASON);
	const { read } = encoder.encodeInto(text, buf);
	return text.slice(0, read);
}

export class Subscriber {
	#quic: WebTransport;

	// The version of the connection.
	readonly version: Version;

	// Shared with the Publisher so reflected announces can be dropped on receipt.
	readonly hop: Hop;

	// Our subscribed tracks. `timescale` resolves once known (from TRACK_INFO on
	// lite-05+, or implicit defaults on older drafts); group streams block on it
	// before decoding any frame, since a group's QUIC stream can race ahead.
	#subscribes = new Map<bigint, SubscribeEntry>();
	#subscribeNext = 0n;

	// Dedup consumed broadcasts per path: repeat consume() calls share one subscription.
	#consumes = new BroadcastCache();

	// Dedup in-flight one-shot fetches, keyed by [broadcast, track, sequence]. Concurrent (or
	// repeat, while still open) fetchGroup() calls for the same group share one FETCH stream and
	// each get an independent mirror; the entry is evicted once the group closes.
	#fetches = new Map<string, { group: netGroup.Producer; accepted: Promise<void> }>();

	// The peer's PROBE estimates, written as they arrive (Lite03+ only).
	#probe?: Signal<ProbeStats>;

	// The peer's SETUP (lite-05+), undefined until it arrives. Gates opening the PROBE
	// stream on the peer having advertised Probe >= Report.
	#peerSetup?: Signal<Setup | undefined>;

	// Distinguishes failures from streams torn down by Subscriber.close().
	#closed = new AbortController();
	/**
	 * Creates a new Subscriber instance.
	 * @param quic - The WebTransport session to use
	 * @param version - The protocol version
	 * @param origin - Hop id shared with the Publisher
	 * @param probe - Optional sink for the peer's PROBE estimates
	 * @param peerSetup - Optional peer SETUP slot for capability gating (lite-05+)
	 *
	 * @internal
	 */
	constructor(
		quic: WebTransport,
		version: Version,
		hop: Hop,
		probe?: Signal<ProbeStats>,
		peerSetup?: Signal<Setup | undefined>,
	) {
		this.#quic = quic;
		this.version = version;
		this.hop = hop;
		this.#probe = probe;
		this.#peerSetup = peerSetup;
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
	 */
	announced(scope: Path.Pattern = Path.Pattern.all(), options?: announce.Options): announce.Consumer {
		const announced = new announce.Producer();
		// The wire speaks announce interest by prefix, and echoes suffixes beneath it.
		void this.#runAnnounced(announced, scopeHead(scope), scope, options?.hidden ?? false);
		return announced.consume();
	}

	async #runAnnounced(
		announced: announce.Producer,
		prefix: Path.Valid,
		scope: Path.Pattern,
		hidden: boolean,
	): Promise<void> {
		console.debug(`announced: prefix=${prefix}`);
		// Lite04/05: send our own session-level Hop ID so the peer can skip announces
		// whose hop chain already passed through us. Encoding drops it on every other
		// version, where we drop the reflected announce on receipt instead. Matches the
		// Rust subscriber's `exclude_hop: self.self_origin.id` in `run_announce_prefix`.
		const msg = new AnnounceRequest(prefix, this.hop, hidden);
		const visible = (path: Path.Valid) => scopeOverlaps(scope, path) && (hidden || !hiddenBelow(prefix, path));

		// Opened outside the try so the catch can reach it: a protocol violation below has
		// to reset the stream, not just close our side of it.
		let stream: Stream;
		try {
			stream = await Stream.open(this.#quic);
		} catch (err: unknown) {
			announced.close(error(err));
			return;
		}

		try {
			// Send the announce interest.
			await stream.writer.u53(StreamId.Announce);
			await msg.encode(stream.writer, this.version);

			// Lite05+: the publisher reports its own Hop ID before any announces.
			// It no longer stamps itself onto each hop chain, so we append it here to
			// keep the reflected-announce loop check seeing the full chain.
			let responderOrigin: Hop | undefined;
			if (hasAnnounceOk(this.version)) {
				const ok = await AnnounceOk.decode(stream.reader, this.version);
				// Keep a withheld 0: it names nobody for loop detection, but it is the
				// anonymous mark and must travel the reconstructed chain. Assigned identities
				// stay off this hop and are never forwarded.
				responderOrigin = ok.hop;
			}

			// Every advertisement the peer currently has live, keyed by suffix (at most one
			// per path is current, and every announce on this stream shares `prefix`).
			//
			// An advertisement skipped locally as a reflected loop is recorded with
			// `publisher: undefined` and `live: false`: the peer numbered it and will retract
			// it regardless of what we made of it, so its path is not free. Dropping it from
			// the map instead would let a later announce take the path, and the skipped one's
			// `endedId` would then retract that one's state.
			//
			// `publisher` is what lets a restart tell a route change (same publisher,
			// subscriptions resume) from a replacement (a new generation took the path,
			// nothing carries over).
			type Advertisement = {
				publisher: Hop | undefined;
				live: boolean;
				route: Route;
				captures: Path.Pattern[] | undefined;
			};
			const advertised = new Map<Path.Valid, Advertisement>();

			switch (this.version) {
				case Version.DRAFT_01:
				case Version.DRAFT_02: {
					// Receive ANNOUNCE_INIT first
					const init = await AnnounceInit.decode(stream.reader, this.version);

					// Process initial announcements. These are advertisements like any other, so
					// they go on record and obey the same one-per-path rule: the initial set
					// naming a path twice is the same violation as two ANNOUNCE_STARTs for it,
					// and the record is what catches either. Draft01/02 carry no hop ids and no
					// ANNOUNCE_OK, so nothing names the publisher.
					for (const suffix of init.suffixes) {
						const path = Path.join(prefix, suffix);
						if (advertised.has(path)) {
							throw new ProtocolViolation(`duplicate announce for ${path}`);
						}
						const route = { hops: [UNKNOWN_HOP], cost: Cost.zero };
						const live = visible(path);
						const captures = scopeCaptures(scope, path);
						advertised.set(path, { publisher: undefined, live, route, captures });
						if (!live) continue;
						console.debug(`announced: broadcast=${path} active=true`);
						announced.append({ prefix: path, captures, kind: "announced", route });
					}
					break;
				}
				default:
					// Draft03+: no AnnounceInit, initial state comes via Announce messages.
					break;
			}

			// Lite06+: announce ids. Each received `active` implicitly assigns the next
			// per-stream ordinal; `endedId`/`restart` reference it, and lite-07 bases copy
			// from it. Tracked even for announces we skip as reflected, since the sender
			// doesn't know we skipped.
			const history = new AnnounceHistory();

			// Receive announce updates (for Draft03, this includes initial state)
			for (;;) {
				const announce = await race([
					decodeAnnounceBroadcastMaybe(stream.reader, this.version),
					announced.closed,
				]);
				// undefined: the stream ended. null: the consumer closed cleanly.
				if (!announce) break;
				if (announce instanceof Error) throw announce;

				let path: Path.Valid;
				let active: boolean;
				// Present on active/restart; ended messages never carry hops worth checking.
				let hops: Hop[] | undefined;
				let cost: Cost | undefined;

				switch (announce.status) {
					case "active": {
						const resolved = hasAnnounceId(this.version) ? history.start(announce) : announce;
						// The wire names the suffix beneath the interest prefix; the consumer
						// sees the covered path from the session root.
						path = Path.join(prefix, resolved.suffix);
						active = true;
						hops = resolved.hops;
						cost = announce.cost;
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
					case "restart": {
						// Resolve the id; it stays live (the replacement reuses it).
						const resolved = history.update(announce);
						path = Path.join(prefix, resolved.suffix);
						active = true;
						hops = resolved.hops;
						cost = announce.cost;
						break;
					}
					case "skipped":
						continue;
				}

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
				const duplicateIsRestart = restartSupported(this.version) && !hasAnnounceId(this.version);
				if (announce.status === "active" && !duplicateIsRestart && advertised.has(path)) {
					throw new ProtocolViolation(`duplicate announce for ${path}`);
				}

				// Retract the path: forget the advertisement, drop the shared consume entry so a
				// later announce subscribes fresh rather than cloning the dead generation's tracks,
				// and tell the consumer. A no-op for an advertisement never surfaced, which is
				// what an id retiring a skipped announce resolves to.
				const retract = () => {
					const previous = advertised.get(path);
					advertised.delete(path);
					if (!previous?.live) return;
					this.#consumes.evict(path);
					console.debug(`announced: broadcast=${path} active=false`);
					announced.append({
						prefix: path,
						captures: previous.captures,
						kind: "retracted",
						route: previous.route,
					});
				};

				// In Lite05+ the sender's origin arrives via AnnounceOk, not in each hop
				// list, so fold it back in before checking.
				if (hops !== undefined) {
					const full = responderOrigin !== undefined ? [...hops, responderOrigin] : hops;
					if (full.includes(this.hop)) {
						// A reflected restart means the peer's remaining route loops back through
						// us, so the route is gone even though the message says active. The
						// advertisement stays live: the peer still holds the path and its id still
						// resolves here.
						retract();
						advertised.set(path, {
							publisher: undefined,
							live: false,
							route: { hops: full, cost: Cost.zero },
							captures: undefined,
						});
						continue;
					}
				}

				if (!active) {
					retract();
					continue;
				}

				// The first hop identifies the original publisher; an empty chain means the
				// peer itself originated it. See `restart_announce` in the Rust subscriber.
				const publisher = hops?.[0] ?? responderOrigin;

				// A publisher with no identity (an empty chain from a peer that withheld its
				// own id, or a lite-03 UNKNOWN placeholder) never proves continuity: two such
				// advertisements can be unrelated publishers. Mirrors the
				// `publisher == Hop::UNKNOWN` arm of the Rust `restart_announce`.
				const identified = publisher !== undefined && publisher !== UNKNOWN_HOP;
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
						publisher: undefined,
						live: false,
						route: { hops: [], cost: Cost.zero },
						captures: undefined,
					});
					continue;
				}
				const route: Route = { hops: fullHops, cost: cost ?? Cost.zero };
				const captures = scopeCaptures(scope, path);
				if (!visible(path)) {
					advertised.set(path, { publisher, live: false, route, captures });
					continue;
				}

				// A second advertisement for a path we already carry is a restart: either an
				// explicit ANNOUNCE_UPDATE, or (lite-05) a duplicate ANNOUNCE.
				const previous = advertised.get(path);
				if (previous?.live) {
					if (identified && previous.publisher === publisher) {
						// Same publisher, new route. In-flight subscriptions resume across it.
						// Emit the route so a forwarder can re-price without retracting.
						if (!routesEqual(previous.route, route)) {
							advertised.set(path, { publisher, live: true, route, captures });
							console.debug(`announced: broadcast=${path} rerouted`);
							announced.append({ prefix: path, captures, kind: "updated", route });
						} else {
							console.debug(`announced: broadcast=${path} rerouted`);
						}
						continue;
					}

					// A different publisher took the path, so cached track info and existing
					// subscriptions must not carry over. Surface a real end before the start.
					retract();
				}

				// After `retract()`, which clears the entry: the path is advertised again, by
				// whoever just took it over. Recording it before would leave nothing behind, so
				// the *next* takeover would read as a first announcement and skip its own end.
				advertised.set(path, { publisher, live: true, route, captures });

				console.debug(`announced: broadcast=${path} active=true`);
				announced.append({ prefix: path, captures, kind: "announced", route });
			}

			announced.close();
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
			if (e instanceof ProtocolViolation) {
				this.#quic.close({ closeCode: PROTOCOL_VIOLATION_CODE, reason: closeReason(reason(e)) });
			}
		}
	}

	/**
	 * Consumes a broadcast from the connection.
	 *
	 * Deduplicated per path: repeat calls for the same still-live path share one reference-counted
	 * broadcast (and one upstream subscription). The shared broadcast closes once every caller has
	 * closed its handle, so callers close normally.
	 *
	 * @param name - The name of the broadcast to consume
	 * @returns A Broadcast instance
	 */
	consume(path: Path.Valid): broadcast.Consumer {
		return this.#consumes.get(path) ?? this.#consumes.insert(path, this.#createConsume(path));
	}

	#createConsume(path: Path.Valid): broadcast.Consumer {
		// A consumed broadcast resolves info() and fetchGroup() over the wire by reaching
		// back into this Subscriber (see ConsumeBroadcast below), rather than the wire
		// installing callbacks on the broadcast.
		const consumer = new ConsumeBroadcast(this, path);

		void (async () => {
			for (;;) {
				const request = await wireOf(consumer).requested();
				if (!request) break;
				void this.#runSubscribe(path, request);
			}
		})();

		return consumer;
	}

	async #runSubscribe(broadcast: Path.Valid, request: track.Request) {
		const id = this.#subscribeNext++;
		const subscription = request.subscription;
		const initialBounds = groupBounds(subscription.groups);
		if (emptyRange({ startGroup: initialBounds.start, endGroup: initialBounds.end })) {
			request.reject(new Error(EMPTY_RANGE));
			return;
		}

		// `timescale` stays undefined until TRACK_INFO (or, on older drafts,
		// implicit defaults) resolves it; runGroup blocks on it before decoding.
		const timescale = new Signal<number | undefined>(undefined);

		console.debug(`subscribe start: id=${id} broadcast=${broadcast} track=${request.name}`);
		const bounds = groupBounds(subscription.groups);

		const msg = new Subscribe({
			id,
			broadcast,
			track: request.name,
			priority: subscription.priority ?? 0,
			maxAge: subscription.maxAge,
			startGroup: subscription.groups?.start === undefined ? undefined : bounds.start,
			endGroup: inclusiveGroupEnd(bounds.end),
		});

		// Open the stream under a timeout. The stream handle flows back via `state`
		// so the timeout path can abort it if it finishes opening after the deadline.
		const state: { stream?: Stream } = {};
		const setup = this.#openSubscribe(state, msg, request, id, timescale);

		let opened: { stream: Stream; entry: SubscribeEntry };
		try {
			opened = await withTimeout(
				setup,
				SUBSCRIBE_SETUP_TIMEOUT_MS,
				`subscribe timed out after ${SUBSCRIBE_SETUP_TIMEOUT_MS}ms waiting for the first response (browser stream limit reached?)`,
			);
			console.debug(`subscribe ok: id=${id} broadcast=${broadcast} track=${request.name}`);
		} catch (err) {
			// The setup outlived its deadline waiting for the first response: a control
			// timeout, not content that arrived late.
			const e = err instanceof TimeoutError ? controlTimeout(err) : await sessionCause(this.#quic, err);
			request.reject(e);
			this.#subscribes.delete(id);
			console.warn(`subscribe error: id=${id} broadcast=${broadcast} track=${request.name} error=${reason(e)}`);
			// If the stream eventually opens after the timeout, abort it so we
			// don't leak it. Cover both branches: setup may resolve late, or it
			// may reject (e.g. encode/decode failure) after the stream is open.
			setup.then(
				() => state.stream?.abort(e),
				() => state.stream?.abort(e),
			);
			return;
		}

		const { stream, entry } = opened;
		const producer = entry.track;
		try {
			// Watch for subscription changes and send SUBSCRIBE_UPDATE. Lite01/Lite02
			// don't carry SUBSCRIBE_UPDATE on the wire, so skip the watcher there
			// and just wait on the stream/track like before.
			//
			// On lite-05+ the publisher sends SUBSCRIBE_START/END/DROP on this stream until
			// its FIN; older drafts just close it. Either way group streams can still be in
			// flight, so the track ends only once the tail is accounted for. A reset rejects
			// instead, so the track ends with that error rather than a clean tail.
			const responses = supportsTrackStream(this.version)
				? this.#runResponses(stream, entry)
				: stream.reader.closed;
			const closed = responses.then(() => this.#settleTail(entry));
			// A reset that lands after the race below settled is moot; the race observes one before.
			closed.catch(() => {});
			const subscriptionUpdates =
				this.version === Version.DRAFT_01 || this.version === Version.DRAFT_02
					? undefined
					: this.#runSubscriptionUpdates(id, broadcast, producer, msg, stream);

			// Terminal conditions (stream end, track close, a failed subscription update) settle at most
			// once; race them into one stable promise so the demand loop doesn't re-subscribe each pass.
			// Updates stop quietly at the FIN, which can land before the responses ahead of it are
			// decoded, so only their failure is terminal on its own.
			const terminal: PromiseLike<unknown>[] = [closed, producer.closed];
			if (subscriptionUpdates !== undefined) terminal.push(subscriptionUpdates.then(() => closed));
			const done = race(terminal);

			// Serve until a terminal condition fires or the last local subscriber leaves. The unused
			// wake is level-triggered: re-check demand so a subscriber that returns before we tear
			// down (e.g. a quickly unmuted tile) resumes on the same subscription.
			const idle = Symbol("idle");
			for (;;) {
				const reason = await race([done, producer.unused().then(() => idle)]);
				if (reason === idle && producer.closed.peek() === undefined && producer.used.peek()) continue;
				break;
			}

			producer.close();
			stream.close();
			console.debug(`subscribe close: id=${id} broadcast=${broadcast} track=${request.name}`);
		} catch (err) {
			const e = await sessionCause(this.#quic, err);
			producer.close(e);
			console.warn(`subscribe error: id=${id} broadcast=${broadcast} track=${request.name} error=${reason(e)}`);
			stream.abort(e);
		} finally {
			this.#subscribes.delete(id);
		}
	}

	// Determine the track's immutable properties, accept the request (so the
	// application's track.Subscriber resolves and incoming groups have a producer to
	// write into), register it, then open the subscribe stream. `state.stream` is
	// populated as soon as the subscribe stream opens so the caller can clean it up
	// on timeout even before this promise settles.
	//
	// On lite-05+ the properties come from a TRACK stream opened first, and the
	// SUBSCRIBE is accepted implicitly (no SUBSCRIBE_OK). Older drafts carry no
	// per-track properties, so they resolve to defaults and just drain SUBSCRIBE_OK.
	async #openSubscribe(
		state: { stream?: Stream },
		msg: Subscribe,
		request: track.Request,
		id: bigint,
		timescale: Signal<number | undefined>,
	): Promise<{ stream: Stream; entry: SubscribeEntry }> {
		let producer: track.Producer;
		let drainOk = false;

		if (supportsTrackStream(this.version)) {
			// Fetch the immutable properties once via the TRACK stream.
			const info = await this.#trackInfo(msg.broadcast, msg.track);
			producer = request.accept(this.#toModelInfo(info));
			timescale.set(info.timescale);
		} else {
			// Older drafts negotiate nothing per-track: verbatim frames, no timescale.
			producer = request.accept();
			timescale.set(0);
			drainOk = true;
		}

		// Register before opening SUBSCRIBE so a racing GROUP stream finds the entry.
		const entry: SubscribeEntry = { track: producer, timescale, tail: new Tail() };
		this.#subscribes.set(id, entry);

		state.stream = await Stream.open(this.#quic);
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

	// Opens a TRACK stream, reads the single TRACK_INFO, and FINs. Lite-05+ only.
	async #trackInfo(broadcast: Path.Valid, track: string): Promise<TrackInfo> {
		const stream = await Stream.open(this.#quic);
		try {
			await stream.writer.u53(StreamId.Track);
			await new TrackMessage(broadcast, track).encode(stream.writer, this.version);
			const info = await TrackInfo.decode(stream.reader, this.version);
			// The publisher FINs after TRACK_INFO; FIN our side too.
			stream.close();
			return info;
		} catch (err) {
			stream.abort(error(err));
			throw err;
		}
	}

	// Map the wire TRACK_INFO onto the model track.Info a producer/consumer holds.
	#toModelInfo(info: TrackInfo): track.Info {
		return {
			timescale: Time.Timescale(info.timescale),
			// Publisher Max Age rides on the wire, so the local retention window
			// matches what the upstream advertises (relays re-serve with the same bound).
			maxAge: Time.Milli(info.maxAge),
			priority: info.priority,
		};
	}

	// Resolve a track's immutable model info via a TRACK stream (lite-05+), for the
	// ConsumeBroadcast backing track.Consumer.query(). On older drafts there's no TRACK
	// stream, so this rejects rather than fabricating defaults.
	async resolveTrackInfo(broadcast: Path.Valid, track: string): Promise<track.Info> {
		if (!supportsTrackStream(this.version)) {
			throw new Error("track info requires moq-lite-05 or newer");
		}
		return this.#toModelInfo(await this.#trackInfo(broadcast, track));
	}

	// Open a FETCH stream for one group and stream its bare frames into a group, for the
	// ConsumeBroadcast backing track.Consumer.fetchGroup() (lite-05+).
	async fetchGroup(
		broadcast: Path.Valid,
		track: string,
		sequence: number,
		options: track.FetchGroupOptions = {},
	): Promise<netGroup.Consumer> {
		// Coalesce onto a still-open fetch of the same group so we don't open a second FETCH
		// stream (and re-download it); each caller reads an independent mirror.
		const key = JSON.stringify([broadcast, track, sequence]);
		let entry = this.#fetches.get(key);
		if (!entry || entry.group.isClosed) {
			const group = new netGroup.Producer(sequence);
			entry = { group, accepted: this.#runFetch(broadcast, track, sequence, options, group) };
			this.#fetches.set(key, entry);
			void group.closed.then(() => {
				if (this.#fetches.get(key)?.group === group) this.#fetches.delete(key);
			});
		}

		// Reserve each caller's mirror before awaiting acceptance so the pump sees demand,
		// and a fast FIN cannot discard frames before these callers receive their handles.
		const consumer = entry.group.mirror();
		try {
			await entry.accepted;
			return consumer;
		} catch (err) {
			consumer.close();
			throw err;
		}
	}

	// Open the FETCH stream and pump the response into the shared group. Setup errors close the
	// group, evict the entry, and reject every caller waiting for acceptance.
	async #runFetch(
		broadcast: Path.Valid,
		track: string,
		sequence: number,
		options: track.FetchGroupOptions,
		group: netGroup.Producer,
	): Promise<void> {
		try {
			if (!supportsTrackStream(this.version)) {
				throw new Error("fetch group requires moq-lite-05 or newer");
			}

			const info = await this.#trackInfo(broadcast, track);
			const priority = options.priority ?? 0;
			const stream = await Stream.open(this.#quic, { sendOrder: sendOrder({ priority }) });

			try {
				await stream.writer.u53(StreamId.Fetch);
				await new FetchMessage({ broadcast, track, priority, group: sequence }).encode(
					stream.writer,
					this.version,
				);
				// A byte or an empty-group FIN accepts the fetch; a reset rejects it.
				// done() buffers that byte so the response pump can decode it normally.
				await stream.reader.done();
			} catch (err: unknown) {
				stream.abort(error(err));
				throw err;
			}

			void this.#runFetchResponse(stream, group, Time.Timescale(info.timescale));
		} catch (err: unknown) {
			group.close(error(err));
			throw err;
		}
	}

	// Read the FETCH response (bare zigzag-delta-timestamped frames) into the group, then
	// FIN. A stream-level failure aborts the group so its reader observes the gap.
	async #runFetchResponse(stream: Stream, group: netGroup.Producer, timescale: Time.Timescale): Promise<void> {
		try {
			let prevTs = 0n;

			// Serve until the stream FINs, the group closes, or every reader leaves. A group can
			// stay open indefinitely (a catalog or JSON stream), so an abandoned fetch is stopped by
			// demand, not by the stream ending. `unused` is watched across frames as one stable
			// promise; the check is level-triggered, so a coalesced fetch that arrives before we
			// cancel re-arms and resumes.
			const idle = Symbol("idle");
			let unused = group.unused().then(() => idle);
			for (;;) {
				const done = await race([stream.reader.done(), group.closed, unused]);
				if (done === idle) {
					if (!group.isClosed && group.used.peek()) {
						unused = group.unused().then(() => idle);
						continue;
					}
					break;
				}
				if (done !== false) break;

				prevTs += unzigzag(await stream.reader.u62());
				const timestamp = new Time.Timestamp(Number(prevTs), timescale);
				const size = await stream.reader.u53();
				const payload = await stream.reader.read(size);
				if (!payload) break;
				group.writeFrame({ payload, timestamp });
			}

			group.close();
			stream.close();
		} catch (err: unknown) {
			const e = error(err);
			group.close(e);
			stream.abort(e);
		}
	}

	// Reads SUBSCRIBE_START/END/DROP on the subscribe stream until FIN (lite-05+), recording
	// the range the tail is accounted against. SUBSCRIBE_END declares the track's end right
	// away, so a consumer learns it before the last groups arrive. Resolves on FIN and rejects
	// when the stream is reset, so the track ends with the publisher's error rather than cleanly.
	async #runResponses(stream: Stream, entry: SubscribeEntry): Promise<void> {
		for (;;) {
			const resp = await decodeSubscribeResponseMaybe(stream.reader, this.version);
			if (!resp) return;

			if ("start" in resp) {
				entry.start = resp.start.group;
			} else if ("end" in resp) {
				if (entry.end !== undefined) throw new ProtocolViolation("duplicate SUBSCRIBE_END");
				entry.end = resp.end.group;
				if (hasStreamCount(this.version)) entry.streams = resp.end.streams;
				// A local close can win the race with the response; there is nothing left to end.
				if (entry.track.closed.peek() !== undefined) continue;
				try {
					entry.track.finishAt(entry.end);
				} catch (err) {
					throw new ProtocolViolation(`invalid SUBSCRIBE_END: ${reason(error(err))}`);
				}
			} else if ("drop" in resp) {
				entry.tail.account(resp.drop.start, resp.drop.end + 1);
			}
		}
	}

	// Wait for the group streams the publisher still owes once it has ended the subscription.
	//
	// lite-07 counts streams, so skipped sequences owe nothing. Older drafts account for
	// the range using received headers and SUBSCRIBE_DROP. A counted stream reset before
	// its header leaves no trace, so the grace still bounds that wait. Streams whose
	// headers arrived keep reading until their own FIN or reset.
	#settleTail(entry: SubscribeEntry): Promise<void> {
		const { tail, track } = entry;
		// Already the smaller of the subscriber's and the track's max age.
		const maxAge = track.subscription.peek()?.maxAge ?? Time.Milli.zero;
		const grace = maxAge > 0 ? maxAge : TAIL_GRACE_MS;

		const complete = () => {
			if (entry.streams !== undefined) return tail.streams >= entry.streams;
			// Without SUBSCRIBE_END (older drafts) nothing says which groups are owed.
			if (entry.end === undefined) return false;
			// Without SUBSCRIBE_START the publisher served no group at all.
			if (entry.start === undefined) return true;
			const bounds = groupBounds(track.subscription.peek()?.groups ?? {});
			const start = Math.max(entry.start, bounds.start);
			const end = bounds.end === undefined ? entry.end : Math.min(entry.end, bounds.end);
			return tail.covers(start, end);
		};

		return tail.settle(complete, grace, track.closed);
	}

	/**
	 * Send SUBSCRIBE_UPDATE messages whenever the track's aggregate subscription changes.
	 *
	 * Resolves cleanly when the stream or track closes, so the caller can include
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
		track: track.Producer,
		msg: Subscribe,
		stream: Stream,
	): Promise<void> {
		const stopped: Promise<null> = race([track.closed, stream.reader.closed]).then(() => null);
		let lastSent: track.Subscription = {
			priority: msg.priority,
			maxAge: Time.Milli(msg.maxAge),
			groups: {
				start: msg.startGroup === undefined ? undefined : { included: msg.startGroup },
				end: msg.endGroup === undefined ? undefined : { excluded: exclusiveGroupEnd(msg.endGroup) ?? 0 },
			},
		};

		for (;;) {
			const current = track.subscription.peek();
			if (current === undefined || this.#sameSubscription(current, lastSent)) {
				// Nothing new to send; wait for a change or termination.
				const next = await race([track.subscription.changed(), stopped]);
				if (next === null) return;
				continue;
			}

			// Demand collapsing to nothing is refused the same way an initial empty
			// request is: the error closes the track, so every local subscriber sees it.
			const bounds = groupBounds(current.groups);
			if (emptyRange({ startGroup: bounds.start, endGroup: bounds.end })) throw new Error(EMPTY_RANGE);

			// Round-trip the other Subscribe parameters so the publisher doesn't
			// interpret SUBSCRIBE_UPDATE as a reset of ordered/maxAge/etc.
			const update = new SubscribeUpdate({
				priority: current.priority ?? 0,
				maxAge: current.maxAge,
				startGroup: current.groups?.start === undefined ? undefined : bounds.start,
				endGroup: inclusiveGroupEnd(bounds.end),
			});
			await update.encode(stream.writer, this.version);
			lastSent = { ...current };
			console.debug(`subscribe update: id=${id} broadcast=${broadcast} track=${track.name}`);
		}
	}

	#sameSubscription(a: track.Subscription, b: track.Subscription): boolean {
		const ag = groupBounds(a.groups);
		const bg = groupBounds(b.groups);
		return (
			(a.priority ?? 0) === (b.priority ?? 0) &&
			(a.maxAge ?? 0) === (b.maxAge ?? 0) &&
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
		const producer = new netGroup.Producer(group.sequence);
		const read = tail.open(group.sequence);

		try {
			track.writeGroup(producer);

			// Block until the timescale is known; the group's stream can arrive before
			// TRACK_INFO (or implicit defaults) resolves it on the subscribe stream.
			let scale = timescale.peek();
			while (scale === undefined) {
				if (track.closed.peek() !== undefined) {
					// Subscription ended before the scale resolved; nothing to decode.
					producer.close();
					stream.stop(new StreamError(StreamCode.Cancel, { message: "cancel" }));
					return;
				}
				await Signal.race(timescale, track.closed);
				scale = timescale.peek();
			}

			// A non-zero scale means every frame is prefixed with a zigzag-delta timestamp
			// (the lite-05 FRAME format), which we decode into a Timestamp at that scale.
			// Scale 0 (pre-lite-05) carries no timestamp, so we wall-clock-stamp.
			let prevTs = 0n;

			for (;;) {
				// Only the group's own stream ends it: a track that closes first has already
				// closed (or aborted) this group through its cache.
				const done = await race([stream.done(), producer.closed]);
				if (done !== false) break;

				let timestamp: Time.Timestamp;
				if (scale !== 0) {
					prevTs += unzigzag(await stream.u62());
					timestamp = new Time.Timestamp(Number(prevTs), Time.Timescale(scale));
				} else {
					timestamp = Time.Timestamp.now();
				}

				const size = await stream.u53();
				const payload = await stream.read(size);
				if (!payload) break;

				producer.writeFrame({ payload, timestamp });
			}

			producer.close();
			stream.stop(new StreamError(StreamCode.Cancel, { message: "cancel" }));
		} catch (err: unknown) {
			const e = error(err);
			producer.close(e);
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
	// datagram (best-effort) if the subscription is unknown/closed or its timescale isn't resolved.
	async #routeDatagram(payload: Uint8Array): Promise<void> {
		const dg = await DatagramMessage.decode(payload);

		const entry = this.#subscribes.get(dg.subscribe);
		if (!entry) return; // Unknown or already-closed subscription.

		// Datagrams are lite-05+, which always negotiates a timescale; if it hasn't resolved
		// yet (the datagram raced ahead of TRACK_INFO), drop rather than guess.
		const scale = entry.timescale.peek();
		if (!scale) return;

		const timestamp = new Time.Timestamp(dg.timestamp, Time.Timescale(scale));
		// A datagram's sequence is never owed a stream, so it never holds the tail open.
		entry.tail.account(dg.sequence, dg.sequence + 1);
		entry.track.insertDatagram(dg.sequence, timestamp, dg.payload);
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
	async #peerProbeLevel(peerSetup: Signal<Setup | undefined>): Promise<ProbeLevel> {
		let setup = peerSetup.peek();
		while (setup === undefined) {
			setup = await peerSetup.changed();
		}
		return setup.probe;
	}

	async runProbe(): Promise<void> {
		if (!this.#probe) return;
		if (this.version === Version.DRAFT_01 || this.version === Version.DRAFT_02) return;

		// Lite-05+ gates the PROBE stream on the peer advertising Probe >= Report in its
		// SETUP. Wait for the SETUP, then bail if the peer can't report bitrate. Older
		// drafts have no SETUP, so they keep probing unconditionally.
		if (this.#peerSetup) {
			const probe = await this.#peerProbeLevel(this.#peerSetup);
			if (probe < ProbeLevel.Report) return;
		}

		// Probe is best-effort: any failure (stream reset by peer, missing peer support,
		// transport hiccup) MUST NOT tear down the connection. On error, drop the
		// estimates so consumers know they're stale.
		try {
			const stream = await Stream.open(this.#quic);
			await stream.writer.u53(StreamId.Probe);

			for (;;) {
				const probe = await Probe.decodeMaybe(stream.reader, this.version);
				if (!probe) break;
				// lite-03 carries no RTT field, so an absent value there means "not
				// carried" and the last reading stands. From lite-04 the field is
				// always present and 0 explicitly means unknown, so undefined is the
				// peer retracting a value we would otherwise hold forever.
				const prev = this.#probe.peek();
				const rtt = probe.rtt !== undefined ? Time.Milli(probe.rtt) : undefined;
				this.#probe.set({
					// `undefined` is the peer reporting "unknown", not an estimate of
					// zero; letting it through would become a real 0 bps ABR target.
					estimatedRecvRate: probe.bitrate,
					rtt: hasProbeRtt(this.version) ? rtt : (rtt ?? prev.rtt),
				});
			}
		} catch (err: unknown) {
			if (!this.#closed.signal.aborted) {
				console.warn("probe stream error", err);
			}
		} finally {
			this.#probe.set({});
		}
	}

	/**
	 * Ends every subscribed track: cleanly for a deliberate close, or with `err` when the
	 * session died, since those tracks were cut off rather than ended.
	 */
	close(err?: Error) {
		this.#closed.abort();

		for (const { track } of this.#subscribes.values()) {
			track.close(err);
		}

		this.#subscribes.clear();
	}
}

/**
 * A broadcast consumed from a lite session. It resolves `track.Consumer.query()` and
 * `.fetchGroup()` over the wire (lite-05+ TRACK / FETCH streams) by reaching into the
 * {@link Subscriber} it was opened from, the way the Rust `BroadcastConsumer` holds its
 * session. Live subscribes still flow through the inherited requested() queue.
 */
class ConsumeBroadcast extends broadcast.Consumer {
	#subscriber: Subscriber;
	#path: Path.Valid;

	constructor(subscriber: Subscriber, path: Path.Valid, state?: never) {
		super(state);
		overrideBroadcastWire(this, {
			resolveTrackInfo: (name) => subscriber.resolveTrackInfo(path, name),
			fetchGroup: (name, sequence, options) => subscriber.fetchGroup(path, name, sequence, options),
		});
		this.#subscriber = subscriber;
		this.#path = path;
	}

	// Preserve the subclass (and its wire-backed info/fetchGroup) when the consume cache shares
	// this broadcast across callers.
	override clone(): ConsumeBroadcast {
		return new ConsumeBroadcast(this.#subscriber, this.#path, this.shareState());
	}
}
