/**
 * Serve a front's logical track straight from its routes' copies. Mirrors `model::resume` in
 * `rs/moq-net`.
 *
 * A {@link Front} is one broadcast served through one route at a time: the origin re-sources it
 * when another route of the same epoch takes over, since routes with one epoch serve one
 * broadcast and so hold the same groups and frames. A reader of a logical track reads the
 * serving route's copy directly, through its own subscription: nothing is copied, and with one
 * route it is a passthrough.
 *
 * When the front switches, each reader subscribes to the new copy from the newest group it
 * handed out. It keeps replaced copies for unread groups they already had and groups still being
 * read, bounds their subscriptions after their newest group at replacement, and drops them once
 * neither needs them. A group carries on across the switch through its {@link Recover}: once its
 * copy fails with its route, or stalls while a newer route serves, it continues from the serving
 * route's copy of the same group at the same frame, found in that copy's cache or fetched from it.
 *
 * A group is handed out once however many copies deliver it: the reader skips any sequence its
 * subscriber already handed out, the same record that keeps a direct reader from taking a
 * re-created group twice.
 *
 * @module
 */
import { type Dispose, type Getter, race, Signal } from "@moq/signals";
import * as broadcast from "./broadcast.ts";
import type { Delivered } from "./delivered.ts";
import { NotFound, StreamCode, StreamError } from "./error.ts";
import type { Consumer as GroupConsumer } from "./group.ts";
import {
	type GroupRecover,
	groupBounds,
	hooks,
	type Recv,
	type RecvDatagram,
	type Replacement,
	type ResumeShell,
	type ResumeSource,
} from "./internal.ts";
import * as track from "./track.ts";
import { type Broadcast as BroadcastWire, registerWire, wireOf } from "./wire.ts";

// Stream codes that report the route failing rather than refusing: another route may still
// deliver what it failed. Anything that is not a stream reset (a session or transport error) is
// one too.
const ROUTE_FAILURES: ReadonlySet<number> = new Set([
	StreamCode.Internal,
	StreamCode.Cancel,
	StreamCode.SessionClosed,
	StreamCode.GoingAway,
	StreamCode.ControlTimeout,
]);

/** Whether `err` is the route failing rather than refusing: another route may still deliver what it failed. */
function routeFailed(err: Error): boolean {
	return !(err instanceof StreamError) || ROUTE_FAILURES.has(err.code);
}

/** Whether `err` is a route's verdict on a group it read: too old, evicted, or the application's. */
function verdict(err: Error): boolean {
	if (!(err instanceof StreamError)) return false;
	return (
		err.code === StreamCode.Old ||
		err.code === StreamCode.Evicted ||
		err.code === StreamCode.TooFarBehind ||
		err.code >= 64
	);
}

/** Two copies claiming the same content must agree on what it is. */
function compatible(a: track.Info, b: track.Info): boolean {
	return a.timescale === b.timescale && a.maxAge === b.maxAge && a.priority === b.priority;
}

/** One route's broadcast, held while the front serves it or a reader still reads a copy of it. */
class Source {
	readonly wire: BroadcastWire;
	/** The table's handle this was served from, so serving it again is a no-op. */
	readonly from: broadcast.Consumer;
	#consumer: broadcast.Consumer;
	#holds = 0;

