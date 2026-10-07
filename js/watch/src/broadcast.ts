import * as Catalog from "@moq/hang/catalog";
import * as Json from "@moq/json";
import * as Msf from "@moq/msf";
import type * as Moq from "@moq/net";
import { Error as NetError, Path } from "@moq/net";
import { Effect, type Getter, getter, type Inputs, type Readonlys, readonlys, Signal } from "@moq/signals";

import { toHang } from "./msf";

type ReferencedRendition = {
	broadcast?: Path.Relative;
};

// Either the catalog's own broadcast, or a sibling to consume by path.
type RelativeTarget = { local: true } | { local: false; path: Moq.Path.Valid };

function filterRenditions<T extends ReferencedRendition>(
	renditions: Record<string, T>,
	usable: (rel: Path.Relative | undefined) => boolean,
): Record<string, T> {
	return Object.fromEntries(Object.entries(renditions).filter(([, config]) => usable(config.broadcast)));
}

// Every section carrying a `broadcast` reference must be listed here, same as `Catalog.checkResolvable`;
// one left out silently exempts its tracks from the reachability filter.
function filterCatalog(catalog: Catalog.Root, usable: (rel: Path.Relative | undefined) => boolean): Catalog.Root {
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

// Watch supports the on-the-wire catalog formats from @moq/hang, plus "hangz" (the
// DEFLATE-compressed `catalog.json.z` track) and a "manual" mode where the user supplies the
// catalog directly without fetching. "hangz" is opt-in only: it shares the `.hang` broadcast suffix
// and is never auto-detected, so set it explicitly via `catalogFormat`.
export const CATALOG_FORMATS = [...Catalog.FORMATS, "hangz", "manual"] as const;
export type CatalogFormat = (typeof CATALOG_FORMATS)[number];

// "error" means the origin refused the broadcast; `out.error` says why.
type Status = "offline" | "loading" | "live" | "error";

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

	// Whether to wait for the broadcast to be announced before subscribing.
	// Defaults to true; pass false to subscribe immediately without waiting for an announcement.
	announced: Getter<boolean>;

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

	// Why the origin refused the broadcast, while `status` is "error". A refusal is final: only a
	// fresh request (a new `name`, `origin`, or `announced`, re-enabling, or a route without an
	// epoch changing) clears it and asks again.
	error: Signal<Error | undefined>;

	// The effective catalog: the fetched one, or a copy of input.catalog in manual mode, minus
	// any rendition this consumer can't use (see `#runFiltered`). A rendition referencing another
	// broadcast appears once that broadcast is announced, so this can change without a new catalog.
	catalog: Signal<Catalog.Root | undefined>;
};

// A catalog source that can wait for announcement before subscribing.
export class Broadcast {
	readonly in: Readonlys<BroadcastInput>;

	readonly #out: BroadcastOutput = {
		status: new Signal<Status>("offline"),
		active: new Signal<Moq.Broadcast.Consumer | undefined>(undefined),
		error: new Signal<Error | undefined>(undefined),
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

	#signals: Effect;

