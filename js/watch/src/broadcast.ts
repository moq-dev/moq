import * as Catalog from "@moq/hang/catalog";
import * as Json from "@moq/json";
import * as Msf from "@moq/msf";
import type * as Moq from "@moq/net";
import { Instance, Error as NetError, Path, StreamCode, Track } from "@moq/net";
import {
	Derived,
	Effect,
	type Getter,
	type GetterType,
	getter,
	type Inputs,
	type Readonlys,
	readonlys,
	Signal,
} from "@moq/signals";

import { toHang } from "./msf";

type ReferencedRendition = {
	broadcast?: Path.Relative;
	instance?: Catalog.ReferenceInstance;
};

// Whether a rendition's reference names something this consumer can play.
type Usable = (rel: Path.Relative | undefined, instance: Moq.Instance.Valid | undefined) => boolean;

// Either the catalog's own broadcast, or a sibling to consume by path.
type RelativeTarget = { local: true } | { local: false; path: Moq.Path.Valid };

function filterRenditions<T extends ReferencedRendition>(
	renditions: Record<string, T>,
	usable: Usable,
): Record<string, T> {
	return Object.fromEntries(
		Object.entries(renditions).filter(([, config]) => usable(config.broadcast, config.instance)),
	);
}

// Whether `active` is proven to be another stream than `instance` names. A route over a wire
// that carries no instance has one its session minted, which proves nothing, so only a known
// mismatch counts.
function provesOther(active: Moq.Broadcast.Consumer | undefined, instance: Moq.Instance.Valid | undefined): boolean {
	if (instance === undefined || active?.instance === undefined) return false;
	return !Instance.isMinted(active.instance) && active.instance !== instance;
}

// Whether an announced route covers `path`, and whether the origin has heard every attached
// session's initial announcements for it, until closed. Hidden routes count: a service claim
// under a `.`-named prefix is kept out of listings, but it still covers the renditions it would
// produce, and this is never shown to anyone.
function watchCovered(
	origin: Moq.Origin.Reader,
	path: Moq.Path.Valid,
): { covered: Getter<boolean>; synced: Getter<boolean>; close: () => void } {
	const announced = origin.announced(Path.Pattern.literal(path), { hidden: true, covering: true });
	const covered = new Signal(false);
	const active = new Set<Moq.Path.Valid>();
	void (async () => {
		try {
			for (;;) {
				const entry = await announced.next();
				if (!entry) break;
				if (entry.kind === "absent" || entry.kind === "offline") active.delete(entry.prefix);
				else if (entry.kind === "present") active.add(entry.prefix);
				covered.set([...active].some((prefix) => Path.hasPrefix(prefix, path)));
			}
		} catch {
			// The origin closed under the watch; nothing covers the path any more.
		}
		covered.set(false);
	})();
	return { covered, synced: announced.synced, close: () => announced.close() };
}

// Every section carrying a `broadcast` reference must be listed here, same as `Catalog.checkResolvable`;
// one left out silently exempts its tracks from the reachability filter.
function filterCatalog(catalog: Catalog.Root, usable: Usable): Catalog.Root {
	return {
		...catalog,
		video: catalog.video
			? { ...catalog.video, renditions: filterRenditions(catalog.video.renditions, usable) }
			: undefined,
		audio: catalog.audio
			? { ...catalog.audio, renditions: filterRenditions(catalog.audio.renditions, usable) }
			: undefined,
		text: catalog.text
			? { ...catalog.text, renditions: filterRenditions(catalog.text.renditions, usable) }
			: undefined,
		json: catalog.json ? { ...catalog.json, tracks: filterRenditions(catalog.json.tracks, usable) } : undefined,
		binary: catalog.binary
			? { ...catalog.binary, tracks: filterRenditions(catalog.binary.tracks, usable) }
			: undefined,
	};
}

/**
 * Watch supports the on-the-wire catalog formats from `@moq/hang`, plus a "manual" mode where the
 * user supplies the catalog directly without fetching.
 */
export const CATALOG_FORMATS = [...Catalog.FORMATS, "manual"] as const;
/** A catalog format the watcher can read; see {@link CATALOG_FORMATS}. */
export type CatalogFormat = (typeof CATALOG_FORMATS)[number];

/**
 * Signals the component reads. Whoever owns the backing Signal (the caller, or
 * another component whose output is wired in) does the writing.
 */