	constructor(from: broadcast.Consumer) {
		this.from = from;
		this.#consumer = from.clone();
		this.wire = wireOf(this.#consumer);
	}

	retain(): void {
		this.#holds++;
	}

	release(): void {
		if (--this.#holds === 0) this.#consumer.close();
	}
}

/** What the front says about one logical track: the copy to read now, and how it ended. */
class Route {
	readonly name: string;
	/** Bumped whenever `source` changes, so a reader can tell a replacement apart. */
	generation = 0;
	/** The serving route's broadcast; undefined between routes or once the front let go. */
	source?: Source;
	/** No front is left to replace the copy: readers follow it to its end. */
	concluded = false;
	/** The front refused the replacement: the track ends with this once the serving copy runs out. */
	refusal?: Error;
	/** Bumped on every change above, waking parked reads. */
	readonly changed = new Signal(0);
	readonly readers = new Set<Reader>();
	/** Called once the last reader leaves. */
	idle: () => void = () => {};

	constructor(name: string) {
		this.name = name;
	}

	#bump(): void {
		this.changed.update((n) => n + 1);
	}

	/** Serve the track from `source` from now on, subscribing every reader to it. */
	serve(source: Source): void {
		if (this.concluded || this.source === source) return;
		this.generation++;
		this.source = source;
		this.#bump();
		for (const reader of [...this.readers]) reader.sync();
	}

	/** Between routes: readers wait for the next one, keeping what they have. */
	park(): void {
		if (!this.source) return;
		this.generation++;
		this.source = undefined;
		this.#bump();
	}

	/** End the track once the serving copy runs out, rather than replacing it. */
	refuse(err: Error): void {
		this.refusal ??= err;
		this.#bump();
	}

	/**
	 * No front will replace the serving copy. Same generation: a reader on the copy keeps it as
	 * the serving one, while a reader arriving later finds nothing to read.
	 */
	conclude(): void {
		if (this.concluded) return;
		this.concluded = true;
		this.source = undefined;
		this.#bump();
	}

	leave(reader: Reader): void {
		if (this.readers.delete(reader) && this.readers.size === 0) this.idle();
	}
}

/** A route's copy, as one reader reads it. */
interface Copy {
	generation: number;
	source: Source;
	sub: track.Subscriber;
	/** Where the subscription was asked to start, so demand updates never ask for less. */
	floor?: number;
	/**
	 * Replaced by a newer route: nothing past this group, the newest it had then, is taken from
	 * it. `null` when it had none.
	 */
	until?: number | null;
	/** The group channel ran out: cleanly (`null`) or failed. */
	done?: Error | null;
	/** The datagram channel ran out. */
	datagramsDone?: Error | null;
	/** The newest datagram handed out before this copy was subscribed, which it may replay. */
	datagramsSeen?: number;
	/** Groups handed out from this copy and still read, which keep a replaced copy subscribed. */
	lease: number;
}

/** Subscribe to a route's copy, or to a failed stand-in when that route can no longer serve. */
function subscribe(wire: BroadcastWire, name: string, subscription: track.Subscription): track.Subscriber {
	try {
		return wire.subscribe(name, subscription);
	} catch (err) {
		const failed = new track.Producer(name);
		failed.close(err instanceof Error ? err : new Error(String(err)));
		return failed.subscribe(subscription);
	}
}

/** The first group past where `copy` was replaced: 0 for one replaced before any group. */
function copyEnd(copy: Pick<Copy, "until">): number | undefined {
	if (copy.until === undefined) return undefined;
	return copy.until === null ? 0 : copy.until + 1;
}

/** One reader's subscription to a logical track; see the module docs. */
class Reader implements ResumeSource {
	readonly route: Route;
	#shell: ResumeShell;
	/** The route generation last read. */
	#generation?: number;
	/** Oldest first: the last one is the serving route's, when it is subscribed. */
	#copies: Copy[] = [];
	/** The newest group handed out: where a new copy is asked to start. */
	#newest?: number;
	/** The newest datagram handed out. */
	#datagram?: number;
	/** Bumped when this reader subscribes a new copy, waking its parked reads. */
	#changed = new Signal(0);
	#dispose: Dispose[] = [];
	#closed = false;

