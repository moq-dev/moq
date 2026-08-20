import * as Catalog from "@moq/hang/catalog";
import * as Json from "@moq/json";
import * as Msf from "@moq/msf";
import type * as Moq from "@moq/net";
import { Announce, Path } from "@moq/net";
import { Effect, type Getter, getter, type Inputs, type Readonlys, readonlys, Signal } from "@moq/signals";

import { toHang } from "./msf";

/**
 * The name of the first rendition whose `broadcast` reference walks above the root, if any.
 *
 * The root is the consumer's authorized subtree, so such a reference names content this
 * consumer cannot reach. It rejects the whole catalog rather than the one rendition: a
 * publisher that emits one has a bug, and quietly serving the rest hides that while the
 * missing rendition resurfaces later as a track that never fills.
 */
function findEscaping(base: Moq.Path.Valid, catalog: Catalog.Root): string | undefined {
	// Every section carrying renditions must be listed here; one left out silently exempts
	// its renditions from the containment check.
	const renditions = [
		...Object.entries(catalog.video?.renditions ?? {}),
		...Object.entries(catalog.audio?.renditions ?? {}),
		...Object.entries(catalog.text?.renditions ?? {}),
	];

	for (const [name, config] of renditions) {
		if (config.broadcast && Path.tryResolve(base, config.broadcast) === undefined) return name;
	}

	return undefined;
}

/** Throw if any rendition's `broadcast` reference escapes the root. */
function assertResolvable(base: Moq.Path.Valid, catalog: Catalog.Root): Catalog.Root {
	const escaping = findEscaping(base, catalog);
	if (escaping !== undefined) {
		throw new Error(`rendition ${JSON.stringify(escaping)}: broadcast reference escapes the root ${base}`);
	}
	return catalog;
}

type ReferencedRendition = {
	broadcast?: string;
};

// Either the catalog's own broadcast, or a sibling to consume by path.
type RelativeTarget = { local: true } | { local: false; path: Moq.Path.Valid };

function filterRenditions<T extends ReferencedRendition>(
	renditions: Record<string, T>,
	usable: (rel: string | undefined) => boolean,
): Record<string, T> {
	return Object.fromEntries(Object.entries(renditions).filter(([, config]) => usable(config.broadcast)));
}

// Every section carrying renditions must be listed here, same as `findEscaping`; one left
// out silently exempts its renditions from the reachability filter.
function filterCatalog(catalog: Catalog.Root, usable: (rel: string | undefined) => boolean): Catalog.Root {
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
	};
}

// Watch supports the on-the-wire catalog formats from @moq/hang, plus "hangz" (the
// DEFLATE-compressed `catalog.json.z` track) and a "manual" mode where the user supplies the
// catalog directly without fetching. "hangz" is opt-in only: it shares the `.hang` broadcast suffix
// and is never auto-detected, so set it explicitly via `catalogFormat`.
export const CATALOG_FORMATS = [...Catalog.FORMATS, "hangz", "manual"] as const;
export type CatalogFormat = (typeof CATALOG_FORMATS)[number];

export function parseCatalogFormat(value: string | null): CatalogFormat | undefined {
	if (value === null) return undefined;
	return CATALOG_FORMATS.find((f) => f === value);
}

type Status = "offline" | "loading" | "live";

// Signals the component reads. Whoever owns the backing Signal (the caller, or
// another component whose output is wired in) does the writing.
export type BroadcastInput = {
	// The origin to consume from. Independent of any connection: whichever sessions feed
	// the origin resolve the broadcast, and the handle spans their reconnects.
	origin: Getter<Moq.Origin.Table | undefined>;

	// Whether to start downloading the broadcast. Defaults to true.
	enabled: Getter<boolean>;

	// The broadcast name.
	name: Getter<Moq.Path.Valid>;

	// Whether to reload the broadcast when it goes offline.
	// Defaults to true; pass false to subscribe immediately without waiting for an announcement.
	reload: Getter<boolean>;

	// Which catalog format to use. When `undefined` (the default), the format is
	// auto-detected from the broadcast name extension (`.hang`, `.msf`), falling
	// back to `"hang"` if the name has no recognized extension. Set to a
	// specific value to override auto-detection. `"hangz"` (the compressed
	// `catalog.json.z` track) is opt-in only and never auto-detected.
	catalogFormat: Getter<CatalogFormat | undefined>;

	// The manual-mode catalog source. Used directly when catalogFormat is "manual";
	// ignored otherwise. Read `output.catalog` for the effective catalog in any mode.
	catalog: Getter<Catalog.Root | undefined>;
};

type BroadcastOutput = {
	status: Signal<Status>;
	active: Signal<Moq.Broadcast.Consumer | undefined>;

	// The effective catalog: the fetched one, or a copy of input.catalog in manual mode, minus
	// any rendition this consumer can't use (see `#runFiltered`). A rendition referencing another
	// broadcast appears once that broadcast is announced, so this can change without a new catalog.
	catalog: Signal<Catalog.Root | undefined>;
};

