import * as Catalog from "@moq/hang/catalog";
import * as Json from "@moq/json";
import * as Msf from "@moq/msf";
import type * as Moq from "@moq/net";
import { Error as NetError, Path, StreamCode, Track } from "@moq/net";
import {
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
	epoch?: Catalog.ReferenceEpoch;
};

// Whether a rendition's reference names something this consumer can play.
type Usable = (rel: Path.Relative | undefined, epoch: Moq.Epoch.Valid | undefined) => boolean;

// Either the catalog's own broadcast, or a sibling to consume by path.
type RelativeTarget = { local: true } | { local: false; path: Moq.Path.Valid };

function filterRenditions<T extends ReferencedRendition>(
	renditions: Record<string, T>,
	usable: Usable,
): Record<string, T> {
	return Object.fromEntries(
		Object.entries(renditions).filter(([, config]) => usable(config.broadcast, config.epoch)),
	);
}

// Whether `active` is proven to be another instance than `epoch` names. An epochless route cannot
// prove its instance, so only a known mismatch counts.
function provesOther(active: Moq.Broadcast.Consumer | undefined, epoch: Moq.Epoch.Valid | undefined): boolean {
	return epoch !== undefined && active?.epoch !== undefined && active.epoch !== epoch;
}

// Whether an announced route covers `path`, until closed. Hidden routes count: a service claim
// under a `.`-named prefix is kept out of listings, but it still covers the renditions it would
// produce, and this is never shown to anyone.
function watchCovered(origin: Moq.Origin.Table, path: Moq.Path.Valid): { covered: Getter<boolean>; close: () => void } {
	const announced = origin.announced(Path.Pattern.literal(path), { hidden: true, covering: true });
	const covered = new Signal(false);
	const active = new Set<Moq.Path.Valid>();
	void (async () => {
		try {
			for (;;) {
				const entry = await announced.next();
				if (!entry) break;
				if (entry.kind === "end") active.delete(entry.prefix);
				else active.add(entry.prefix);
				covered.set([...active].some((prefix) => Path.hasPrefix(prefix, path)));
			}
		} catch {
			// The origin closed under the watch; nothing covers the path any more.
		}
		covered.set(false);
	})();
	return { covered, close: () => announced.close() };
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
	// The origin to consume from. Independent of any connection: whichever sessions feed
	// the origin resolve the broadcast, and the handle spans their reconnects.
	origin: Getter<Moq.Origin.Table | undefined>;

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
};