	constructor(route: Route, shell: ResumeShell) {
		this.route = route;
		this.#shell = shell;
		route.readers.add(this);
		this.#dispose.push(
			shell.subscription.subscribe(() => {
				for (const copy of this.#copies) this.#update(copy);
			}),
			shell.cursor.subscribe(() => {
				for (const copy of this.#copies) this.#bound(copy);
			}),
		);
		// Subscribed to the serving copy at once, so groups it has now are the reader's even if
		// the route changes before the first read, and to each copy after.
		this.sync();
	}

	/** Follow the route: subscribe to a new copy and bound the ones it replaced. */
	sync(): void {
		if (this.#closed) return;
		const route = this.route;
		if (this.#generation === route.generation) return;
		this.#generation = route.generation;
		const source = route.source;
		if (!source) return;
		// The replaced copies finish the groups they had when replaced, and nothing newer.
		for (const copy of this.#copies) {
			if (copy.until !== undefined) continue;
			const latest = copy.sub.latest();
			const newest = this.#newest === undefined ? latest : Math.max(this.#newest, latest ?? 0);
			copy.until = newest ?? null;
			this.#update(copy);
			this.#bound(copy);
		}
		const floor = this.#newest;
		const copy: Copy = {
			generation: route.generation,
			source,
			// The wire request carries the floor from the first message: some sessions read the
			// demand only once.
			sub: subscribe(source.wire, route.name, this.#wire({ floor })),
			floor,
			datagramsSeen: this.#datagram,
			lease: 0,
		};
		source.retain();
		this.#bound(copy);
		this.#copies.push(copy);
		const shell = this.#shell;
		copy.sub.info().then(
			(info) => shell.commit(info),
			() => {},
		);
		this.#changed.update((n) => n + 1);
	}

	/** What `copy` is asked for: the reader's own preferences, from its floor and through its replacement. */
	#wire(copy: Pick<Copy, "floor" | "until">): track.Subscription {
		const preferences = this.#shell.subscription.peek() ?? {};
		const bounds = groupBounds(preferences.groups);
		const start = Math.max(bounds.start, copy.floor ?? 0);
		const replaced = copyEnd(copy);
		const end = replaced === undefined ? bounds.end : Math.min(bounds.end ?? replaced, replaced);
		return {
			...preferences,
			groups: {
				start: start > 0 ? { included: start } : undefined,
				end: end === undefined ? undefined : { excluded: end },
			},
		};
	}

	#update(copy: Copy): void {
		if (copy.sub.closed.peek() === undefined) copy.sub.update(this.#wire(copy));
	}

	/**
	 * Apply the reader's group cursor to `copy`, never below the copy's own resume point: groups
	 * under it were handed out by an earlier route, or never owed.
	 */
	#bound(copy: Copy): void {
		const cursor = this.#shell.cursor.peek();
		const start = Math.max(cursor.start, copy.floor ?? 0);
		const replaced = copyEnd(copy);
		const end = replaced === undefined ? cursor.end : Math.min(cursor.end ?? replaced, replaced);
		hooks.replaceGroups(copy.sub, {
			start: { included: start },
			end: end === undefined ? undefined : { excluded: end },
		});
	}

	/** The reader's copy of the route at `generation`, if it subscribed one. */
	copy(generation: number): Copy | undefined {
		return this.#copies.find((copy) => copy.generation === generation);
	}

	#deliverable(copy: Copy, sequence: number, delivered: Delivered): boolean {
		const end = copyEnd(copy);
		return (end === undefined || sequence < end) && !delivered.has(sequence);
	}

	tryGroup(ordered: boolean, delivered: Delivered): Recv {
		this.sync();
		// The serving copy first: it is the one that keeps delivering.
		for (let index = this.#copies.length - 1; index >= 0; index--) {
			const copy = this.#copies[index];
			if (copy.done !== undefined) continue;
			for (;;) {
				const recv = ordered ? hooks.tryNextGroup(copy.sub) : hooks.tryRecvGroup(copy.sub);
				if (recv.kind === "group") {
					if (!this.#deliverable(copy, recv.group.sequence, delivered)) {
						recv.group.close();
						continue;
					}
					return { kind: "group", group: this.#handOut(copy, recv.group) };
				}
				// Drained: the track ended as the copy did, which is not cleanly when its
				// session went away short of the declared end.
				if (recv.kind === "done") copy.done = copy.sub.closed.peek() ?? null;
				else if (recv.kind === "error") copy.done = recv.error;
				break;
			}
		}
		this.#letGo(delivered);
		return this.#end((copy) => copy.done);
	}

	tryDatagram(): RecvDatagram {
		this.sync();
		for (let index = this.#copies.length - 1; index >= 0; index--) {
			const copy = this.#copies[index];
			// A replaced copy's datagrams are the serving one's to deliver.
			if (copy.datagramsDone !== undefined || copy.until !== undefined) continue;
			for (;;) {
				const recv = hooks.tryRecvDatagram(copy.sub);
				if (recv.kind === "datagram") {
					const sequence = recv.datagram.sequence;
					if (copy.datagramsSeen !== undefined && sequence <= copy.datagramsSeen) continue;
					this.#datagram = Math.max(this.#datagram ?? 0, sequence);
					return recv;
				}
				if (recv.kind === "done") copy.datagramsDone = copy.sub.closed.peek() ?? null;
				else if (recv.kind === "error") copy.datagramsDone = recv.error;
				break;
			}
		}
		const end = this.#end((copy) => copy.datagramsDone);
		if (end.kind === "error" || end.kind === "done") return end;
		return { kind: "idle" };
	}

	/**
	 * Whether the track ended, once the serving copy's channel (as `done` reads it) ran out:
	 * cleanly, or with a verdict a route failure does not excuse.
	 */
	#end(done: (copy: Copy) => Error | null | undefined): Recv & { kind: "idle" | "done" | "error" } {
		const route = this.route;
		const last = this.#copies.at(-1);
		const serving = last?.generation === this.#generation ? last : undefined;
		const ended = serving ? done(serving) : undefined;
		// The serving copy drained a complete track: that is its end, whichever route would
		// serve it next.
		if (serving && ended === null) return this.#finish({ kind: "done" });
		if (ended instanceof Error) {
			// A refusal from a route that still serves is the track's answer.
			if (!routeFailed(ended)) return this.#finish({ kind: "error", error: ended });
			if (route.refusal) return this.#finish({ kind: "error", error: route.refusal });
		}
		if (route.concluded) {
			// No front is left to replace the copy: the track ends as it does.
			if (!serving) return this.#finish({ kind: "error", error: dropped() });
			if (ended instanceof Error) return this.#finish({ kind: "error", error: ended });
		}
		return { kind: "idle" };
	}

	#finish<T extends Recv & { kind: "done" | "error" }>(end: T): T {
		this.#shell.close(end.kind === "error" ? end.error : undefined);
		return end;
	}

