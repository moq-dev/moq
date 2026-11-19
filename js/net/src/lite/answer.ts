// Lite-07: what this session sends on its Announce Stream as a publisher, answering the prefixes
// the peer asks for from this origin's own claims.
//
// A browser forwards nothing and declares no Node ID, so it sends claims and never routes: one
// per broadcast the peer is shown, at no cost. The peer edits its prefixes in place: each edit
// takes effect at the ANNOUNCE_OK answering it, where both ends drop every claim within a prefix
// it removed, and the answer then tells what the added prefixes cover.
//
// A prefix wider than our grant is answered region by region, as if the peer had asked for each
// granted region inside it: a claim above a region is presented at the region, where the grant
// covers it. A grant change is answered before the ANNOUNCE_OK of the next edit.

import { type GetPromise, type Getter, race } from "@moq/signals";
import type { Grant } from "../auth.ts";
import { ProtocolViolation } from "../error.ts";
import { coveringPrefixes, hiddenBelow, presented, scopeHead } from "../internal.ts";
import * as Path from "../path.ts";
import { type Advertised, type Advertisements, type OriginConsumer, sameInstance } from "../wire.ts";
import { type Add, type AnnounceBroadcast, type Claim, claimsEqual, ownClaim, requestFits } from "./announce.ts";
import type { AnnounceStream } from "./announce_stream.ts";
import type { Version } from "./version.ts";

/** What the session answers its peer from. */
export interface Publish {
	version: Version;
	/** The origin's claims, or undefined once it closed. */
	advertised: Getter<Advertisements | undefined>;
	/** The origin's scope, when the session publishes one. */
	origin?: Pick<OriginConsumer, "covers">;
	/** Our grant: only what it lets us publish is announced. */
	grant?: Getter<Grant | undefined>;
	/** Resolves once the tokens this session presented at setup are answered, so nothing is
	 * announced before the grant it would be checked against. */
	ready: Promise<void>;
	/** Settles when the session starts leaving. */
	closing: GetPromise<true>;
	stream: AnnounceStream;
}

/** A prefix the peer asked for. */
interface Prefix {
	path: Path.Valid;
	hidden: boolean;
}

/** A claim the peer holds: its Announce ID, the broadcast it serves, and what it was sent. */
interface Sent {
	id: bigint;
	snap: Advertised;
	claim: Claim;
}

/** Answers the prefixes a lite-07 peer asks for. @internal */
export class Answer {
	#publish: Publish;
	/** The prefixes in effect, by Prefix ID. */
	#prefixes = new Map<bigint, Prefix>();
	#nextPrefix = 0n;
	/** The peer's edit not answered yet. */
	#request?: { remove: bigint[]; add: [bigint, Prefix][] };
	/** The ANNOUNCE_REQUESTs of an edit that continues: its removes and its adds so far. */
	#parts?: { remove: bigint[]; add: Add[] };
	/** The claims the peer holds, by their path from the session's root. */
	#live = new Map<Path.Valid, Sent>();
	#nextAnnounce = 0n;
	/** The session is leaving: nothing more is sent. */
	#withdrawn = false;
	#closed = false;

	// Anything that may change what the peer is owed marks the answer dirty and wakes the loop,
	// which alone writes. A change landing mid-sync leaves it dirty for the next pass.
	#dirty = true;
	#notify?: () => void;
	#running?: Promise<void>;

	constructor(publish: Publish) {
		this.#publish = publish;
	}