export type BroadcastInput = {
	// The read-side origin to borrow. Independent of any connection: whichever sessions feed
	// the origin resolve the broadcast, and the handle spans their reconnects.
	// Unset reads "connecting", like an origin with no session attached, never "offline".
	origin: Getter<Moq.Origin.Reader | undefined>;

	// Whether to start downloading the broadcast. Defaults to true.
	enabled: Getter<boolean>;

	// The broadcast name.
	name: Getter<Moq.Path.Valid>;

	// Whether to follow the broadcast's announcements (`Origin.follow`), waiting for one before
	// subscribing and moving to each restart. Defaults to true; pass false to request it once
	// (`Origin.request`), subscribing immediately without waiting for an announcement.
	follow: Getter<boolean>;

	// Which catalog format to use. When `undefined` (the default), the format is
	// auto-detected from the broadcast name extension (`.hang`, `.msf`), falling
	// back to `"hang"` if the name has no recognized extension. Set to a
	// specific value to override auto-detection.
	catalogFormat: Getter<CatalogFormat | undefined>;

	// The manual-mode catalog source. Used directly when catalogFormat is "manual";
	// ignored otherwise. Read `output.catalog` for the effective catalog in any mode.
	catalog: Getter<Catalog.Root | undefined>;

	// Whether playback still has media of the broadcast left to play once its publisher finished it.
	// A finished broadcast reads "live" until this is false. Defaults to false.
	tail: Getter<boolean>;
};

/** Signals the component derives. */
export type BroadcastOutput = {
	// "connecting" means no session can answer yet: none is attached, as while the connection
	// dials, redials, or was refused, or one is still delivering its initial announcements.
	// "offline" means one could answer and the broadcast is not there, or its publisher finished it
	// and `tail` says nothing is left to play.
	// "error" means the origin refused the broadcast; `error` says why.
	// "unsupported" means its catalog requires a newer player.
	status: Signal<"connecting" | "offline" | "loading" | "live" | "error" | "unsupported">;
	active: Signal<Moq.Broadcast.Consumer | undefined>;

	// Why the origin refused the broadcast, while `status` is "error". A refusal is final:
	// only a fresh request (a new `name`, `origin`, or `follow`, or re-enabling) clears it and asks again.
	error: Signal<Error | undefined>;

	// The effective catalog: the fetched one, or a copy of input.catalog in manual mode, minus
	// any rendition this consumer can't use (see `#runFiltered`). A rendition referencing another
	// broadcast appears once that broadcast is announced, so this can change without a new catalog.
	catalog: Signal<Catalog.Root | undefined>;
};

type Status = GetterType<BroadcastOutput["status"]>;

// What the catalog run reports, where "absent" means no handle to read; `#runStatus` turns that
// into "connecting" or "offline".
type Progress = Exclude<Status, "connecting" | "offline"> | "absent";

/** A catalog source that can wait for announcement before subscribing. */
export class Broadcast {
	/** The wired inputs, read-only. */
	readonly in: Readonlys<BroadcastInput>;