	constructor(props?: Inputs<BroadcastInput>) {
		if (props && "reload" in props) {
			throw new Error("Watch.Broadcast: `reload` was renamed to `announced`");
		}
		this.#signals = new Effect();
		this.in = {
			origin: getter(props?.origin),
			name: getter(props?.name ?? Path.empty()),
			enabled: getter(props?.enabled ?? true),
			announced: getter(props?.announced ?? true),
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
		if (!effect.get(this.in.announced)) return;

		const origin = effect.get(this.in.origin);
		if (!origin) return;

		// Hidden routes count: a service claim under a `.`-named prefix is kept out of listings, but
		// it still covers the renditions it would produce, and this set is never shown to anyone.
		const announced = origin.announced(Path.Pattern.all(), { hidden: true });
		effect.cleanup(() => announced.close());
		this.#announced.set(new Set());

		effect.spawn(async () => {
			for (;;) {
				const entry = await effect.race(announced.next());
				if (!entry) break;
				this.#announced.mutate((active) => {
					if (!active) return;
					if (entry.kind === "end") active.delete(entry.prefix);
					else active.add(entry.prefix);
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
		const usable = (rel: Path.Relative | undefined) => this.#relativeTarget(effect, rel) !== undefined;
		effect.set(this.#out.catalog, raw ? filterCatalog(raw, usable) : undefined);
	}

	// Whether `path` is covered by an announced route, for `relativeBroadcast`'s
	// cross-broadcast refs. Announcements are prefix routes, so a route at "room/" covers
	// "room/alice/cam.hang" without naming it. That is how a rendition produced only on demand
	// gets selected: its service claims a covering prefix, and nothing announces the exact path
	// until this subscribes. Opens the announcement stream on first use.
	// The blind cases (announcement gate off, no discovery) never reach here; see `#relativeTarget`.
	#isPathAnnounced(effect: Effect, path: Moq.Path.Valid): boolean {
		this.#wantAnnounced.set(true);

		const active = effect.get(this.#announced);
		if (!active) return false; // stream not open yet: wait rather than subscribe to a maybe-absent path
		if (active.has(path)) return true;
		for (const prefix of active) {
			if (Path.hasPrefix(prefix, path)) return true;
		}
		return false;
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

	// Subscribe to the broadcast, by default waiting for its announcement so we never race a
	// publisher that comes online after us. @moq/net drives the re-consume on a same-name republish
	// and the blind fallback on a relay without discovery; mirror its handle into `active`, and its
	// refusal into `error`.
	#runBroadcast(effect: Effect): void {
		const enabled = effect.get(this.in.enabled);
		if (!enabled) return;

		const origin = effect.get(this.in.origin);
		if (!origin) return;

		const name = effect.get(this.in.name);
		const announced = effect.get(this.in.announced);

		// An epoch names the publisher instance, and a new one already swaps the handle. A route
		// without one (moq-lite 06) cannot say whether a change in place is a restarted publisher,
		// so ask for the path again on any change, which starts everything downstream over.
		const routes = origin.broadcasts(undefined, { hidden: true });
		const instance = announced
			? effect.computed((nested) => {
					let route: Moq.Origin.Route | undefined;
					let length = -1;
					for (const [prefix, candidate] of nested.get(routes)) {
						if (prefix.length > length && Path.hasPrefix(prefix, name)) {
							route = candidate;
							length = prefix.length;
						}
					}
					return route?.epoch ?? route;
				})
			: undefined;

		effect.run((nested) => {
			if (instance) nested.get(instance);

			const request = origin.request(name, { announced });
			nested.cleanup(() => request.close());

			nested.run((inner) => {
				inner.set(this.#out.active, inner.get(request.active), undefined);
			});

			nested.run((inner) => {
				const closed = inner.get(request.closed);
				if (closed) inner.set(this.#out.error, closed, undefined);
			});
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
			let accepted: Catalog.Root | undefined;
			try {
				accepted = catalog && Catalog.checkResolvable(Catalog.checkRenditions(catalog), name);
			} catch (err) {
				console.error("rejecting catalog", name, err);
			}
			this.#raw.set(accepted, true);
			effect.cleanup(() => this.#raw.set(undefined, true));
			this.#out.status.set(accepted ? "live" : "loading");
			return;
		}

		const broadcast = effect.get(this.out.active);
		// A withdrawn handle can close before its removal propagates through the active signal.
		if (!broadcast || effect.get(broadcast.closed) !== undefined) return;

		this.#out.status.set("loading");

		const trackName = format === "hang" ? Catalog.TRACK : format === "hangz" ? Catalog.TRACK_COMPRESSED : "catalog";
		const track = broadcast.track(trackName).subscribe({ priority: Catalog.PRIORITY.catalog });
		effect.cleanup(() => track.close());

		// The hang catalog is reconstructed from snapshots (and future deltas) via @moq/json, with
		// "hangz" decompressing the `.z` track; MSF stays on its own one-blob-per-group fetch.
		let fetchNext: () => Promise<Catalog.Root | undefined>;
		if (format === "hang" || format === "hangz") {
			const consumer = new Json.Snapshot.Consumer<Catalog.Root>({
				track,
				schema: Catalog.RootSchema,
				compression: format === "hangz" ? "deflate" : "none",
			});
			fetchNext = () => consumer.next();
		} else {
			const ordered = track.ordered();
			fetchNext = async () => {
				const update = await Msf.fetch(ordered);
				return update ? toHang(update) : undefined;
			};
		}

		effect.spawn(async () => {
			try {
				for (;;) {
					const update = await effect.race(fetchNext());
					if (!update) break;

					console.debug("received catalog", format, this.in.name.peek(), update);

					this.#raw.set(Catalog.checkResolvable(Catalog.checkRenditions(update), name), true);
					this.#out.status.set("live");
				}
			} catch (err) {
				if (err instanceof NetError.Stream)
					console.debug("catalog subscription ended", this.in.name.peek(), err);
				else console.error("error fetching catalog", this.in.name.peek(), err);
			} finally {
				this.#raw.set(undefined);
				this.#out.status.set("offline");
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
		if (effect.get(this.in.announced) && effect.get(origin.discovery) !== false) {
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
	relativeBroadcast(effect: Effect, rel: Path.Relative | undefined): Moq.Broadcast.Consumer | undefined {
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