// A catalog source that (optionally) reloads automatically when live/offline.
export class Broadcast {
	readonly in: Readonlys<BroadcastInput>;

	readonly #out: BroadcastOutput = {
		status: new Signal<Status>("offline"),
		active: new Signal<Moq.Broadcast.Consumer | undefined>(undefined),
		catalog: new Signal<Catalog.Root | undefined>(undefined),
	};
	readonly out = readonlys(this.#out);

	// The set of announced paths on the connection, for cross-broadcast (`broadcast: ../`) references
	// so `relativeBroadcast` can gate on whether a sibling is announced. `undefined` until the stream
	// is open. Opened lazily; the main broadcast doesn't use it (`#runBroadcast` drives off its own
	// name-scoped stream).
	readonly #announced = new Signal<Set<Moq.Path.Valid> | undefined>(undefined);

	// Set true the first time a relative reference needs the announcement gate, so a broadcast with
	// no cross-broadcast renditions never opens the (broad) connection-scoped announcement stream.
	readonly #wantAnnounced = new Signal(false);

	// The catalog as published, before renditions this consumer cannot use are dropped. Kept
	// separate so the effective catalog can be re-derived when a reference becomes reachable.
	//
	// Writes notify unconditionally. A manual catalog can be mutated in place, and the rerun that
	// delivers it lands inside the same flush, where a value-compare coalesces the write away and
	// strands the filtered copy on the previous contents.
	readonly #raw = new Signal<Catalog.Root | undefined>(undefined);

	#signals = new Effect();

	constructor(props?: Inputs<BroadcastInput>) {
		this.in = {
			origin: getter(props?.origin),
			name: getter(props?.name ?? Path.empty()),
			enabled: getter(props?.enabled ?? true),
			reload: getter(props?.reload ?? true),
			catalogFormat: getter<CatalogFormat | undefined>(props?.catalogFormat),
			catalog: getter(props?.catalog),
		};

		this.#signals.run(this.#runAnnounced.bind(this));
		this.#signals.run(this.#runBroadcast.bind(this));
		this.#signals.run(this.#runCatalog.bind(this));
		this.#signals.run(this.#runFiltered.bind(this));
	}

	// Maintain the set of announced paths used by `relativeBroadcast`, by draining an origin-scoped
	// announcement stream. Only opened once a relative reference asks for it (see `#wantAnnounced`).
	#runAnnounced(effect: Effect): void {
		this.#announced.set(undefined);

		if (!effect.get(this.#wantAnnounced)) return;
		if (!effect.get(this.in.reload)) return;

		const origin = effect.get(this.in.origin);
		if (!origin) return;

		const announced = origin.announced(Path.empty());
		effect.cleanup(() => announced.close());
		this.#announced.set(new Set());

		effect.spawn(async () => {
			for (;;) {
				const entry = await Promise.race([effect.cancel, announced.next()]);
				if (!entry) break;
				this.#announced.mutate((active) => {
					if (!active) return;
					if (entry.active) active.add(entry.path);
					else active.delete(entry.path);
				});
			}
		});
	}

	// Publish the catalog minus every rendition this consumer cannot use: one naming a
	// broadcast that isn't announced to us. Selecting one of those would render nothing,
	// since `relativeBroadcast` resolves it to no broadcast at all. Reruns as announcements
	// arrive, so a rendition appears once its broadcast does.
	#runFiltered(effect: Effect): void {
		const raw = effect.get(this.#raw);
		const usable = (rel: string | undefined) => this.#relativeTarget(effect, rel) !== undefined;
		effect.set(this.#out.catalog, raw ? filterCatalog(raw, usable) : undefined);
	}

	// Whether `path` is currently announced, for `relativeBroadcast`'s cross-broadcast refs.
	// Opens the announcement stream on first use. The blind cases (reload off, no discovery)
	// never reach here; see `#relativeTarget`.
	#isPathAnnounced(effect: Effect, path: Moq.Path.Valid): boolean {
		this.#wantAnnounced.set(true);

		const active = effect.get(this.#announced);
		if (!active) return false; // stream not open yet: wait rather than subscribe to a maybe-absent path
		return active.has(path);
	}

	// Resolve `path` without waiting for an announcement. The request is table-first, so a
	// routed broadcast (a local publish, or anything announced) resolves synchronously and
	// a blind session answer covers the rest, arriving on a later run.
	#requestBroadcast(
		effect: Effect,
		origin: Moq.Origin.Table,
		path: Moq.Path.Valid,
	): Moq.Broadcast.Consumer | undefined {
		const request = origin.request(path);
		effect.cleanup(() => request.close());
		return effect.get(request.active);
	}

	// Subscribe to the broadcast, waiting for its announcement so we never race a publisher that
	// comes online after us. @moq/net drives the re-consume on a same-name republish and the blind
	// fallback on a relay without discovery; mirror its handle into `active`.
	#runBroadcast(effect: Effect): void {
		const enabled = effect.get(this.in.enabled);
		if (!enabled) return;

		const origin = effect.get(this.in.origin);
		if (!origin) return;

		const name = effect.get(this.in.name);

		// No announcement gate: subscribe immediately.
		if (!effect.get(this.in.reload)) {
			effect.set(this.#out.active, this.#requestBroadcast(effect, origin, name), undefined);
			return;
		}

		const announced = new Announce.Broadcast({ origin: this.in.origin, path: name });
		effect.cleanup(() => announced.close());

		effect.run((nested) => {
			nested.set(this.#out.active, nested.get(announced.active), undefined);
		});
	}

	#runCatalog(effect: Effect): void {
		const enabled = effect.get(this.in.enabled);
		if (!enabled) return;

		const catalogFormat = effect.get(this.in.catalogFormat);
		const name = effect.get(this.in.name);
		// Explicit override beats name-derived auto-detection. When neither is
		// set we fall back to the default, keeping legacy names that have no
		// extension working.
		const format: CatalogFormat = catalogFormat ?? Catalog.detectFormat(name) ?? Catalog.DEFAULT_FORMAT;

		if (format === "manual") {
			// Mirror the caller-supplied catalog into the effective output. A caller-supplied
			// catalog is rejected the same way a fetched one is, minus the throw: this runs in
			// the effect body, where an exception would surface as an unhandled error.
			const catalog = effect.get(this.in.catalog);
			const escaping = catalog && findEscaping(name, catalog);
			if (escaping !== undefined) {
				console.error("rejecting catalog: broadcast reference escapes the root", name, escaping);
			}

			const accepted = escaping === undefined ? catalog : undefined;
			this.#raw.set(accepted, true);
			effect.cleanup(() => this.#raw.set(undefined, true));
			this.#out.status.set(accepted ? "live" : "loading");
			return;
		}

		const broadcast = effect.get(this.out.active);
		if (!broadcast) return;

		this.#out.status.set("loading");

		const trackName = format === "hang" ? Catalog.TRACK : format === "hangz" ? Catalog.TRACK_COMPRESSED : "catalog";
		const track = broadcast.track(trackName).subscribe({ priority: Catalog.PRIORITY.catalog });
		effect.cleanup(() => track.close());

		// The hang catalog is reconstructed from snapshots (and future deltas) via @moq/json, with
		// "hangz" decompressing the `.z` track; MSF stays on its own one-blob-per-group fetch.
		let fetchNext: () => Promise<Catalog.Root | undefined>;
		if (format === "hang" || format === "hangz") {
			const consumer = new Json.Snapshot.Consumer<Catalog.Root>(track, {
				schema: Catalog.RootSchema,
				compression: format === "hangz",
			});
			fetchNext = () => consumer.next();
		} else {
			fetchNext = async () => {
				const update = await Msf.fetch(track);
				return update ? toHang(update) : undefined;
			};
		}

		effect.spawn(async () => {
			try {
				for (;;) {
					const update = await Promise.race([effect.cancel, fetchNext()]);
					if (!update) break;

					console.debug("received catalog", format, this.in.name.peek(), update);

					this.#raw.set(assertResolvable(name, update), true);
					this.#out.status.set("live");
				}
			} catch (err) {
				console.error("error fetching catalog", this.in.name.peek(), err);
			} finally {
				this.#raw.set(undefined);
				this.#out.status.set("offline");
			}
		});
	}

	// Where a rendition's `broadcast` reference points once resolved and gated on the announcement
	// stream, or `undefined` when it names nothing consumable right now. Playback and rendition
	// selection both go through this so they cannot disagree about what is reachable.
	#relativeTarget(effect: Effect, rel: string | undefined): RelativeTarget | undefined {
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

		// Without an announcement gate (reload off, or no session supports discovery),
		// resolve blind rather than waiting for an announcement that never comes. With the
		// gate, only report the path usable once it is announced: the request then resolves
		// from the table, never blind.
		if (effect.get(this.in.reload) && effect.get(origin.discovery) !== false) {
			if (!this.#isPathAnnounced(effect, resolved)) return undefined;
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
	 */
	relativeBroadcast(effect: Effect, rel: string | undefined): Moq.Broadcast.Consumer | undefined {
		const target = this.#relativeTarget(effect, rel);
		if (!target) return undefined;
		if (target.local) return effect.get(this.out.active);

		if (!effect.get(this.in.enabled)) return undefined;

		const origin = effect.get(this.in.origin);
		if (!origin) return undefined;

		return this.#requestBroadcast(effect, origin, target.path);
	}

	close() {
		this.#signals.close();
	}
}