	#poke() {
		this.#dirty = true;
		this.#notify?.();
	}

	/** Whether the peer's edit continues in its next message, which must be its next ANNOUNCE_REQUEST. */
	continuing(): boolean {
		return this.#parts !== undefined;
	}

	/**
	 * ANNOUNCE_REQUEST: the peer edits the prefixes it asks for, over as many messages as `more`
	 * says. Returns the loop answering it, started by the first request and running until the
	 * session leaves.
	 */
	request(more: boolean, remove: bigint[], add: Add[]): Promise<void> {
		this.#running ??= this.#run();
		if (this.#withdrawn || this.#closed) return this.#running;
		// One edit at a time, so a peer that never reads its answers leaves one here beside the
		// one being answered.
		if (this.#request) throw new ProtocolViolation("ANNOUNCE_REQUEST before the last was answered");
		if (this.#parts) {
			remove = [...this.#parts.remove, ...remove];
			add = [...this.#parts.add, ...add];
			this.#parts = undefined;
		}
		const left = new Map(this.#prefixes);
		// Parts that could never make a valid edit fail before more of them pile up: their Adds
		// alone already pass the set's limit, or they remove more than the set holds.
		if (
			remove.length > left.size ||
			!requestFits(
				add.map(({ prefix }) => prefix),
				this.#publish.version,
			)
		)
			throw new ProtocolViolation("ANNOUNCE_REQUEST parts past the limit");
		if (more) {
			this.#parts = { remove, add };
			return this.#running;
		}
		for (const id of remove) {
			if (!left.delete(id)) throw new ProtocolViolation(`ANNOUNCE_REQUEST removes unknown Prefix ID ${id}`);
		}
		const set = new Set<Path.Valid>();
		for (const prefix of left.values()) set.add(prefix.path);
		for (const { prefix } of add) {
			if (set.has(prefix)) throw new ProtocolViolation(`ANNOUNCE_REQUEST adds ${prefix} twice`);
			set.add(prefix);
		}
		// No prefix in the set lies below another, and the whole set fits one message.
		for (const prefix of set) {
			for (const above of coveringPrefixes(prefix)) {
				if (above !== prefix && set.has(above))
					throw new ProtocolViolation(`ANNOUNCE_REQUEST nests ${prefix} below ${above}`);
			}
		}
		if (!requestFits(set, this.#publish.version)) throw new ProtocolViolation("ANNOUNCE_REQUEST set too large");
		// Each added prefix takes the next Prefix ID, in the order the edit names them.
		const added = add.map(({ prefix, hidden }): [bigint, Prefix] => [this.#nextPrefix++, { path: prefix, hidden }]);
		this.#request = { remove, add: added };
		this.#notify?.();
		return this.#running;
	}

	/** The session ended: stop answering. */
	close() {
		this.#closed = true;
		this.#notify?.();
	}

	async #run(): Promise<void> {
		const { advertised, grant, closing, ready } = this.#publish;
		// Woken by the origin's claims or our grant.
		const stops = [advertised, grant].map((source) => source?.subscribe(() => this.#poke()));
		try {
			const settled = ready.then(() => "ready" as const);
			if ((await race([settled, closing])) !== "ready") return;
			for (;;) {
				if (this.#closed) return;
				// The session is leaving: finishing the stream ends everything the peer was sent.
				if (closing.peek()) {
					this.#withdrawn = true;
					this.#live.clear();
					return;
				}
				const request = this.#request;
				if (request) {
					// Let go before its answer is written: the peer may send its next edit the
					// moment it reads the answer, which can be before the write returns here.
					this.#request = undefined;
					await this.#edit(request);
					continue;
				}
				if (this.#dirty) {
					await this.#sync();
					continue;
				}
				const woke = new Promise<void>((resolve) => {
					this.#notify = resolve;
				});
				await race([woke, closing]);
				this.#notify = undefined;
			}
		} finally {
			for (const stop of stops) stop?.();
		}
	}

	// Put the peer's request in effect: answer it, drop what the removed prefixes covered, tell
	// what the added ones cover, and close each added prefix's initial set.
	async #edit(request: { remove: bigint[]; add: [bigint, Prefix][] }) {
		// A grant change the peer may have seen before this edit is answered before it.
		if (this.#dirty) await this.#sync();
		const removed: Path.Valid[] = [];
		for (const id of request.remove) {
			const prefix = this.#prefixes.get(id);
			if (prefix) removed.push(prefix.path);
			this.#prefixes.delete(id);
		}
		// Both ends drop every claim the removed prefixes covered.
		for (const path of [...this.#live.keys()]) {
			if (removed.some((prefix) => Path.hasPrefix(prefix, path))) this.#live.delete(path);
		}
		for (const [id, prefix] of request.add) this.#prefixes.set(id, prefix);

		await this.#publish.stream.send({ status: "ok" });
		await this.#sync();
		const synced = request.add.map(([prefix]) => ({ status: "synced", prefix }) as const);
		if (synced.length > 0) await this.#publish.stream.send(...synced);
	}

	// Bring the claims the peer holds in line with the origin's.
	async #sync() {
		this.#dirty = false;
		const msgs = this.#changes();
		if (msgs.length > 0) await this.#publish.stream.send(...msgs);
	}

	// How the claims the peer holds differ from what the requested prefixes show it now.
	#changes(): AnnounceBroadcast[] {
		const { advertised, origin } = this.#publish;
		const table = origin ? advertised.peek() : undefined;
		const grant = this.#publish.grant?.peek();
		const granted = grant && regions(grant.publish);
		// The claims the peer is shown, by their path from the session's root: the table under each
		// prefix, less the hidden ones it did not ask for and whatever our grant does not let us
		// publish, which is kept apart to tell a claim it no longer covers from one that ended.
		const latest = new Map<Path.Valid, Advertised>();
		const ungranted = new Set<Path.Valid>();
		const ids = [...this.#prefixes.keys()].sort((a, b) => (a < b ? -1 : 1));
		for (const id of table ? ids : []) {
			const prefix = this.#prefixes.get(id);
			if (!prefix || !table) continue;
			const carries = (covered: Path.Valid) => prefix.hidden || !hiddenBelow(prefix.path, covered);
			for (const read of reads(prefix.path, granted)) {
				// A claim covering what is read from above keeps how many segments broader than it it is.
				for (const [suffix, snap] of presented(read, table, carries, true)) {
					const path = Path.join(read, suffix);
					if (!grant || grant.publish.matches(path)) latest.set(path, snap);
					else ungranted.add(path);
				}
			}
		}

		const msgs: AnnounceBroadcast[] = [];
		const end = (path: Path.Valid, sent: Sent, lost: boolean) => {
			this.#live.delete(path);
			console.debug(`announce: broadcast=${path} active=false lost=${lost}`);
			msgs.push({ status: "endedId", id: sent.id, lost });
		};
		for (const [path, sent] of [...this.#live]) {
			// No longer granted, it is lost to the peer: its publisher never ended it.
			if (!latest.has(path)) end(path, sent, ungranted.has(path));
		}
		for (const [path, snap] of latest) {
			const { instance, epoch, cost } = snap.route;
			if (instance === undefined) continue;
			const claim = { ...ownClaim(epoch ?? 0n, snap.route.above ?? 0n), cost };
			const sent = this.#live.get(path);
			if (!sent || !sameInstance(sent.snap, snap)) {
				this.#live.set(path, { id: this.#nextAnnounce++, snap, claim });
				console.debug(`announce: broadcast=${path} active=true instance=${instance}`);
				// Every claim's path is relative to the session, whichever prefix it answers.
				msgs.push({ status: "start", suffix: path, instance, claim });
				// Another instance is another claim, started before the one it replaces ends, so
				// the peer sees the broadcast restart rather than go.
				if (sent) msgs.push({ status: "endedId", id: sent.id, lost: false });
			} else if (!claimsEqual(sent.claim, claim)) {
				this.#live.set(path, { id: sent.id, snap, claim });
				msgs.push({ status: "change", id: sent.id, claim });
			} else {
				sent.snap = snap;
			}
		}
		return msgs;
	}
}

// The regions `patterns` confine the peer to, none within another, or undefined when they allow
// every path. Mirrors rs/moq-net's `interest_prefixes`.
function regions(patterns: Path.Patterns): Path.Valid[] | undefined {
	if (patterns.contains(Path.Pattern.all())) return undefined;
	const heads = new Set(patterns.toArray().map(scopeHead));
	return [...heads].filter((head) => ![...heads].some((other) => other !== head && Path.hasPrefix(other, head)));
}

// What answers the requested `prefix`: the prefix itself when a region holds it or nothing confines
// the peer, and otherwise each region inside it.
function reads(prefix: Path.Valid, granted: Path.Valid[] | undefined): Path.Valid[] {
	if (!granted || granted.some((region) => Path.hasPrefix(region, prefix))) return [prefix];
	return granted.filter((region) => Path.hasPrefix(prefix, region));
}
