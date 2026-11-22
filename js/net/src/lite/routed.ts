// Lite-07: what the peer sends on its Announce Stream as a publisher, and the prefixes this
// session asks it for on its own.
//
// Every `routes()` reader of the session shares the one stream. The session asks the peer only
// for the covering set of what its readers want, no prefix at or below another, edited in place
// one ANNOUNCE_REQUEST at a time, and each reader is fed from the claims the stream carries,
// presented under the reader's own prefix as the peer would present them to a request for it.
//
// This node is an endpoint: it declares no Node ID, so the peer sends it claims and never routes.
// Each claim is one instance's at a path, with what the peer's own reader shows of it: its cost,
// how far above the path it was made, and whether a route still reaches it.

import { type Dispose, type GetPromise, type Getter, race } from "@moq/signals";
import type * as announce from "../announced.ts";
import type { Grant } from "../auth.ts";
import type { Goaway } from "../connection/goaway.ts";
import type { BroadcastCache } from "../consume.ts";
import { error, ProtocolViolation } from "../error.ts";
import { routesEqual, WireRoute } from "../hop.ts";
import type * as Instance from "../instance.ts";
import { coveringPrefixes, hiddenBelow, mostSpecific, scopeCaptures, scopeOverlaps } from "../internal.ts";
import * as Path from "../path.ts";
import { SlotMap, type Slots } from "../util/limits.ts";
import {
	type Add,
	type AnnounceBroadcast,
	AnnounceHistory,
	type Claim,
	requestFits,
	requestParts,
} from "./announce.ts";
import type { Version } from "./version.ts";

/** What a `routes()` reader asked for. */
export interface Request {
	/** The prefix the reader's scope lies under. */
	prefix: Path.Valid;
	scope: Path.Pattern;
	/** Also report claims with a `.`-prefixed segment below the prefix. */
	hidden: boolean;
	/** Also report claims whose subtree overlaps the scope without matching it. */
	covering: boolean;
	/** Report each instance's claim at a path rather than the best one. */
	claims: boolean;
}

/** What the session's route layer is built on. */
export interface Session {
	quic: WebTransport;
	version: Version;
	consumes: BroadcastCache;
	/** The session's announce cap: one slot per live claim. */
	slots: Slots;
	/** Rank a route last once the peer sent GOAWAY. */
	drained(route: WireRoute): WireRoute;
	goaway?: GetPromise<Goaway>;
	/** Our grant: the peer answers a prefix wider than what it lets us subscribe to region by region. */
	grant?: Getter<Grant | undefined>;
	/** Send one message on this session's Announce Stream. */
	send(msg: AnnounceBroadcast): Promise<void>;
}

/** One reader, and what it was last told of each path. */
interface Reader extends Request {
	announced: announce.Producer;
	/** What each path reports, by instance with `claims` and otherwise under `undefined`. */
	reported: Map<Path.Valid, Map<Instance.Valid | undefined, WireRoute>>;
	synced: boolean;
}

/** A prefix this session asked the peer for, by its Prefix ID. */
interface Asked {
	path: Path.Valid;
	hidden: boolean;
	/** The peer answered the ANNOUNCE_REQUEST adding it, so its stream may name it. */
	live: boolean;
	/** The peer sent its initial set. */
	synced: boolean;
}

/** One live claim the peer sent, by its Announce ID. */
interface Live {
	/** Relative to the session, or undefined when it resolves to no path this node can hold,
	 * which the claim then never has. */
	path?: Path.Valid;
	instance: Instance.Valid;
	claim: Claim;
	/** Whether the claim's run, its epoch and how far above its path it is, was ever told
	 * available. A run first told unavailable is not there: nothing can serve it. */
	taken: boolean;
	/** Who the claim is filed under in the consume cache. */
	owner: object;
}

