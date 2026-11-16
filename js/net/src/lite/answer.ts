// Lite-07: what this session sends on its Announce Stream as a publisher, answering the prefixes
// the peer asks for from this origin's own route.
//
// A browser forwards nothing, so each requested prefix carries at most one route, this origin's,
// and only while it shows the peer one of its claims. The peer edits its prefixes in place: each
// edit takes effect at the ANNOUNCE_OK answering it, where both ends carry the stream's routes
// over alike (`rekey.plan`), and the answer then sends only what the carried routes lack. Mirrors
// `publisher/routed.rs` in rs/moq-net for a node that does not proxy.

import { type GetPromise, type Getter, race } from "@moq/signals";
import type { Grant } from "../auth.ts";
import { ProtocolViolation } from "../error.ts";
import { routesEqual, type WireRoute } from "../hop.ts";
import { coveringPrefixes, hiddenBelow, presented } from "../internal.ts";
import * as Path from "../path.ts";
import { type Advertised, type Advertisements, type OriginConsumer, sameInstance } from "../wire.ts";
import { type Add, type AnnounceBroadcast, type Offer, requestFits } from "./announce.ts";
import type { AnnounceStream } from "./announce_stream.ts";
import * as rekey from "./rekey.ts";
import type { Version } from "./version.ts";