	/**
	 * Drop replaced copies only once neither a handed-out nor an unread group needs them. One not
	 * yet replaced stays, even between routes: it is still the newest copy there is.
	 */
	#letGo(delivered: Delivered): void {
		const cursor = this.#shell.cursor.peek();
		this.#copies = this.#copies.filter((copy) => {
			if (copy.until === undefined || copy.lease > 0) return true;
			if (copy.done === undefined && hooks.hasUnreadGroup(copy.sub, cursor.start, copyEnd(copy), delivered)) {
				return true;
			}
			this.#drop(copy);
			return false;
		});
	}

	#drop(copy: Copy): void {
		copy.sub.close();
		copy.source.release();
	}

	#handOut(copy: Copy, group: GroupConsumer): GroupConsumer {
		this.#newest = Math.max(this.#newest ?? 0, group.sequence);
		hooks.recoverGroup(group, new Recover(this, group, copy));
		return group;
	}

	changed(fn: () => void): Dispose {
		let fired = false;
		const disposes: Dispose[] = [];
		const dispose = () => {
			fired = true;
			for (const close of disposes) close();
		};
		const once = () => {
			if (fired) return;
			dispose();
			fn();
		};
		disposes.push(this.route.changed.changed(once), this.#changed.changed(once));
		for (const copy of this.#copies)
			disposes.push(hooks.groupChanged(copy.sub, once), hooks.datagramChanged(copy.sub, once));
		return dispose;
	}

	forgetPastBudget(delivered: Delivered): void {
		const newest = this.#copies.at(-1);
		if (newest) hooks.forgetPastBudget(newest.sub, delivered);
	}

	latest(): number | undefined {
		let latest: number | undefined;
		for (const copy of this.#copies) {
			const sequence = copy.sub.latest();
			if (sequence !== undefined && (latest === undefined || sequence > latest)) latest = sequence;
		}
		return latest;
	}

	#serving(): Copy | undefined {
		const last = this.#copies.at(-1);
		return last?.generation === this.#generation ? last : undefined;
	}

	end(): track.Location | undefined {
		this.sync();
		return this.#serving()?.sub.end();
	}

	async finished(): Promise<track.Location> {
		for (;;) {
			this.sync();
			const serving = this.#serving();
			const end = serving?.sub.end();
			if (end) return end;
			const failed = serving?.sub.closed.peek();
			// A copy that failed is only the end of the track if no front replaces it.
			if (failed instanceof Error && (this.route.concluded || !routeFailed(failed))) throw failed;
			if (!serving && this.route.concluded) throw dropped();
			const closed = this.#shell.closed.peek();
			if (closed instanceof Error) throw closed;
			if (closed !== undefined) throw new Error("track closed before its end was known");
			await new Promise<void>((resolve) => {
				const dispose = this.changed(() => {
					dispose();
					resolve();
				});
			});
		}
	}

	largest(): track.Location | undefined {
		this.sync();
		return this.#serving()?.sub.largest();
	}

	fork(options?: track.Subscription): track.Subscriber {
		this.sync();
		const serving = this.#serving();
		if (!serving) throw new Error("track has no serving route to fork from");
		return serving.sub.fork(options);
	}

	close(): void {
		if (this.#closed) return;
		this.#closed = true;
		for (const dispose of this.#dispose) dispose();
		this.#dispose = [];
		for (const copy of this.#copies) this.#drop(copy);
		this.#copies = [];
		this.route.leave(this);
	}
}

/** A fetch of a group's rest from one route generation. */
interface Fetch {
	generation: number;
	/** Set once the fetch settles. */
	outcome?: { group: GroupConsumer } | { error: Error };
	/** Fires once it settles. */
	settled: Signal<boolean>;
}

function startFetch(
	wire: BroadcastWire,
	name: string,
	sequence: number,
	generation: number,
	signal: AbortSignal,
): Fetch {
	const fetch: Fetch = { generation, settled: new Signal(false) };
	wire.fetchGroup(name, sequence, { signal })
		.then(
			(group) => {
				fetch.outcome = { group };
			},
			(err: unknown) => {
				fetch.outcome = { error: err instanceof Error ? err : new Error(String(err)) };
			},
		)
		.finally(() => fetch.settled.set(true));
	return fetch;
}

/** Resolves once `signal` aborts. */
function aborted(signal: AbortSignal): Promise<void> {
	return new Promise((resolve) => {
		if (signal.aborted) resolve();
		else signal.addEventListener("abort", () => resolve(), { once: true });
	});
}

function dropped(): Error {
	return new StreamError(StreamCode.Cancel, { message: "no route serves the track" });
}

/** Carries a group across route changes; see the module docs. */
class Recover implements GroupRecover {
	#reader: Reader;
	#sequence: number;
	/** The copy being read, whose subscription this group keeps alive while it is read. */
	#copy?: Copy;

	constructor(reader: Reader, group: GroupConsumer, copy: Copy) {
		this.#reader = reader;
		this.#sequence = group.sequence;
		this.#copy = copy;
		copy.lease++;
		void group.closed.then(() => this.#release());
	}

	#release(): void {
		if (this.#copy) this.#copy.lease--;
		this.#copy = undefined;
	}

	get changed(): Getter<unknown> {
		return this.#reader.route.changed;
	}

	wants(failed: Error | undefined): boolean {
		const copy = this.#copy;
		if (!copy) return false;
		// Only a route that serves can refuse a group on purpose.
		if (failed && (!verdict(failed) || copy.sub.closed.peek() instanceof Error)) return true;
		const route = this.#reader.route;
		return route.generation !== copy.generation || route.concluded;
	}

	async replace(_index: number, failed: Error | undefined, signal: AbortSignal): Promise<Replacement> {
		const reader = this.#reader;
		const route = reader.route;
		const abort = aborted(signal);
		let fetch: Fetch | undefined;
		for (;;) {
			signal.throwIfAborted();
			// Subscribe the reader to a new route from its newest group, which is this one or
			// later, so that route's subscription may deliver the rest of it.
			reader.sync();
			const current = this.#copy;
			if (!current) throw new Error("group closed");
			const generation = route.generation;
			const serving = route.source ? reader.copy(generation) : undefined;
			const waits: Getter<unknown>[] = [route.changed];
			if (!serving) {
				// Nothing serves: a failed copy waits for a route, unless none will come.
				if (failed && route.concluded) throw failed;
			} else if (generation !== current.generation || failed) {
				// A newer copy's track caches it: continue from there.
				if (generation !== current.generation) {
					const cached = hooks.cachedGroup(serving.sub, this.#sequence);
					if (cached) return this.#replacement(cached, serving);
				}
				// Otherwise ask for it. The subscription may still deliver it first.
				if (fetch?.generation !== generation)
					fetch = startFetch(serving.source.wire, route.name, this.#sequence, generation, signal);
				const stale = hooks.staleAt(serving.sub, this.#sequence);
				if (stale.expired()) throw new StreamError(StreamCode.Old, { message: "group is past the budget" });
				waits.push(...stale.changed, ...(fetch.outcome ? [] : [fetch.settled]));
				const outcome = fetch.outcome;
				if (outcome && "group" in outcome) return this.#replacement(outcome.group, serving);
				// That route failing too waits for the next one, and a refusal is a verdict only for
				// a copy that failed.
				if (outcome && failed && !routeFailed(outcome.error) && !(serving.sub.closed.peek() instanceof Error)) {
					throw outcome.error;
				}
			}
			await race([Signal.race(...waits), abort]);
		}
	}

	#replacement(group: GroupConsumer, copy: Copy): Replacement {
		return {
			group,
			expiry: hooks.staleAt(copy.sub, this.#sequence),
			adopt: () => {
				// A later switch must keep this copy subscribed while the recovered group still
				// reads it, just like a group handed out from that copy directly.
				this.#release();
				this.#copy = copy;
				copy.lease++;
			},
		};
	}
}

/**
 * One broadcast served through one route at a time; see the module docs.
 *
 * Its {@link consumer} is the broadcast handed out: every track read through it follows the
 * front's route. The origin calls {@link serve} as routes of the same epoch take over, and
 * {@link conclude} once none will: readers then follow the last copy to its end.
 */
export class Front {
	/** The logical broadcast; closing its last handle concludes the front. */
	readonly consumer: broadcast.Consumer;
	#source?: Source;
	#routes = new Map<string, Route>();
	/** Each track's properties as first committed, which every later copy must match. */
	#info = new Map<string, track.Info>();
	/** Bumped whenever the source changes, so an answer from a replaced one is dropped. */
	#changed = new Signal(0);
	#concluded = false;

	constructor() {
		this.consumer = makeFrontConsumer(this);
		void this.consumer.closed.then(() => this.conclude());
	}

	/** Serve every track from `from` from now on, once each copy proves compatible. */
	serve(from: broadcast.Consumer): void {
		if (this.#concluded || this.#source?.from === from) return;
		const source = new Source(from);
		source.retain();
		const previous = this.#source;
		this.#source = source;
		this.#changed.update((n) => n + 1);
		for (const route of this.#routes.values()) this.#splice(route, source);
		previous?.release();
	}

	/** Between routes: readers keep what they have and wait for the next one. */
	park(): void {
		if (this.#concluded || !this.#source) return;
		const previous = this.#source;
		this.#source = undefined;
		this.#changed.update((n) => n + 1);
		for (const route of this.#routes.values()) route.park();
		previous.release();
	}

	/** No route will replace the serving one: readers follow it to its end. Idempotent. */
	conclude(): void {
		if (this.#concluded) return;
		this.#concluded = true;
		for (const route of this.#routes.values()) route.conclude();
		this.#source?.release();
		this.#source = undefined;
		this.#changed.update((n) => n + 1);
	}

	/** Whether the serving broadcast came from a session; see {@link BroadcastWire.fromSession}. */
	get fromSession(): boolean {
		return this.#source?.wire.fromSession ?? false;
	}

	// Splice `source`'s copy of a track in, once it claims the same content as the copies before.
	#splice(route: Route, source: Source): void {
		const expected = this.#info.get(route.name);
		if (!expected) {
			route.serve(source);
			return;
		}
		// The replaced copy keeps feeding the track while the new one answers.
		source.retain();
		source.wire
			.resolveTrackInfo(route.name)
			.then(
				(info) => {
					if (this.#source !== source) return;
					// A copy claiming the same content with different metadata is refused rather
					// than reinterpreted.
					if (compatible(expected, info)) route.serve(source);
					else route.refuse(new Error(`track ${route.name} changed its properties across routes`));
				},
				(err: unknown) => {
					if (this.#source !== source) return;
					const error = err instanceof Error ? err : new Error(String(err));
					// A route on its way out is no verdict: the next one is asked.
					if (!routeFailed(error)) route.refuse(error);
				},
			)
			.finally(() => source.release());
	}

	#route(name: string): Route {
		let route = this.#routes.get(name);
		if (route) return route;
		route = new Route(name);
		const created = route;
		created.idle = () => {
			if (this.#routes.get(name) === created) this.#routes.delete(name);
		};
		this.#routes.set(name, created);
		if (this.#concluded) created.conclude();
		else if (this.#source) created.serve(this.#source);
		return created;
	}

	#record(name: string, info: track.Info): void {
		if (!this.#info.has(name)) this.#info.set(name, info);
	}

	subscribe(name: string, options?: track.Subscription): track.Subscriber {
		if (this.consumer.closed.peek() !== undefined) throw new Error("broadcast is closed");
		const route = this.#route(name);
		return hooks.resumeSubscriber(
			name,
			options,
			(shell) =>
				new Reader(route, {
					...shell,
					commit: (info) => {
						this.#record(name, info);
						shell.commit(info);
					},
				}),
		);
	}

	// Wait for the source to change, or `signal` to abort.
	async #next(signal?: AbortSignal): Promise<void> {
		signal?.throwIfAborted();
		await race(signal ? [this.#changed.changed(), aborted(signal)] : [this.#changed.changed()]);
		signal?.throwIfAborted();
	}

	async resolveTrackInfo(name: string, hold?: AbortSignal): Promise<track.Info> {
		for (;;) {
			const source = this.#source;
			if (!source) {
				if (this.#concluded) throw dropped();
				await this.#next(hold);
				continue;
			}
			source.retain();
			try {
				const info = await source.wire.resolveTrackInfo(name, hold);
				this.#record(name, info);
				return info;
			} catch (err) {
				const error = err instanceof Error ? err : new Error(String(err));
				// The route failed: ask the next one, unless none will come.
				if (!routeFailed(error) || this.#concluded || hold?.aborted) throw error;
				if (this.#source === source) await this.#next(hold);
			} finally {
				source.release();
			}
		}
	}

	largest(name: string): track.Location | undefined {
		return this.#source?.wire.largest(name);
	}

	async fetchGroup(name: string, sequence: number, options?: track.FetchGroupOptions): Promise<GroupConsumer> {
		const fetched = await this.#fetch(name, sequence, options);
		let current = fetched.source;
		void fetched.group.closed.then(() => current.release());
		// Carried across route changes like a subscribed group, but by fetching again: a fetch has
		// no copy of its own to find the rest in.
		hooks.recoverGroup(fetched.group, {
			changed: this.#changed,
			wants: (failed) => {
				// Too old and evicted judge a subscription's delivery, not whether the group
				// exists, so only the application's refusal is a verdict on a fetch.
				if (failed) return !(failed instanceof StreamError && failed.code >= 64);
				return this.#source !== undefined && this.#source !== current;
			},
			replace: async (_index, failed, signal) => {
				// A stalled fetch only moves to another route; a failed one may ask its own again.
				const next = await this.#fetch(name, sequence, { ...options, signal }, failed ? undefined : current);
				let adopted = false;
				void next.group.closed.then(() => {
					if (!adopted) next.source.release();
				});
				return {
					group: next.group,
					// The route applies the fetch's own budget.
					expiry: { expired: () => false, changed: [] },
					adopt: () => {
						adopted = true;
						current.release();
						current = next.source;
					},
				};
			},
		});
		return fetched.group;
	}

	// Fetch a group from whichever route serves the track, asking the next one when a route
	// fails, and never `avoid`. The source comes back retained for as long as the group is read.
	async #fetch(
		name: string,
		sequence: number,
		options: track.FetchGroupOptions | undefined,
		avoid?: Source,
	): Promise<{ group: GroupConsumer; source: Source }> {
		for (;;) {
			const source = this.#source;
			if (!source || source === avoid) {
				if (this.#concluded) throw new NotFound(`group ${sequence}`);
				await this.#next(options?.signal);
				continue;
			}
			source.retain();
			try {
				return { group: await source.wire.fetchGroup(name, sequence, options), source };
			} catch (err) {
				source.release();
				const error = err instanceof Error ? err : new Error(String(err));
				// The route failed: ask the next one, unless none will come.
				if (!routeFailed(error) || this.#concluded || options?.signal?.aborted) throw error;
				if (this.#source === source) await this.#next(options?.signal);
			}
		}
	}
}

let makeFrontConsumer: (front: Front) => FrontConsumer;

/** A front's logical broadcast: a consumer handle whose tracks follow the front's route. */
class FrontConsumer extends broadcast.Consumer {
	#front: Front;

	private constructor(front: Front, state?: never) {
		super(state);
		this.#front = front;
		const view = wireOf(this);
		registerWire(this, {
			requested: view.requested,
			subscribe: (name, options) => front.subscribe(name, options),
			resolveTrackInfo: (name, hold) => front.resolveTrackInfo(name, hold),
			largest: (name) => front.largest(name),
			fetchGroup: (name, sequence, options) => front.fetchGroup(name, sequence, options),
			get fromSession() {
				return front.fromSession;
			},
		});
	}

	static {
		makeFrontConsumer = (front) => new FrontConsumer(front);
	}

	// Preserve the subclass, and the front behind it, when a handle is shared.
	override clone(): FrontConsumer {
		return new FrontConsumer(this.#front, this.shareState());
	}
}