// The most a claim at `path` can be above it: a claim above the session's root covers the whole
// of what the peer shows. Mirrors rs/moq-net.
const checkAbove = (path: Path.Valid, above: bigint) => {
	if (above > BigInt(Path.parts(path).length))
		throw new ProtocolViolation(`a claim at ${path} cannot be ${above} segments above it`);
};

/** The claims a lite-07 session's peer sends, shared by every reader of the session. */
export class Routed {
	#session: Session;
	#readers = new Set<Reader>();
	/** The prefixes asked of the peer. */
	#asked = new Map<bigint, Asked>();
	#nextPrefix = 0n;
	/** The ANNOUNCE_REQUEST the peer has not answered yet. */
	#editing?: { remove: bigint[]; add: bigint[] };
	/** The peer finished its stream: it answers nothing more. */
	#finished = false;
	/** This side finished its stream: it asks for nothing more. */
	#silent = false;
	/** The grant showed a reader more since the last edit: the next goes even when it changes
	 * nothing, since its ANNOUNCE_OK follows what the change shows under a wider prefix. */
	#regranted = false;
	/** What our grant lets us subscribe to, as last seen. */
	#granted?: Path.Prefixes;

	// Each live claim holds one of the session's announce slots.
	#claims: SlotMap<bigint, Live>;
	// The claims shown at each path, oldest first.
	#byPath = new Map<Path.Valid, Set<bigint>>();
	#history = new AnnounceHistory();

	// Set once the session lets go of its claims, which then show nothing anew.
	#closing = false;
	#stopDrain?: Dispose;
	#stopGrant?: Dispose;