/** Signals the component derives. */
export type BroadcastOutput = {
	// "error" means the origin refused the broadcast; `error` says why.
	// "unsupported" means its catalog requires a newer player.
	status: Signal<"offline" | "loading" | "live" | "error" | "unsupported">;
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

/** A catalog source that can wait for announcement before subscribing. */
export class Broadcast {
	/** The wired inputs, read-only. */
	readonly in: Readonlys<BroadcastInput>;

	readonly #out: BroadcastOutput = {
		status: new Signal<Status>("offline"),
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
		{ origin: Moq.Origin.Table; covered: Getter<boolean>; close: () => void; users: number }
	>();

	// Requests are shared by callers; each owns its restart watch in the origin.
	readonly #references = new Map<
		Moq.Path.Valid,
		{ origin: Moq.Origin.Table; follow: boolean; request: Moq.Origin.Requesting; users: number }
	>();

	// The catalog as published, before renditions this consumer cannot use are dropped. Kept
	// separate so the effective catalog can be re-derived when a reference becomes reachable.
	//
	// Writes notify unconditionally. A manual catalog can be mutated in place, and the rerun that
	// delivers it lands inside the same flush, where a value-compare coalesces the write away and
	// strands the filtered copy on the previous contents.
	readonly #raw = new Signal<Catalog.Root | undefined>(undefined);

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
		};

		this.#signals.run(this.#runBroadcast.bind(this));
		this.#signals.run(this.#runCatalog.bind(this));
		this.#signals.run(this.#runFiltered.bind(this));
	}

	// Publish the catalog minus every rendition this consumer cannot use: one naming a
	// broadcast that isn't announced to us, or one whose broadcast proves another epoch than it
	// names. Selecting one of those would render nothing, since `relativeBroadcast` resolves it to
	// no broadcast at all. Reruns as announcements arrive, so a rendition appears once its
	// broadcast does. Only a rendition naming an epoch resolves its broadcast here, sharing the
	// request playback uses.
	#runFiltered(effect: Effect): void {
		const raw = effect.get(this.#raw);
		const usable: Usable = (rel, epoch) =>
			this.#relativeTarget(effect, rel) !== undefined &&
			(epoch === undefined || !provesOther(this.#relativeActive(effect, rel), epoch));
		effect.set(this.#out.catalog, raw ? filterCatalog(raw, usable) : undefined);
	}

	// Whether `path` is covered by an announced route, for `relativeBroadcast`'s
	// cross-broadcast refs. Announcements are prefix routes, so a route at "room/" covers
	// "room/alice/cam.hang" without naming it. That is how a rendition produced only on demand
	// gets selected: its service claims a covering prefix, and nothing announces the exact path
	// until this subscribes. Watches exactly `path`, shared by every caller asking about it.
	// The blind cases (announcement gate off, no discovery) never reach here; see `#relativeTarget`.
	#isPathAnnounced(effect: Effect, origin: Moq.Origin.Table, path: Moq.Path.Valid): boolean {
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
		return effect.get(held.covered);
	}

	// Share one origin request across callers reading the same referenced broadcast.
	#requestBroadcast(
		effect: Effect,
		origin: Moq.Origin.Table,
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

		const origin = effect.get(this.in.origin);
		if (!origin) return;

		const name = effect.get(this.in.name);
		const follow = effect.get(this.in.follow);

		let request: Moq.Origin.Requesting;
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
		// clears the error on a fresh request, and this run's cleanup drops back to "offline".
		if (effect.get(this.#out.error)) {
			effect.set(this.#out.status, "error", "offline");
			return;
		}
		effect.cleanup(() => this.#out.status.set("offline"));

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
			let status: Status = "loading";
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
			this.#out.status.set(status);
			return;
		}

		const broadcast = effect.get(this.out.active);
		// A withdrawn handle can close before its removal propagates through the active signal.
		if (!broadcast || effect.get(broadcast.closed) !== undefined) return;

		this.#out.status.set("loading");

		const trackName = format === "hang" ? Catalog.TRACK : "catalog";
		const track = broadcast.track(trackName).subscribe({
			...Track.Subscription.default(),
			priority: Catalog.PRIORITY.catalog,
		});
		effect.cleanup(() => track.close());

		// The hang catalog is reconstructed from snapshots (and future deltas) via @moq/json, which reads
		// it compressed or not; MSF stays on its own one-blob-per-group fetch.
		let fetchNext: () => Promise<Catalog.Root | undefined>;
		if (format === "hang") {
			const consumer = new Json.Snapshot.Consumer<Catalog.Root>({ track, schema: Catalog.RootSchema });
			fetchNext = async () => (await consumer.latest())?.value;
		} else {
			const ordered = track.ordered();
			fetchNext = async () => {
				const update = await Msf.fetch(ordered);
				return update ? toHang(update, name) : undefined;
			};
		}

		// The snapshot outlives a clean end of the catalog track, so it clears with this run.
		effect.cleanup(() => this.#raw.set(undefined));

		effect.spawn(async () => {
			let status: Status = "offline";
			let received = false;
			let finished = false;
			try {
				for (;;) {
					const update = await effect.race(fetchNext());
					// A finished catalog only means no more edits. A finished broadcast keeps serving
					// its tail until it closes or is withdrawn, which reruns this, so keep playing it.
					// One that ended empty has nothing to play.
					if (!update) {
						finished = received;
						break;
					}
					received = true;

					console.debug("received catalog", format, this.in.name.peek(), update);

					this.#raw.set(Catalog.checkResolvable(Catalog.checkRenditions(update), name), true);
					this.#out.status.set("live");
				}
			} catch (err) {
				if (err instanceof Catalog.UnsupportedVersion) status = "unsupported";
				if (err instanceof NetError.Stream)
					console.debug("catalog subscription ended", this.in.name.peek(), err);
				else console.error("error fetching catalog", this.in.name.peek(), err);
			} finally {
				if (!finished) {
					this.#raw.set(undefined);
					this.#out.status.set(status);
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
			if (!this.#isPathAnnounced(effect, origin, resolved)) return undefined;
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
	 * `epoch` is the rendition's `epoch` field, the publisher instance the reference names.
	 * Returns `undefined` while the followed broadcast is proven to be another instance, rather
	 * than reading it. A route without an epoch cannot prove one, so there it is best effort.
	 *
	 * @internal
	 */
	relativeBroadcast(
		effect: Effect,
		rel: Path.Relative | undefined,
		epoch?: Moq.Epoch.Valid,
	): Moq.Broadcast.Consumer | undefined {
		const active = this.#relativeActive(effect, rel);
		if (!active || !provesOther(active, epoch)) return active;
		console.warn("ignoring rendition: its broadcast serves another epoch", active.path, active.epoch, epoch);
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

	/** Stop and release everything this holds. */
	close() {
		this.#signals.close();
		for (const entry of this.#references.values()) entry.request.close();
		this.#references.clear();
		for (const entry of this.#watches.values()) entry.close();
		this.#watches.clear();
	}
}
