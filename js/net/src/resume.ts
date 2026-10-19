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
 * handed out. A replaced copy keeps delivering until the new copy delivers its first group, then
 * is bounded just below it (or at the newest group handed out, if later), so a group only the old
 * route got after the switch is not lost. It is kept for unread groups it had and groups still
 * being read (from it, or from another copy while it holds them too), and dropped once neither
 * needs it. A group carries on across the switch through its {@link Recover}: once its copy
 * stalls while a newer route serves, it continues from the serving route's copy of the same group
 * at the same frame, found in that copy's cache or fetched from it. Once its copy fails, any other
 * copy still holding the group continues it first, since a group's name fixes its content.
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

/** A copy claiming the same content with different metadata: refused rather than reinterpreted. */
class Incompatible extends Error {
	constructor(name: string) {
		super(`track ${name} changed its properties across routes`);
		this.name = "Incompatible";
	}
}

/** Whether `err` is the route failing rather than refusing: another route may still deliver what it failed. */
function routeFailed(err: Error): boolean {
	if (err instanceof Incompatible) return false;
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

/**
 * Ask `source` what track `name` is, abandoning the question once `retired` aborts. A caller's
 * `hold` keeps the track wanted until it or `retired` aborts; without one, only until the answer.
 */
function ask(source: Source, name: string, retired: AbortSignal, hold?: AbortSignal): Promise<track.Info> {
	if (hold) return source.wire.resolveTrackInfo(name, AbortSignal.any([hold, retired]));
	const query = new AbortController();
	const onRetire = () => query.abort(retired.reason);
	if (retired.aborted) onRetire();
	else retired.addEventListener("abort", onRetire, { once: true });
	return source.wire.resolveTrackInfo(name, query.signal).finally(() => {
		retired.removeEventListener("abort", onRetire);
		query.abort();
	});
}

/** One route's broadcast, held while the front serves it or a reader still reads a copy of it. */
class Source {
	readonly wire: BroadcastWire;
	/** The table's handle this was served from, so serving it again is a no-op. */
	readonly from: broadcast.Consumer;
	/** Whether a session delivered it, read from the handle served rather than the clone. */
	readonly fromSession: boolean;
	#consumer: broadcast.Consumer;
	#holds = 0;

	constructor(from: broadcast.Consumer) {
		this.from = from;
		this.fromSession = wireOf(from).fromSession === true;
		this.#consumer = from.clone();
		this.wire = wireOf(this.#consumer);
	}

	retain(): void {
		this.#holds++;
	}

	release(): void {
		if (this.#holds === 0) throw new Error("route released more often than retained");
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
	/** The last broadcast served, kept between routes: what a replacement must match. */
	served?: Source;
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
		// A refusal ends the track: no later source continues it, compatible or not.
		if (this.concluded || this.refusal || this.source === source) return;
		this.generation++;
		this.source = source;
		this.served = source;
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
	/** Replaced by a newer route: its datagrams are the newer copy's to deliver. */
	replaced: boolean;
	/**
	 * Bounded once the newer copy delivered its first group: nothing past this group is taken
	 * from it. `null` when that leaves nothing. Unset while it is the only copy still feeding
	 * groups the newer one may never have.
	 */
	until?: number | null;
	/** The group channel ran out: cleanly (`null`) or failed. */
	done?: Error | null;
	/** The datagram channel ran out. */
	datagramsDone?: Error | null;
	/** The newest datagram handed out before this copy was subscribed, which it may replay. */
	datagramsSeen?: number;
	/**
	 * Groups still read that came from this copy, or that it held too when handed out, which keep
	 * a replaced copy subscribed.
	 */
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

/** Take a copy's next group, in sequence or arrival order. */
function take(copy: Copy, ordered: boolean): Recv {
	return ordered ? hooks.tryNextGroup(copy.sub) : hooks.tryRecvGroup(copy.sub, false);
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
	/** Bumped when this reader subscribes a new copy or a copy's last lease ends, waking its parked reads. */
	#changed = new Signal(0);
	#dispose: Dispose[] = [];
	#closed = false;
	/** The channels that ran out cleanly. */
	#drained = new Set<"groups" | "datagrams">();
	/** Records the properties a copy committed, and which source it came from. */
	#record: (info: track.Info, source: Source) => void;

	constructor(route: Route, shell: ResumeShell, record: (info: track.Info, source: Source) => void) {
		this.route = route;
		this.#shell = shell;
		this.#record = record;
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

	/** Follow the route: subscribe to a new copy and mark the ones it replaced. */
	sync(): void {
		if (this.#closed) return;
		const route = this.route;
		if (this.#generation === route.generation) return;
		this.#generation = route.generation;
		const source = route.source;
		if (!source) return;
		// The replaced copies keep delivering until the new one delivers its first group: a
		// group they get meanwhile may be one the new route never has. See #observe.
		for (const copy of this.#copies) copy.replaced = true;
		const floor = this.#newest;
		const copy: Copy = {
			generation: route.generation,
			source,
			// The wire request carries the floor from the first message: some sessions read the
			// demand only once.
			sub: subscribe(source.wire, route.name, this.#wire({ floor })),
			floor,
			datagramsSeen: this.#datagram,
			replaced: false,
			lease: 0,
		};
		source.retain();
		this.#bound(copy);
		this.#copies.push(copy);
		const shell = this.#shell;
		const record = this.#record;
		copy.sub.info().then(
			(info) => {
				record(info, source);
				shell.commit(info);
			},
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

	/** Wake parked reads, so they look again and let go of what nothing needs. */
	wake(): void {
		this.#changed.update((n) => n + 1);
	}

	/** The reader's copy of the route at `generation`, if it subscribed one. */
	copy(generation: number): Copy | undefined {
		return this.#copies.find((copy) => copy.generation === generation);
	}

	/** The copies other than `except` whose track still holds group `sequence` unfailed, newest first. */
	holding(sequence: number, except: Copy): Copy[] {
		const holding: Copy[] = [];
		for (let index = this.#copies.length - 1; index >= 0; index--) {
			const copy = this.#copies[index];
			if (copy !== except && hooks.holdsGroup(copy.sub, sequence)) holding.push(copy);
		}
		return holding;
	}

	/**
	 * `copy` handed out `sequence`. The serving copy's first group bounds the copies it replaced
	 * just below it: what they hold under it may be groups the new route never has, and what
	 * is at or above it the new route delivers. Never below a group already handed out. A group
	 * it replays, already handed out, bounds nothing: the replaced copies may still hold the next.
	 */
	#observe(copy: Copy, sequence: number): void {
		if (copy.generation !== this.#generation) return;
		const bound = Math.max(this.#newest ?? -1, sequence - 1);
		for (const replaced of this.#copies) {
			if (replaced === copy || !replaced.replaced || replaced.until !== undefined) continue;
			replaced.until = bound < 0 ? null : bound;
			this.#update(replaced);
			this.#bound(replaced);
		}
	}

	#deliverable(copy: Copy, sequence: number, delivered: Delivered): boolean {
		const end = copyEnd(copy);
		return (end === undefined || sequence < end) && !delivered.has(sequence);
	}

	tryGroup(ordered: boolean, delivered: Delivered): Recv {
		this.sync();
		// The lowest group any copy holds next, in either order: a replaced copy's buffered group
		// comes before a later one the serving copy already has. Arrival order still delivers a
		// lower group that truly arrives later.
		const next = this.#lowest(ordered, delivered);
		if (next) {
			const recv = take(next, ordered);
			if (recv.kind === "group") {
				this.#observe(next, recv.group.sequence);
				return { kind: "group", group: this.#handOut(next, recv.group) };
			}
		}
		// The serving copy first: it is the one that keeps delivering.
		for (let index = this.#copies.length - 1; index >= 0; index--) {
			const copy = this.#copies[index];
			if (copy.done !== undefined) continue;
			for (;;) {
				const recv = take(copy, ordered);
				if (recv.kind === "group") {
					if (!this.#deliverable(copy, recv.group.sequence, delivered)) {
						recv.group.close();
						continue;
					}
					this.#observe(copy, recv.group.sequence);
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
		return this.#end("groups", (copy) => copy.done);
	}

	// The copy whose next group is the lowest deliverable one, dropping any it holds next that
	// are not.
	#lowest(ordered: boolean, delivered: Delivered): Copy | undefined {
		let lowest: { copy: Copy; sequence: number } | undefined;
		for (const copy of this.#copies) {
			if (copy.done !== undefined) continue;
			for (;;) {
				const sequence = hooks.peekGroup(copy.sub, ordered);
				if (sequence === undefined) break;
				if (this.#deliverable(copy, sequence, delivered)) {
					// A tie goes to the newer copy, so the serving one hands out its first group and
					// bounds the copies it replaced.
					if (!lowest || sequence <= lowest.sequence) lowest = { copy, sequence };
					break;
				}
				const recv = take(copy, ordered);
				if (recv.kind !== "group") break;
				recv.group.close();
			}
		}
		return lowest?.copy;
	}

	tryDatagram(delivered: Delivered | undefined): RecvDatagram {
		this.sync();
		for (let index = this.#copies.length - 1; index >= 0; index--) {
			const copy = this.#copies[index];
			// A replaced copy's datagrams are the serving one's to deliver.
			if (copy.datagramsDone !== undefined || copy.replaced) continue;
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
		this.#letGo(delivered);
		const end = this.#end("datagrams", (copy) => copy.datagramsDone);
		if (end.kind === "error" || end.kind === "done") return end;
		return { kind: "idle" };
	}

	/**
	 * Whether the track ended, once the serving copy's channel (as `done` reads it) ran out:
	 * cleanly, or with a verdict a route failure does not excuse.
	 */
	#end(
		channel: "groups" | "datagrams",
		done: (copy: Copy) => Error | null | undefined,
	): Recv & { kind: "idle" | "done" | "error" } {
		const route = this.route;
		const last = this.#copies.at(-1);
		const serving = last?.generation === this.#generation ? last : undefined;
		const ended = serving ? done(serving) : undefined;
		// The serving copy drained a complete track: that is its end, whichever route would
		// serve it next.
		if (serving && ended === null) return this.#finish({ kind: "done" }, channel);
		if (ended instanceof Error) {
			// A refusal from a route that still serves is the track's answer.
			if (!routeFailed(ended)) return this.#finish({ kind: "error", error: ended }, channel);
			if (route.refusal) return this.#finish({ kind: "error", error: route.refusal }, channel);
		}
		// Refused before any copy fed it: nothing will.
		if (!last && route.refusal) return this.#finish({ kind: "error", error: route.refusal }, channel);
		if (route.concluded) {
			// No front is left to replace the copy: the track ends as it does.
			if (!serving) return this.#finish({ kind: "error", error: dropped() }, channel);
			if (ended instanceof Error) return this.#finish({ kind: "error", error: ended }, channel);
		}
		return { kind: "idle" };
	}

	// End the logical track on a failure, or cleanly once both channels drained: groups and
	// datagrams end apart, and one running out leaves the other's backlog readable.
	#finish<T extends Recv & { kind: "done" | "error" }>(end: T, channel: "groups" | "datagrams"): T {
		if (end.kind === "error") {
			this.#shell.close(end.error);
			return end;
		}
		this.#drained.add(channel);
		if (this.#drained.size === 2) this.#shell.close();
		return end;
	}

	/**
	 * Drop replaced copies only once neither a handed-out nor an unread group needs them. One not
	 * yet replaced stays, even between routes: it is still the newest copy there is.
	 */
	#letGo(delivered?: Delivered): void {
		const cursor = this.#shell.cursor.peek();
		this.#copies = this.#copies.filter((copy) => {
			// The serving copy, or one between routes with nothing replacing it yet.
			if (!copy.replaced || copy.lease > 0) return true;
			// Its group channel ran out, or nobody reads groups at all: it feeds nothing more.
			if (copy.done !== undefined || !delivered) {
				this.#drop(copy);
				return false;
			}
			// Not yet bounded, it may still feed a group the new copy never has.
			if (copy.until === undefined) return true;
			if (hooks.hasUnreadGroup(copy.sub, cursor.start, copyEnd(copy), delivered)) return true;
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
		// Another copy holding the group is where a failure continues it, so it stays while the
		// group is read. Only while copies overlap: a passthrough read has none.
		const backups = this.#copies.length > 1 ? this.holding(group.sequence, copy) : [];
		const recover = new Recover(this, group, copy, backups);
		hooks.recoverGroup(group, recover);
		recover.watch(group);
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
			const sequence = hooks.latestSequence(copy.sub);
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
			// A copy that failed is only the end of the track if no front replaces it, and a
			// refused replacement leaves the refusal as the answer.
			if (failed instanceof Error) {
				if (!routeFailed(failed)) throw failed;
				if (this.route.refusal) throw this.route.refusal;
				if (this.route.concluded) throw failed;
			}
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

	bufferedLargest(): track.Location | undefined {
		this.sync();
		const serving = this.#serving();
		return serving && hooks.bufferedLargest(serving.sub);
	}

	peekLargest(): { largest?: track.Location; ended: boolean } {
		this.sync();
		const serving = this.#serving();
		return serving ? hooks.peekLargest(serving.sub) : { ended: false };
	}

	async largest(seen: track.Location | undefined): Promise<track.Location | undefined> {
		for (;;) {
			this.sync();
			const serving = this.#serving();
			const view = serving && hooks.peekLargest(serving.sub);
			const largest = view?.largest;
			// A replacement copy can start below what an earlier one reported.
			if (
				largest &&
				(!seen || largest.group > seen.group || (largest.group === seen.group && largest.frame > seen.frame))
			)
				return largest;
			// A copy ending is its route failing: the logical track's stream ends only with the route.
			if (this.#shell.closed.peek() !== undefined) return undefined;
			if ((this.route.concluded || this.route.refusal) && (!serving || view?.ended)) return undefined;
			await new Promise<void>((resolve) => {
				const disposes: Dispose[] = [];
				const wake = () => {
					for (const dispose of disposes) dispose();
					resolve();
				};
				disposes.push(this.changed(wake), this.#shell.closed.changed(wake));
				if (serving) disposes.push(hooks.largestChanged(serving.sub, wake));
			});
		}
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
	/** Abandon it, closing a group it returns, unless the caller kept that group. */
	cancel(): void;
}

function startFetch(
	wire: BroadcastWire,
	name: string,
	sequence: number,
	generation: number,
	signal: AbortSignal,
): Fetch {
	// Its own controller, so a fetch a later route supersedes stops without ending the recovery.
	const controller = new AbortController();
	const onAbort = () => controller.abort(signal.reason);
	signal.addEventListener("abort", onAbort, { once: true });
	const fetch: Fetch = {
		generation,
		settled: new Signal(false),
		cancel: () => {
			signal.removeEventListener("abort", onAbort);
			controller.abort();
			if (fetch.outcome && "group" in fetch.outcome) fetch.outcome.group.close();
		},
	};
	wire.fetchGroup(name, sequence, { signal: controller.signal })
		.then(
			(group) => {
				// A copy that already failed continues nothing: that is the route failing again.
				const failed = group.closed.peek();
				if (failed instanceof Error) {
					group.close();
					fetch.outcome = { error: failed };
					return;
				}
				fetch.outcome = { group };
				// Abandoned while it was out: nobody reads it.
				if (controller.signal.aborted) group.close();
			},
			(err: unknown) => {
				fetch.outcome = { error: err instanceof Error ? err : new Error(String(err)) };
			},
		)
		.finally(() => {
			signal.removeEventListener("abort", onAbort);
			fetch.settled.set(true);
		});
	return fetch;
}

/** Wait for `change`, or throw once `signal` aborts, leaving no listener behind either way. */
async function until(change: PromiseLike<unknown>, signal?: AbortSignal): Promise<void> {
	signal?.throwIfAborted();
	if (!signal) {
		await race([change]);
		return;
	}
	let onAbort = () => {};
	const abort = new Promise<void>((resolve) => {
		onAbort = resolve;
		signal.addEventListener("abort", onAbort, { once: true });
	});
	try {
		await race([change, abort]);
	} finally {
		signal.removeEventListener("abort", onAbort);
	}
	signal.throwIfAborted();
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
	/** Other copies that held the group when it was handed out, kept for a failure to continue from. */
	#backups: Copy[];

	constructor(reader: Reader, group: GroupConsumer, copy: Copy, backups: Copy[] = []) {
		this.#reader = reader;
		this.#sequence = group.sequence;
		this.#copy = copy;
		this.#backups = backups;
		copy.lease++;
		for (const backup of backups) backup.lease++;
	}

	/**
	 * Release the copy once the group ends. Called only after the group carries this recovery:
	 * reading `closed` before then would settle it on a failure recovery could still continue.
	 */
	watch(group: GroupConsumer): void {
		void group.closed.then(() => this.#release());
	}

	#release(): void {
		const released = this.#copy ? [this.#copy, ...this.#backups] : this.#backups;
		this.#copy = undefined;
		this.#backups = [];
		let freed = false;
		for (const copy of released) if (--copy.lease === 0 && copy.replaced) freed = true;
		// A parked read lets go of a replaced copy nothing needs any more.
		if (freed) this.#reader.wake();
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
		let fetch: Fetch | undefined;
		let kept: GroupConsumer | undefined;
		try {
			const replacement = await this.#replace(failed, signal, (next) => {
				fetch?.cancel();
				fetch = next;
			});
			kept = replacement.group;
			return replacement;
		} finally {
			// A fetch nothing continues from is abandoned, along with whatever it returns.
			if (fetch?.outcome === undefined || !("group" in fetch.outcome) || fetch.outcome.group !== kept)
				fetch?.cancel();
		}
	}

	async #replace(
		failed: Error | undefined,
		signal: AbortSignal,
		started: (fetch: Fetch) => void,
	): Promise<Replacement> {
		const reader = this.#reader;
		const route = reader.route;
		let fetch: Fetch | undefined;
		for (;;) {
			signal.throwIfAborted();
			// Subscribe the reader to a new route from its newest group, which is this one or
			// later, so that route's subscription may deliver the rest of it.
			reader.sync();
			const current = this.#copy;
			if (!current) throw new Error("group closed");
			// The front refused the route that would continue it: nothing will.
			if (failed && route.refusal) throw route.refusal;
			const generation = route.generation;
			const serving = route.source ? reader.copy(generation) : undefined;
			const waits: Getter<unknown>[] = [route.changed];
			// A failed group continues from any copy still holding it, newest first: its name fixes
			// its content. A stall only moves to the serving copy, so two stalled copies never trade
			// the group back and forth.
			if (failed) {
				const held = reader.holding(this.#sequence, current)[0];
				const cached = held && hooks.cachedGroup(held.sub, this.#sequence);
				if (cached) return this.#replacement(cached, held);
			}
			if (!serving) {
				// Nothing serves: a failed copy waits for a route, unless none will come.
				if (failed && route.concluded) throw failed;
			} else if (generation !== current.generation || failed) {
				// A newer copy's track caches it: continue from there.
				if (!failed && generation !== current.generation) {
					const cached = hooks.cachedGroup(serving.sub, this.#sequence);
					if (cached) return this.#replacement(cached, serving);
				}
				// Otherwise ask for it. The subscription may still deliver it first.
				if (fetch?.generation !== generation) {
					fetch = startFetch(serving.source.wire, route.name, this.#sequence, generation, signal);
					started(fetch);
				}
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
			await until(Signal.race(...waits), signal);
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
			discard: () => group.close(),
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
	/** Each track's properties as first committed, and the source that said so: every later copy must match. */
	#info = new Map<string, { info: track.Info; source: Source }>();
	/** Bumped whenever the source changes, so an answer from a replaced one is dropped. */
	#changed = new Signal(0);
	/** Aborted once the current source is replaced, retiring what was asked of it. */
	#retire = new AbortController();
	#concluded = false;
	/** The tracks each source was checked against the recorded properties for; see #verify. */
	#verified = new WeakMap<Source, Set<string>>();
	/** Whether any source it served came from a session, kept between routes. */
	#fromSession = false;

	constructor() {
		this.consumer = makeFrontConsumer(this);
		void this.consumer.closed.then(() => this.conclude());
	}

	/** Serve every track from `from` from now on, once each copy proves compatible. */
	serve(from: broadcast.Consumer): void {
		if (this.#concluded || this.#source?.from === from) return;
		const source = new Source(from);
		source.retain();
		this.#fromSession ||= source.fromSession;
		const previous = this.#source;
		this.#source = source;
		this.#retired();
		for (const route of this.#routes.values()) this.#splice(route, source);
		previous?.release();
	}

	/** Between routes: readers keep what they have and wait for the next one. */
	park(): void {
		if (this.#concluded || !this.#source) return;
		const previous = this.#source;
		this.#source = undefined;
		this.#retired();
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
		this.#retired();
	}

	// The source changed: wake what waits on it, and retire what was asked of the old one.
	#retired(): void {
		this.#changed.update((n) => n + 1);
		this.#retire.abort();
		this.#retire = new AbortController();
	}

	/**
	 * Whether a broadcast it served came from a session; see {@link BroadcastWire.fromSession}.
	 * Sticky, since the front can move back onto a session's route at any time.
	 */
	get fromSession(): boolean {
		return this.#fromSession;
	}

	// Splice `source`'s copy of a track in, once it claims the same content as the copies before.
	#splice(route: Route, source: Source): void {
		const recorded = this.#info.get(route.name);
		const previous = route.served;
		// The first copy of a track has nothing to match, and the source that said what it is
		// matches itself.
		if ((!recorded && !previous) || recorded?.source === source) {
			route.serve(source);
			return;
		}
		// The replaced copy keeps feeding the track while the new one answers. One that has not
		// answered yet is asked too, so a swap racing its first answer is still checked.
		source.retain();
		previous?.retain();
		// Abandoned once another route supersedes this one, releasing both.
		const retired = this.#retire.signal;
		const expected = recorded
			? Promise.resolve(recorded.info)
			: ask(previous as Source, route.name, retired).then(
					(info) => {
						this.#record(route.name, info, previous as Source);
						return info;
					},
					// Its route failing is no answer; a refusal is the track's.
					(err: unknown) => {
						if (routeFailed(err instanceof Error ? err : new Error(String(err)))) return undefined;
						throw err;
					},
				);
		Promise.all([expected, ask(source, route.name, retired)])
			.then(
				([expected, info]) => {
					if (this.#source !== source) return;
					// The copy it replaces never answered, so this one says what the track is.
					if (!expected) this.#record(route.name, info, source);
					// A copy claiming the same content with different metadata is refused rather
					// than reinterpreted.
					if (!expected || compatible(expected, info)) route.serve(source);
					else route.refuse(new Incompatible(route.name));
				},
				(err: unknown) => {
					if (this.#source !== source) return;
					const error = err instanceof Error ? err : new Error(String(err));
					// A route on its way out is no verdict: the next one is asked.
					if (!routeFailed(error)) route.refuse(error);
				},
			)
			.finally(() => {
				source.release();
				previous?.release();
			});
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
		// A track whose properties another source already gave is checked like any replacement.
		if (this.#concluded) created.conclude();
		else if (this.#source) this.#splice(created, this.#source);
		return created;
	}

	// Check `source` claims the same properties for `name` as recorded, once per source: a fetch
	// never reads a copy a subscription would refuse.
	async #verify(name: string, source: Source): Promise<void> {
		const recorded = this.#info.get(name);
		if (recorded?.source === source) return;
		let verified = this.#verified.get(source);
		if (verified?.has(name)) return;
		const retired = this.#retire.signal;
		await this.#askServed(name, source, retired);
		const info = await ask(source, name, retired);
		// The first answer says what the track is; later sources have to match it.
		const expected = this.#info.get(name);
		if (!expected) this.#record(name, info, source);
		else if (!compatible(expected.info, info)) throw new Incompatible(name);
		verified ??= new Set();
		verified.add(name);
		this.#verified.set(source, verified);
	}

	// A copy a reader still reads says what the track is before `source` does: one logical track
	// has one set of properties. Its route failing is no answer; a refusal is the track's.
	async #askServed(name: string, source: Source, retired: AbortSignal): Promise<void> {
		const served = this.#routes.get(name)?.served;
		if (this.#info.has(name) || !served || served === source) return;
		let info: track.Info;
		try {
			info = await ask(served, name, retired);
		} catch (err) {
			if (routeFailed(err instanceof Error ? err : new Error(String(err)))) return;
			throw err;
		}
		this.#record(name, info, served);
	}

	#record(name: string, info: track.Info, source: Source): void {
		if (!this.#info.has(name)) this.#info.set(name, { info, source });
	}

	subscribe(name: string, options?: track.Subscription): track.Subscriber {
		if (this.consumer.closed.peek() !== undefined) throw new Error("broadcast is closed");
		const route = this.#route(name);
		return hooks.resumeSubscriber(
			name,
			options,
			(shell) => new Reader(route, shell, (info, source) => this.#record(name, info, source)),
		);
	}

	// Wait for the source to change, or `signal` to abort.
	#next(signal?: AbortSignal): Promise<void> {
		return until(Signal.race(this.#changed), signal);
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
			const retired = this.#retire.signal;
			let info: track.Info;
			try {
				await this.#askServed(name, source, retired);
				info = await ask(source, name, retired, hold);
			} catch (err) {
				// Another route took over before this one answered: ask it instead.
				if (retired.aborted && !hold?.aborted && !this.#concluded) continue;
				const error = err instanceof Error ? err : new Error(String(err));
				// The route failed: ask the next one, unless none will come.
				if (!routeFailed(error) || this.#concluded || hold?.aborted) throw error;
				if (this.#source === source) await this.#next(hold);
				continue;
			} finally {
				source.release();
			}
			const recorded = this.#info.get(name);
			// One logical track has one set of properties, whichever route answers.
			if (recorded && !compatible(recorded.info, info)) throw new Incompatible(name);
			this.#record(name, info, source);
			return recorded?.info ?? info;
		}
	}

	largest(name: string): track.Location | undefined {
		return this.#source?.wire.largest(name);
	}

	async fetchGroup(name: string, sequence: number, options?: track.FetchGroupOptions): Promise<GroupConsumer> {
		const fetched = await this.#fetch(name, sequence, options);
		let current = fetched.source;
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
				// The fetch's hold on its route passes to the group on adoption, or goes with the discard.
				return {
					group: next.group,
					// The route applies the fetch's own budget.
					expiry: { expired: () => false, changed: [] },
					adopt: () => {
						current.release();
						current = next.source;
					},
					discard: () => {
						next.group.close();
						next.source.release();
					},
				};
			},
		});
		// Only once the group carries its recovery: reading `closed` before then would settle it
		// on a failure recovery could still continue, releasing the route while it is read.
		void fetched.group.closed.then(() => current.release());
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
			// Abandoned once another route takes over, and asked of that one instead.
			const retired = this.#retire.signal;
			const signal = options?.signal ? AbortSignal.any([options.signal, retired]) : retired;
			try {
				await until(this.#verify(name, source), signal);
				const group = await source.wire.fetchGroup(name, sequence, { ...options, signal });
				// A copy that already failed continues nothing: that is the route failing again.
				const failed = group.closed.peek();
				if (failed instanceof Error) {
					group.close();
					throw failed;
				}
				return { group, source };
			} catch (err) {
				source.release();
				if (retired.aborted && !options?.signal?.aborted && !this.#concluded) continue;
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