	constructor(session: Session) {
		this.#session = session;
		this.#claims = new SlotMap(session.slots);
		// A draining peer ranks its claims last from the GOAWAY itself.
		this.#stopDrain = session.goaway?.changed(() => {
			if (this.#closing) return;
			for (const [id, live] of this.#claims) this.#serve(id, live, false, false);
		});
		this.#granted = session.grant?.peek()?.subscribe;
		this.#stopGrant = session.grant?.subscribe(() => {
			this.#ask();
			this.#release();
		});
	}

	// Note a grant change, read as it is set rather than when its listeners run, so a reader
	// attached between the two waits too. What it newly shows within a prefix asked may already be
	// in the peer's answer, unheard here: the next edit goes, since its answer follows the change,
	// and the readers there and any reader arriving before that answer wait for it. Elsewhere, and
	// on a narrowing, nothing new can arrive, and a reader stays synced. Mirrors rs/moq-net.
	#regrant(): void {
		const next = this.#session.grant?.peek()?.subscribe;
		const granted = this.#granted;
		if (next === granted || (next && granted?.equals(next))) return;
		this.#granted = next;
		const regions = gained(granted, next);
		const overlaps = (prefix: Path.Valid) =>
			regions.some((region) => Path.hasPrefix(region, prefix) || Path.hasPrefix(prefix, region));
		if ([...this.#asked.values()].some((asked) => overlaps(asked.path))) this.#regranted = true;
		for (const reader of this.#readers) {
			if (!overlaps(reader.prefix)) continue;
			this.#regranted = true;
			if (!reader.synced) continue;
			reader.synced = false;
			reader.announced.behind();
		}
	}

	/** Feed `announced` what the stream carries for `request`, until its last consumer leaves. */
	attach(announced: announce.Producer, request: Request): void {
		if (this.#closing) {
			announced.close();
			return;
		}
		const reader: Reader = { ...request, announced, reported: new Map(), synced: false };
		this.#readers.add(reader);
		// What the stream already carries shows before the reader counts as synced.
		const paths = new Set<Path.Valid>();
		for (const path of this.#byPath.keys()) {
			const at = presentedAt(reader, path);
			if (at !== undefined) paths.add(at);
		}
		for (const path of paths) this.#refresh(reader, path, false);
		this.#ask();
		this.#release();
		void race([announced.closed]).then(() => {
			if (!this.#readers.delete(reader)) return;
			this.#ask();
			this.#release();
		});
	}

	// The covering set of what the readers want: no prefix at or below another. A prefix asks for
	// hidden claims when a reader beneath it does, or sits behind a hidden segment itself.
	#wanted(): Map<Path.Valid, boolean> {
		const heads = new Set<Path.Valid>();
		for (const reader of this.#readers) heads.add(reader.prefix);
		const covering = (head: Path.Valid): Path.Valid => {
			let top = head;
			for (const above of coveringPrefixes(head)) if (heads.has(above)) top = above;
			return top;
		};
		const wanted = new Map<Path.Valid, boolean>();
		for (const reader of this.#readers) {
			const top = covering(reader.prefix);
			const hidden = reader.hidden || hiddenBelow(top, reader.prefix);
			wanted.set(top, (wanted.get(top) ?? false) || hidden);
		}
		// The whole set has to fit one request: past that, everything is asked for at the root.
		if (!requestFits(wanted.keys(), this.#session.version)) {
			const hidden = [...this.#readers].some(
				(reader) => reader.hidden || hiddenBelow(Path.empty(), reader.prefix),
			);
			return new Map([[Path.empty(), hidden]]);
		}
		return wanted;
	}

	// The edit that brings what is asked of the peer in line with what the readers want.
	#edit(): { remove: bigint[]; add: Add[] } {
		const wanted = this.#wanted();
		const remove: bigint[] = [];
		for (const [id, asked] of this.#asked) {
			if (wanted.get(asked.path) === asked.hidden) wanted.delete(asked.path);
			else remove.push(id);
		}
		return { remove, add: [...wanted].map(([prefix, hidden]) => ({ prefix, hidden })) };
	}

	// Ask the peer for what the readers now want, one edit at a time.
	#ask(): void {
		this.#regrant();
		if (this.#editing || this.#finished || this.#silent || this.#closing) return;
		const { remove, add } = this.#edit();
		if (remove.length === 0 && add.length === 0 && !this.#regranted) return;
		this.#regranted = false;
		const added: bigint[] = [];
		for (const { prefix, hidden } of add) {
			const id = this.#nextPrefix++;
			this.#asked.set(id, { path: prefix, hidden, live: false, synced: false });
			added.push(id);
		}
		this.#editing = { remove, add: added };
		console.debug(`announced: remove=${remove.join(",")} add=${add.map(({ prefix }) => prefix).join(",")}`);
		// However large, the edit goes whole, in as many messages as it takes.
		for (const part of requestParts(remove, add, this.#session.version)) {
			this.#session.send(part).catch((err: unknown) => this.close(error(err)));
		}
	}

	// Let each reader see its prefix synced once its answer is in: the peer answered every
	// request, and sent the initial set of each prefix asked for it.
	#release(): void {
		const done = this.#finished || this.#silent;
		if (!done) {
			this.#ask();
			if (this.#editing) return;
			const { remove, add } = this.#edit();
			if (remove.length > 0 || add.length > 0) return;
		}
		for (const reader of this.#readers) {
			if (reader.synced) continue;
			const answered = [...this.#asked.values()].every(
				(asked) =>
					asked.synced ||
					!(Path.hasPrefix(asked.path, reader.prefix) || Path.hasPrefix(reader.prefix, asked.path)),
			);
			if (!done && !answered) continue;
			reader.synced = true;
			reader.announced.synced();
		}
	}

	/** This side finished its stream: the prefixes asked of the peer stay as they are. */
	silence(): void {
		this.#silent = true;
		this.#release();
	}

	/** The peer finished its stream: it is leaving, its claims go, and it answers nothing more. */
	finish(): void {
		this.#finished = true;
		for (const id of [...this.#claims.keys()]) this.#end(id, true);
		this.#release();
	}

	/** The session ended: every reader ends with `err`, and every claim goes. */
	close(err?: Error): void {
		if (this.#closing) return;
		this.#closing = true;
		this.#stopDrain?.();
		this.#stopGrant?.();
		for (const live of this.#claims.values()) {
			if (live.path !== undefined && live.taken) this.#session.consumes.withdraw(live.path, live.owner);
		}
		this.#claims.clear();
		this.#byPath.clear();
		const readers = [...this.#readers];
		this.#readers.clear();
		for (const reader of readers) reader.announced.close(err);
	}

	#claim(id: bigint): Live {
		const found = this.#claims.get(id);
		if (!found) throw new ProtocolViolation(`unknown announce id ${id}`);
		return found;
	}

	// A claim through this session: its cost past this session's link cost of 1, as Rust's
	// default, ranked last while the peer or a node on its way drains, then by the lowest
	// priority on its way, past this link's 0, and unavailable while no route reaches it.
	#candidate(live: Live, claim: Claim): WireRoute {
		return this.#session.drained(
			WireRoute.normalize({
				instance: live.instance,
				epoch: claim.epoch,
				cost: claim.cost + 1n,
				claimed: true,
				priority: Math.min(0, claim.priority),
				above: claim.above,
				draining: claim.draining || undefined,
				available: !claim.unavailable,
			}),
		);
	}

	// Whether `live` is shown: at a path this node can hold, on a run that was told available.
	#shown(live: Live): live is Live & { path: Path.Valid } {
		return live.path !== undefined && live.taken;
	}

	// Bring what the session shows of the claim `id` in line with `live`, which `was` shown or
	// not before the change. `restart` marks another epoch, whose consumes subscribe fresh, and
	// `lost` a claim let go rather than withdrawn.
	#serve(id: bigint, live: Live, restart: boolean, lost: boolean, was = this.#shown(live)) {
		if (live.path === undefined) return;
		const consumes = this.#session.consumes;
		let ids = this.#byPath.get(live.path);
		if (this.#shown(live)) {
			if (!ids) {
				ids = new Set();
				this.#byPath.set(live.path, ids);
			}
			// A claim shown anew or restarted iterates last, as the newest; a re-price keeps its
			// place, as the consume cache does. See mostSpecific.
			if (!was || restart) ids.delete(id);
			ids.add(id);
			consumes.announce(live.path, live.owner, this.#candidate(live, live.claim), restart);
		} else if (was) {
			ids?.delete(id);
			if (ids?.size === 0) this.#byPath.delete(live.path);
			consumes.withdraw(live.path, live.owner);
		} else {
			return;
		}
		this.#touch(live.path, lost);
	}

	// The claim `id` is gone: its publisher withdrew it unless `lost`.
	#end(id: bigint, lost: boolean) {
		const live = this.#claim(id);
		this.#history.end(id);
		this.#claims.delete(id);
		const was = this.#shown(live);
		live.taken = false;
		this.#serve(id, live, false, lost, was);
	}

	// The claims `reader` is shown at `path`, each with where it is filed and how it is shown:
	// those at the path itself, and at the reader's own prefix each claim at or above it, as the
	// peer presents a claim covering more than a requested prefix: that many segments more above
	// the path, so a claim exactly there still outranks it. Oldest first at each filing.
	#candidates(reader: Reader, path: Path.Valid): { prefix: Path.Valid; filed: WireRoute; shown: WireRoute }[] {
		const candidates: { prefix: Path.Valid; filed: WireRoute; shown: WireRoute }[] = [];
		for (const at of path === reader.prefix ? coveringPrefixes(path) : [path]) {
			const by = BigInt(Path.parts(path).length - Path.parts(at).length);
			for (const id of this.#byPath.get(at) ?? []) {
				const live = this.#claim(id);
				const shown = by === 0n ? live.claim : { ...live.claim, above: live.claim.above + by };
				candidates.push({
					prefix: at,
					filed: this.#candidate(live, live.claim),
					shown: this.#candidate(live, shown),
				});
			}
		}
		return candidates;
	}

	// Report to `reader` the best claim at `path`, or with `claims` each instance's, and each
	// one's end. `lost` marks the claims leaving as let go rather than withdrawn, so they go
	// offline rather than ending.
	#refresh(reader: Reader, path: Path.Valid, lost: boolean) {
		const { announced, scope } = reader;
		if (announced.closed.peek() !== undefined) return;
		// Keyed by instance with `claims`, and otherwise the one key `undefined`.
		const now = new Map<Instance.Valid | undefined, WireRoute>();
		if (visible(reader, path)) {
			const candidates = this.#candidates(reader, path);
			if (reader.claims) {
				// The closest claim of each instance.
				for (const { shown: next } of candidates) {
					const held = now.get(next.instance);
					if (!held || (next.above ?? 0n) < (held.above ?? 0n)) now.set(next.instance, next);
				}
			} else {
				// The same pick a consume of `path` makes from the cache, ranked where each claim is
				// filed, newest first at each filing.
				const best = mostSpecific(
					path,
					(prefix) => candidates.filter((candidate) => candidate.prefix === prefix).reverse(),
					(candidate) => candidate.filed,
				);
				if (best) now.set(undefined, best.shown);
			}
		}

		const captures = scopeCaptures(scope, path);
		const prev = reader.reported.get(path) ?? new Map<Instance.Valid | undefined, WireRoute>();
		// Arrivals first, so a reader sees a claim replaced rather than the path gone between.
		for (const [key, next] of now) {
			const was = prev.get(key);
			if (was && routesEqual(was, next)) continue;
			const restart = !!was && (was.instance !== next.instance || was.epoch !== next.epoch);
			const kind = !was ? "start" : restart ? "restart" : "update";
			console.debug(`announced: broadcast=${path} ${kind} instance=${next.instance}`);
			announced.append({ prefix: path, captures, kind, route: next });
		}
		for (const [key, was] of prev) {
			if (now.has(key)) continue;
			console.debug(`announced: broadcast=${path} active=false lost=${lost}`);
			announced.append({ prefix: path, captures, kind: lost ? "offline" : "end", route: was });
		}
		if (now.size > 0) reader.reported.set(path, now);
		else reader.reported.delete(path);
	}

	// Report the claims at `path` to every reader it shows under: at the path itself for a reader
	// above it, and at the reader's own prefix for one below.
	#touch(path: Path.Valid, lost: boolean) {
		for (const reader of this.#readers) {
			const at = presentedAt(reader, path);
			if (at !== undefined) this.#refresh(reader, at, lost);
		}
	}

	// ANNOUNCE_OK: the last edit takes effect on the peer's stream. Both ends drop every claim
	// within a prefix it removed, and the peer tells again what an added prefix covers.
	#ok() {
		const editing = this.#editing;
		if (!editing) throw new ProtocolViolation("ANNOUNCE_OK with no ANNOUNCE_REQUEST unanswered");
		this.#editing = undefined;
		const removed: Path.Valid[] = [];
		for (const id of editing.remove) {
			const asked = this.#asked.get(id);
			if (asked) removed.push(asked.path);
			this.#asked.delete(id);
		}
		// Whether a prefix in effect before the edit survives it: the peer's claims within one stay.
		const survives = [...this.#asked.values()].some((asked) => asked.live);
		for (const id of editing.add) {
			const asked = this.#asked.get(id);
			if (!asked) throw new Error("an added prefix");
			asked.live = true;
		}
		if (removed.length > 0) {
			for (const [id, live] of [...this.#claims]) {
				const { path } = live;
				// A claim whose path could not be resolved lies in a prefix this side cannot name.
				// The peer may still hold it, and ends it by its id, so it goes only when no earlier
				// prefix is left for it to lie in.
				const within = path === undefined ? !survives : removed.some((prefix) => Path.hasPrefix(prefix, path));
				// The peer never ended it: it only stopped telling this session.
				if (within) this.#end(id, true);
			}
		}
		// What the readers want may have moved while the peer answered.
		this.#ask();
		this.#release();
	}

	/** Apply one message the peer sent on its Announce Stream as a publisher. */
	async receive(msg: AnnounceBroadcast): Promise<void> {
		if (this.#closing) return;
		switch (msg.status) {
			case "ok":
				this.#ok();
				break;
			case "synced": {
				const asked = this.#asked.get(msg.prefix);
				if (!asked?.live) throw new ProtocolViolation(`SYNCED for unknown Prefix ID ${msg.prefix}`);
				if (asked.synced) throw new ProtocolViolation(`duplicate SYNCED for Prefix ID ${msg.prefix}`);
				asked.synced = true;
				this.#release();
				break;
			}
			case "request":
				// The peer's own request is answered by the publisher half.
				break;
			case "start": {
				// Every claim's path is relative to the session, whichever requested prefix it lies in.
				const path = this.#history.start(msg);
				const id = this.#history.next - 1n;
				if (path === undefined) console.warn(`refusing a claim: claim=${id} reason=path`);
				else checkAbove(path, msg.claim.above);
				for (const other of path === undefined ? [] : (this.#byPath.get(path) ?? [])) {
					if (this.#claim(other).instance === msg.instance)
						throw new ProtocolViolation(`${path} claimed twice for one instance`);
				}
				const live: Live = { path, instance: msg.instance, claim: msg.claim, taken: false, owner: {} };
				this.#claims.set(id, live);
				live.taken = !msg.claim.unavailable;
				this.#serve(id, live, false, false, false);
				break;
			}
			case "change": {
				const live = this.#claim(msg.id);
				if (live.path !== undefined) checkAbove(live.path, msg.claim.above);
				const was = this.#shown(live);
				const run = live.claim.epoch !== msg.claim.epoch || live.claim.above !== msg.claim.above;
				const restart = live.claim.epoch !== msg.claim.epoch;
				live.claim = msg.claim;
				// The bit counts only on a run this node already holds: another run is taken once
				// it is told available, and the one held before it is lost.
				live.taken = run ? !msg.claim.unavailable : live.taken || !msg.claim.unavailable;
				this.#serve(msg.id, live, restart, true, was);
				break;
			}
			case "endedId":
				this.#end(msg.id, msg.lost ?? false);
				break;
			case "skipped":
				break;
			case "active":
			case "update":
			case "ended":
				// Lite-07 names every claim's instance, and ends one by its Announce ID.
				throw new ProtocolViolation(`${msg.status} announcement on lite-07`);
		}
	}
}

// Whether `reader` reports a claim at `path`.
function visible(reader: Reader, path: Path.Valid): boolean {
	const matches = reader.covering ? scopeOverlaps(reader.scope, path) : reader.scope.matches(path);
	return matches && (reader.hidden || !hiddenBelow(reader.prefix, path));
}

// Where `reader` is shown a claim at `path`: there when it lies within the reader's prefix, at
// the prefix itself when it covers more, and nowhere otherwise.
function presentedAt(reader: Reader, path: Path.Valid): Path.Valid | undefined {
	if (Path.hasPrefix(reader.prefix, path)) return path;
	return Path.hasPrefix(path, reader.prefix) ? reader.prefix : undefined;
}

// The prefixes holding what the grant `next` shows that `before` did not: each of its prefixes
// `before` does not cover, less any too long for a path, which covers nothing a peer could name.
// Empty for a narrowing. Mirrors rs/moq-net's `Permit::beyond`.
function gained(before: Path.Prefixes | undefined, next: Path.Prefixes | undefined): Path.Valid[] {
	if (!before) return [];
	if (!next) return [Path.empty()];
	return next
		.toArray()
		.filter((prefix) => !before.matches(prefix.text))
		.map((prefix) => prefix.text as Path.Valid)
		.filter((region) => Path.check(region) === undefined);
}