/** What the session answers its peer from. */
export interface Publish {
	version: Version;
	/** The origin's claims, or undefined once it closed. */
	advertised: Getter<Advertisements | undefined>;
	/** The origin's route identity and scope, when the session publishes one. */
	origin?: Pick<OriginConsumer, "own" | "covers">;
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

/** The route the peer holds for a requested prefix: from the first claim shown until the last one goes. */
interface Sent {
	id: bigint;
	node: bigint;
	/** Whether its last floor certified, so a grant change alone floors it again. Undefined until
	 * its first floor, and once an edit widened it. */
	certified?: boolean;
	/** The claims the peer holds, by path. */
	live: Map<Path.Valid, { id: bigint; snap: Advertised }>;
}

// What the peer decodes for a claim: lite-07 carries its instance, epoch, and cost.
const onWire = (route: WireRoute): WireRoute => ({ ...route, auth: undefined });
const restarted = (prev: Advertised, next: Advertised): boolean =>
	!sameInstance(prev, next) || (prev.route.epoch ?? 0n) !== (next.route.epoch ?? 0n);

/** Answers the prefixes a lite-07 peer asks for. @internal */
export class Answer {
	#publish: Publish;
	/** The prefixes in effect, by Prefix ID. */
	#prefixes = new Map<bigint, Prefix>();
	#nextPrefix = 0n;
	/** The peer's edit not put in effect yet. */
	#request?: { remove: bigint[]; add: [bigint, Prefix][] };
	/** The ANNOUNCE_REQUESTs of an edit that continues: its removes and its adds so far. */
	#parts?: { remove: bigint[]; add: Add[] };
	/** The route the peer holds for each requested prefix. */
	#routes = new Map<Path.Valid, Sent>();
	#nextRoute = 0n;
	#nextAnnounce = 0n;
	/** The session is leaving: every route was ended, and nothing more is sent. */
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
		// The peer sends one edit at a time.
		if (this.#request) throw new ProtocolViolation("ANNOUNCE_REQUEST before the last was answered");
		if (this.#parts) {
			remove = [...this.#parts.remove, ...remove];
			add = [...this.#parts.add, ...add];
			this.#parts = undefined;
		}
		// Parts that could never make a valid edit fail before more of them pile up: their Adds
		// alone already pass the set's limit, or they remove more than the set holds.
		if (
			remove.length > this.#prefixes.size ||
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
		const left = new Map(this.#prefixes);
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
		const { advertised, grant, origin, closing, ready } = this.#publish;
		// Woken by the origin's claims, our grant, or the identity they are originated under.
		const stops = [advertised, grant, origin?.own.identity].map((source) => source?.subscribe(() => this.#poke()));
		try {
			const settled = ready.then(() => "ready" as const);
			if ((await race([settled, closing])) !== "ready") return;
			for (;;) {
				if (this.#closed) return;
				if (closing.peek()) return await this.#withdraw();
				if (this.#request) {
					await this.#edit(this.#request);
					continue;
				}
				if (this.#dirty) {
					await this.#syncAll();
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

	// The session is leaving: end every route, so the peer lets go of them before the stream finishes.
	async #withdraw() {
		this.#withdrawn = true;
		const ids = [...this.#routes.values()].map((sent) => sent.id).sort((a, b) => (a < b ? -1 : 1));
		this.#routes.clear();
		if (ids.length > 0) await this.#publish.stream.send(...ids.map((id) => ({ status: "routeEnd", id }) as const));
	}

	// Put the peer's request in effect: answer it, carry the routes it holds over, send what they
	// lack, and close each added prefix's initial set.
	async #edit(request: { remove: bigint[]; add: [bigint, Prefix][] }) {
		const old = new Set<Path.Valid>();
		for (const prefix of this.#prefixes.values()) old.add(prefix.path);
		for (const id of request.remove) this.#prefixes.delete(id);
		const added: Path.Valid[] = [];
		for (const [id, prefix] of request.add) {
			this.#prefixes.set(id, prefix);
			added.push(prefix.path);
		}
		const next = new Set<Path.Valid>();
		for (const prefix of this.#prefixes.values()) next.add(prefix.path);
		this.#rekey(old, next, added);

		// The peer may send its next edit once this is on its way, and not before.
		const answered = this.#publish.stream.send({ status: "ok" });
		this.#request = undefined;
		await answered;
		await this.#syncAll();
		// Every route the peer holds in an added prefix has its floor by now.
		const synced = request.add.map(([prefix]) => ({ status: "synced", prefix }) as const);
		if (synced.length > 0) await this.#publish.stream.send(...synced);
	}

	// Carry the routes and claims the peer holds over an edit of the prefixes from `old` to
	// `next`, as the peer does at the ANNOUNCE_OK.
	#rekey(old: Set<Path.Valid>, next: Set<Path.Valid>, added: Path.Valid[]) {
		const table = new Map<bigint, rekey.Route>();
		const sources = new Map<bigint, Sent>();
		const snaps = new Map<bigint, Advertised>();
		// Every route here is this origin's own along one offer, so they rank alike whoever the peer is.
		const order = rekey.order(false, this.#offer(), this.#offer().path, 0n);
		for (const [prefix, sent] of this.#routes) {
			const claims = new Map<Path.Valid, bigint>();
			for (const [path, { id, snap }] of sent.live) {
				claims.set(path, id);
				snaps.set(id, snap);
			}
			table.set(sent.id, { node: sent.node, prefix, region: prefix, order, claims });
			sources.set(sent.id, sent);
		}
		const plan = rekey.plan(old, next, added, table, this.#nextRoute, this.#nextAnnounce);
		this.#routes = new Map();
		for (const carried of plan.routes) {
			const source = sources.get(carried.from);
			if (!source) throw new Error("a carried route's source");
			const live = new Map<Path.Valid, { id: bigint; snap: Advertised }>();
			for (const [path, claim] of carried.claims) {
				const snap = snaps.get(claim.from);
				if (!snap) throw new Error("a carried claim's state");
				live.set(path, { id: claim.id, snap });
			}
			this.#routes.set(carried.prefix, {
				id: carried.id,
				node: source.node,
				// A widened route certifies nothing until its next floor.
				certified: carried.kind === "widens" ? undefined : source.certified,
				live,
			});
		}
		this.#nextRoute = plan.nextRoute;
		this.#nextAnnounce = plan.nextClaim;
	}

	// The origin's own route: a path of its own hop alone, learned on no session, at the top
	// priority and no cost. It forwards no other. Mirrors the origin's offer in rs/moq-net.
	#offer(): Offer {
		const origin = this.#publish.origin;
		return {
			path: origin ? [{ node: origin.own.hop, session: 0n }] : [],
			draining: false,
			priority: 127,
			metric: 0n,
			downOnly: false,
		};
	}

	async #syncAll() {
		this.#dirty = false;
		const ids = [...this.#prefixes.keys()].sort((a, b) => (a < b ? -1 : 1));
		for (const id of ids) {
			const prefix = this.#prefixes.get(id);
			if (!prefix) continue;
			const msgs = this.#sync(id, prefix);
			if (msgs.length > 0) await this.#publish.stream.send(...msgs);
		}
	}

	// Bring the peer's view of one requested prefix in line with the origin's: one batch of claim
	// changes, closed by its floor, ending the route once it shows nothing.
	#sync(id: bigint, prefix: Prefix): AnnounceBroadcast[] {
		const { advertised, origin } = this.#publish;
		const msgs: AnnounceBroadcast[] = [];
		// Read together, so the floor certifies exactly the claims it closes.
		const table = advertised.peek();
		const identity = origin?.own.identity.peek();
		const revision = origin?.own.revision();
		const grant = this.#publish.grant?.peek();
		// The claims the peer is shown, by their path from the session's root: the table under the
		// prefix, less the hidden ones it did not ask for and whatever our grant does not let us
		// publish.
		const latest = new Map<Path.Valid, Advertised>();
		if (table && identity) {
			const carries = (covered: Path.Valid) => prefix.hidden || !hiddenBelow(prefix.path, covered);
			for (const [suffix, snap] of presented(prefix.path, table, carries)) {
				const path = Path.join(prefix.path, suffix);
				if (!grant || grant.publish.matches(path)) latest.set(path, snap);
			}
		}
		// Whether a floor may certify the whole requested prefix: nothing the handle's scope or our
		// grant leaves out lies beneath it, since the peer cannot tell what was left out from what
		// is gone. A hidden claim left out does not count: the peer did not ask to see it.
		const certifies =
			(!grant || grant.publish.contains(Path.Pattern.subtree(prefix.path))) &&
			(origin?.covers(prefix.path) ?? true);

		// End the route on the wire: it retires every claim under it.
		const end = () => {
			const sent = this.#routes.get(prefix.path);
			if (!sent) return;
			this.#routes.delete(prefix.path);
			msgs.push({ status: "routeEnd", id: sent.id });
		};

		// A rotated identity ends the old route with every claim under it.
		if (this.#routes.get(prefix.path)?.node !== identity?.node) end();
		if (!identity) return msgs;

		let route = this.#routes.get(prefix.path);
		let changed = false;
		for (const [path, live] of [...(route?.live ?? [])]) {
			if (latest.has(path)) continue;
			route?.live.delete(path);
			changed = true;
			console.debug(`announce: broadcast=${path} active=false`);
			msgs.push({ status: "endedId", id: live.id });
		}
		for (const [path, snap] of latest) {
			const prev = route?.live.get(path);
			if (prev && !restarted(prev.snap, snap) && routesEqual(onWire(prev.snap.route), onWire(snap.route))) {
				continue;
			}
			changed = true;
			if (!route) {
				route = { id: this.#nextRoute++, node: identity.node, live: new Map() };
				this.#routes.set(prefix.path, route);
				// The origin's own route covers every path, so the requested prefix too.
				msgs.push({
					status: "routeStart",
					node: identity.node,
					prefix: id,
					suffix: Path.empty(),
					offer: this.#offer(),
				});
			}
			const { instance, epoch, cost } = snap.route;
			if (!prev) {
				const claim = this.#nextAnnounce++;
				route.live.set(path, { id: claim, snap });
				console.debug(`announce: broadcast=${path} active=true instance=${instance}`);
				// Every claim's path is relative to the session, whichever prefix it answers.
				msgs.push({ status: "active", suffix: path, route: route.id, instance, epoch, cost });
			} else if (restarted(prev.snap, snap)) {
				route.live.set(path, { id: prev.id, snap });
				console.debug(`announce: broadcast=${path} restart=true instance=${instance} epoch=${epoch}`);
				msgs.push({ status: "restart", id: prev.id, instance, epoch, cost });
			} else {
				route.live.set(path, { id: prev.id, snap });
				msgs.push({ status: "update", id: prev.id, cost });
			}
		}
		if (!route) return msgs;
		if (changed || route.certified !== certifies) {
			route.certified = certifies;
			msgs.push({ status: "floor", id: route.id, revision: certifies ? revision : undefined });
		}
		// A route an edit carried over with nothing to show ends like one whose last claim went.
		if (route.live.size === 0) end();
		return msgs;
	}
}