	readonly #out: BroadcastOutput = {
		status: new Signal<Status>("connecting"),
		active: new Signal<Moq.Broadcast.Consumer | undefined>(undefined),
		error: new Signal<Error | undefined>(undefined),
		catalog: new Signal<Catalog.Root | undefined>(undefined),
	};
	/** The derived outputs, read-only. */
	readonly out = readonlys(this.#out);

	// One announcement watch per referenced broadcast, shared by callers, so the origin only asks
	// its relay about the broadcasts a catalog actually references.
	readonly #watches = new Map<
		Moq.Path.Valid,
		{
			origin: Moq.Origin.Reader;
			covered: Getter<boolean>;
			synced: Getter<boolean>;
			close: () => void;
			users: number;
		}
	>();

	// `#runCatalog`'s view of the broadcast, before "absent" is told apart.
	readonly #progress = new Signal<Progress>("absent");
	// Whether the publisher finished the catalog, which it does only when it ends the broadcast.
	readonly #finished = new Signal(false);

	// Requests are shared by callers; each owns its restart watch in the origin.
	readonly #references = new Map<
		Moq.Path.Valid,
		{
			origin: Moq.Origin.Reader;
			follow: boolean;
			request: Moq.Origin.Requesting | Moq.Origin.Following;
			users: number;
		}
	>();

	// The catalog as published, before renditions this consumer cannot use are dropped. Kept
	// separate so the effective catalog can be re-derived when a reference becomes reachable.
	//
	// Writes notify unconditionally. A manual catalog can be mutated in place, and the rerun that
	// delivers it lands inside the same flush, where a value-compare coalesces the write away and
	// strands the filtered copy on the previous contents.
	readonly #raw = new Signal<Catalog.Root | undefined>(undefined);

	// Set only on an include: its broadcast resolves through the parent catalog's references,
	// so the two never resolve one path independently.
	readonly #resolve = new Signal<((effect: Effect) => Moq.Broadcast.Consumer | undefined) | undefined>(undefined);

	#signals: Effect;

	constructor(props?: Inputs<BroadcastInput>) {
		for (const renamed of ["reload", "announced"]) {
			if (props && renamed in props) throw new Error(`Watch.Broadcast: \`${renamed}\` was renamed to \`follow\``);
		}
		this.#signals = new Effect();
		this.in = {
			origin: getter(props?.origin),
			name: getter(props?.name ?? Path.empty()),
			enabled: getter(props?.enabled ?? true),
			follow: getter(props?.follow ?? true),
			catalogFormat: getter<CatalogFormat | undefined>(props?.catalogFormat),
			catalog: getter(props?.catalog),
			tail: getter(props?.tail ?? false),
		};

		this.#signals.run(this.#runBroadcast.bind(this));
		this.#signals.run(this.#runCatalog.bind(this));
		this.#signals.run(this.#runFiltered.bind(this));
		this.#signals.run(this.#runStatus.bind(this));
	}

	// Without a handle, the broadcast is offline only once a session could have announced it.
	// A connection holds the origin's announcements unsynced while it is not established, and
	// a bare origin has no session at all; either way nobody has answered yet.
	#runStatus(effect: Effect): void {
		const progress = effect.get(this.#progress);
		if (progress === "live" && effect.get(this.#finished) && !effect.get(this.in.tail)) {
			this.#out.status.set("offline");
			return;
		}
		if (progress !== "absent") {
			this.#out.status.set(progress);
			return;
		}

		// A disabled broadcast asks nobody, so there is no answer to wait for.
		if (!effect.get(this.in.enabled)) {
			this.#out.status.set("offline");
			return;
		}

		const origin = effect.get(this.in.origin);
		const discovery = origin && effect.get(origin.discovery);
		let connecting = discovery === undefined;
		// A session without discovery never announces, so there is nothing to wait for.
		if (origin && discovery === true) {
			connecting = !effect.get(this.#watchPath(effect, origin, effect.get(this.in.name)).synced);
		}
		this.#out.status.set(connecting ? "connecting" : "offline");
	}

	// Publish the catalog minus every rendition this consumer cannot use: one naming a
	// broadcast that isn't announced to us, or one whose broadcast proves another instance than it
	// names. Selecting one of those would render nothing, since `relativeBroadcast` resolves it to
	// no broadcast at all. Reruns as announcements arrive, so a rendition appears once its
	// broadcast does. Only a rendition naming an instance resolves its broadcast here, sharing the
	// request playback uses.
	#runFiltered(effect: Effect): void {
		const raw = effect.get(this.#raw);
		const usable: Usable = (rel, instance) =>
			this.#relativeTarget(effect, rel) !== undefined &&
			(instance === undefined || !provesOther(this.#relativeActive(effect, rel), instance));
		effect.set(this.#out.catalog, raw ? filterCatalog(raw, usable) : undefined);
	}

	// Watch the announcements covering exactly `path`, shared by every caller asking about it
	// until the effect reruns. `covered` gates `relativeBroadcast`'s cross-broadcast refs:
	// announcements are prefix routes, so a route at "room/" covers "room/alice/cam.hang" without
	// naming it. That is how a rendition produced only on demand gets selected: its service claims
	// a covering prefix, and nothing announces the exact path until this subscribes. `synced` tells
	// `#runStatus` whether the broadcast's absence is known yet.
	#watchPath(
		effect: Effect,
		origin: Moq.Origin.Reader,
		path: Moq.Path.Valid,
	): { covered: Getter<boolean>; synced: Getter<boolean> } {
		let entry = this.#watches.get(path);
		if (!entry || entry.origin !== origin) {
			entry?.close();
			entry = { origin, ...watchCovered(origin, path), users: 0 };
			this.#watches.set(path, entry);
		}

		const held = entry;
		held.users += 1;
		effect.cleanup(() => {
			held.users -= 1;
			// Deferred, so a caller's rerun reuses the watch rather than closing and reopening it.
			queueMicrotask(() => {
				if (held.users > 0 || this.#watches.get(path) !== held) return;
				this.#watches.delete(path);
				held.close();
			});
		});
		return held;
	}

	// Share one origin request across callers reading the same referenced broadcast.
	#requestBroadcast(
		effect: Effect,
		origin: Moq.Origin.Reader,
		path: Moq.Path.Valid,
	): Moq.Broadcast.Consumer | undefined {
		const follow = effect.get(this.in.follow);
		let entry = this.#references.get(path);
		if (!entry || entry.origin !== origin || entry.follow !== follow) {
			entry?.request.close();
			entry = { origin, follow, request: follow ? origin.follow(path) : origin.request(path), users: 0 };
			this.#references.set(path, entry);
		}

		const held = entry;
		held.users += 1;
		effect.cleanup(() => {
			held.users -= 1;
			// Deferred, so a caller's rerun reuses the request rather than closing and reopening it.
			queueMicrotask(() => {
				if (held.users > 0 || this.#references.get(path) !== held) return;
				this.#references.delete(path);
				held.request.close();
			});
		});
		return effect.get(held.request.active);
	}

	// The origin owns restart following; mirror its current handle and terminal refusal.
	#runBroadcast(effect: Effect): void {
		const enabled = effect.get(this.in.enabled);
		if (!enabled) return;

		const resolve = effect.get(this.#resolve);
		if (resolve) {
			// Like the origin request below, a path outside the origin's scope throws.
			let active: Moq.Broadcast.Consumer | undefined;
			try {
				active = resolve(effect);
			} catch (err) {
				effect.set(this.#out.error, err instanceof Error ? err : new Error(String(err)), undefined);
				return;
			}
			effect.set(this.#out.active, active, undefined);
			return;
		}

		const origin = effect.get(this.in.origin);
		if (!origin) return;

		const name = effect.get(this.in.name);
		const follow = effect.get(this.in.follow);

		let request: Moq.Origin.Requesting | Moq.Origin.Following;
		try {
			request = follow ? origin.follow(name) : origin.request(name);
		} catch (err) {
			effect.set(this.#out.error, err instanceof Error ? err : new Error(String(err)), undefined);
			return;
		}
		effect.cleanup(() => request.close());
		let resolved = false;
		effect.run((nested) => {
			const active = nested.get(request.active);
			if (active) resolved = true;
			nested.set(this.#out.active, active, undefined);
		});
		effect.run((nested) => {
			const closed = nested.get(request.closed);
			if (!closed) return;
			// A follower outlives its instances, so only a refusal ends it. A plain request
			// ends with its accepted instance, which reads as offline rather than an error.
			const offline =
				!follow && resolved && closed instanceof NetError.Stream && closed.code === StreamCode.Unroutable;
			if (!offline) nested.set(this.#out.error, closed, undefined);
		});
	}

	#runCatalog(effect: Effect): void {
		const enabled = effect.get(this.in.enabled);
		if (!enabled) return;

		// Even a manual catalog is unplayable once the origin refuses its media. `#runBroadcast`
		// clears the error on a fresh request, and this run's cleanup drops back to "absent".
		if (effect.get(this.#out.error)) {
			effect.set(this.#progress, "error", "absent");
			return;
		}
		effect.cleanup(() => this.#progress.set("absent"));

		const catalogFormat = effect.get(this.in.catalogFormat);
		const name = effect.get(this.in.name);
		// Explicit override beats name-derived auto-detection. When neither is
		// set we fall back to the default, keeping legacy names that have no
		// extension working.
		const format: CatalogFormat = catalogFormat ?? Catalog.detectFormat(name) ?? Catalog.DEFAULT_FORMAT;

		if (format === "manual") {
			// Parse the caller-supplied catalog into the effective output with the same schema and
			// checks as a fetched one, minus the throw: this runs in the effect body, where an
			// exception would surface as an unhandled error.
			const catalog = effect.get(this.in.catalog);
			let accepted: Catalog.Root | undefined;
			let status: Progress = "loading";
			try {
				accepted =
					catalog &&
					Catalog.checkResolvable(Catalog.checkRenditions(Catalog.RootSchema.parse(catalog)), name);
				if (accepted) status = "live";
			} catch (err) {
				if (err instanceof Catalog.UnsupportedVersion) status = "unsupported";
				console.error("rejecting catalog", name, err);
			}
			this.#raw.set(accepted, true);
			effect.cleanup(() => this.#raw.set(undefined, true));
			this.#progress.set(status);
			return;
		}

		const broadcast = effect.get(this.out.active);
		// A withdrawn handle can close before its removal propagates through the active signal.
		if (!broadcast || effect.get(broadcast.closed) !== undefined) return;

		this.#progress.set("loading");

		// The hang catalog is reconstructed from snapshots (and future deltas) via @moq/json, which reads
		// it compressed or not and subscribes itself; MSF stays on its own one-blob-per-group fetch.
		let fetchNext: () => Promise<Catalog.Root | undefined>;
		if (format === "hang") {
			const consumer = new Json.Snapshot.Consumer<Catalog.Root>({
				track: broadcast.track(Catalog.TRACK),
				schema: Catalog.RootSchema,
				priority: Catalog.PRIORITY.catalog,
			});
			effect.cleanup(() => consumer.close());
			fetchNext = async () => (await consumer.latest())?.value;
		} else {
			const track = broadcast.track("catalog").subscribe({
				...Track.Subscription.default(),
				priority: Catalog.PRIORITY.catalog,
			});
			effect.cleanup(() => track.close());
			const ordered = track.ordered();
			fetchNext = async () => {
				const update = await Msf.fetch(ordered);
				return update ? toHang(update, name) : undefined;
			};
		}

		// The snapshot outlives a clean end of the catalog track, so it clears with this run.
		effect.cleanup(() => {
			this.#raw.set(undefined);
			this.#finished.set(false);
		});

		effect.spawn(async () => {
			let status: Progress = "absent";
			let received = false;
			let finished = false;
			try {
				for (;;) {
					const update = await effect.race(fetchNext());
					// A finished catalog means the publisher ended the broadcast. It keeps serving its tail
					// until it closes or is withdrawn, which reruns this, so keep playing it, and read
					// offline once `tail` has nothing left. One that ended empty has nothing to play.
					if (!update) {
						finished = received;
						// A rerun cancelling the read ends it the same way, which says nothing of the publisher.
						if (!effect.abort.aborted) this.#finished.set(finished);
						break;
					}
					received = true;

					console.debug("received catalog", format, this.in.name.peek(), update);

					this.#raw.set(Catalog.checkResolvable(Catalog.checkRenditions(update), name), true);
					this.#progress.set("live");
				}
			} catch (err) {
				if (err instanceof Catalog.UnsupportedVersion) status = "unsupported";
				if (err instanceof NetError.Stream)
					console.debug("catalog subscription ended", this.in.name.peek(), err);
				else console.error("error fetching catalog", this.in.name.peek(), err);
			} finally {
				if (!finished) {
					this.#raw.set(undefined);
					this.#progress.set(status);
				}
			}
		});
	}

	// Where a rendition's `broadcast` reference points once resolved and gated on the announcement
	// stream, or `undefined` when it names nothing consumable right now. Playback and rendition
	// selection both go through this so they cannot disagree about what is reachable.
	#relativeTarget(effect: Effect, rel: Path.Relative | undefined): RelativeTarget | undefined {
		if (!rel) return { local: true };

		const base = effect.get(this.in.name);
		const resolved = Path.tryResolve(base, rel);
		if (resolved === undefined) {
			console.warn("ignoring rendition: broadcast reference escapes the root", base, rel);
			return undefined;
		}

		// A reference that walks back to the catalog's own broadcast is served by the
		// catalog broadcast itself, avoiding a duplicate subscription on the same path.
		if (resolved === base) return { local: true };

		const origin = effect.get(this.in.origin);
		// Nothing to ask yet. `relativeBroadcast` still bails on the missing origin, and this
		// keeps a reconnect from briefly hiding every cross-broadcast rendition from selection.
		if (!origin) return { local: false, path: resolved };

		// Without an announcement gate (disabled, or no session supports discovery),
		// resolve blind rather than waiting for an announcement that never comes. With the
		// gate, only report the path usable once it is announced: the request then resolves
		// from the table, never blind.
		if (effect.get(this.in.follow) && effect.get(origin.discovery) !== false) {
			if (!effect.get(this.#watchPath(effect, origin, resolved).covered)) return undefined;
		}

		return { local: false, path: resolved };
	}

	/**
	 * Resolve the `Moq.Broadcast.Consumer` that publishes a given track.
	 *
	 * If `rel` is set (a rendition's catalog `broadcast` field), treat it as a path
	 * relative to this broadcast's name and consume the resolved broadcast on the same
	 * connection. Otherwise return the catalog's own active broadcast.
	 *
	 * Returns `undefined` for a reference that walks above the root: hang requires such a
	 * rendition to be ignored, since clamping at the root would point it at an unrelated
	 * broadcast.
	 *
	 * The consumer is scoped to the caller's `effect` (closed on its next run), so a
	 * reference resolves lazily and reacts to `enabled` / connection / announcement
	 * changes exactly like the catalog broadcast.
	 *
	 * `instance` is the rendition's `instance` field, the stream the reference names. Returns
	 * `undefined` while the followed broadcast is proven to be another stream, rather than
	 * reading it. A route over a wire that carries no instance cannot prove one, so there it is
	 * best effort.
	 *
	 * @internal
	 */
	relativeBroadcast(
		effect: Effect,
		rel: Path.Relative | undefined,
		instance?: Moq.Instance.Valid,
	): Moq.Broadcast.Consumer | undefined {
		const active = this.#relativeActive(effect, rel);
		if (!active || !provesOther(active, instance)) return active;
		console.warn(
			"ignoring rendition: its broadcast serves another instance",
			active.path,
			active.instance,
			instance,
		);
		return undefined;
	}

	#relativeActive(effect: Effect, rel: Path.Relative | undefined): Moq.Broadcast.Consumer | undefined {
		const target = this.#relativeTarget(effect, rel);
		if (!target) return undefined;
		if (target.local) return effect.get(this.out.active);

		if (!effect.get(this.in.enabled)) return undefined;

		const origin = effect.get(this.in.origin);
		if (!origin) return undefined;

		return this.#requestBroadcast(effect, origin, target.path);
	}

	/**
	 * Select a whole include, opening its catalog only while the caller owns this effect.
	 *
	 * Listing an include in the catalog opens nothing; this is the demand. The child tracks
	 * the catalog entry and resolves through this broadcast's `relativeBroadcast`, sharing
	 * its request: while following announcements it follows the include's restarts, and
	 * otherwise it requests the first instance once. With an `instance` it is offline while its
	 * broadcast resolves another stream. An absent entry leaves the child disabled, and an
	 * unavailable include reads as offline. Its rendition IDs stay local to the child, so
	 * they never shadow this catalog's IDs.
	 *
	 * ```ts
	 * effects.run((effect) => {
	 *   const child = broadcast.include(effect, "alice");
	 *   const source = new Video.Source({ broadcast: child, supported: Video.Decoder.supported });
	 *   effect.cleanup(() => source.close());
	 *   // Feed source into a decoder as with any broadcast.
	 * });
	 * ```
	 */
	include(effect: Effect, id: string): Broadcast {
		// Each computed notifies only on a real change, so an update that leaves the child's path
		// alone (such as a new `instance`) never restarts its catalog.
		const entry = effect.computed((run) => {
			const include = run.get(this.out.catalog)?.include;
			return include && Object.hasOwn(include, id) ? include[id] : undefined;
		});
		const name = effect.computed((run) => {
			// An empty reference is present: it names the catalog's own broadcast.
			const selected = run.get(entry);
			return selected && Path.tryResolve(run.get(this.in.name), selected.broadcast);
		});
		const enabled = effect.computed((run) => run.get(entry) !== undefined);
		const child = new Broadcast({
			origin: this.in.origin,
			name: new Derived([name], (name) => name ?? Path.empty()),
			enabled: new Derived([enabled], (enabled) => enabled === true),
			follow: this.in.follow,
		});
		child.#resolve.set((run) => {
			const selected = run.get(entry);
			return selected && this.relativeBroadcast(run, selected.broadcast, selected.instance);
		});
		effect.cleanup(() => child.close());
		return child;
	}

	/** Stop and release everything this holds. */
	close() {
		this.#signals.close();
		for (const entry of this.#references.values()) entry.request.close();
		this.#references.clear();
		for (const entry of this.#watches.values()) entry.close();
		this.#watches.clear();
	}
}
