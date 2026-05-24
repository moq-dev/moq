import * as Catalog from "@moq/hang/catalog";
import type * as Moq from "@moq/lite";
import { Path } from "@moq/lite";
import * as Msf from "@moq/msf";
import { Effect, type Getter, Signal } from "@moq/signals";

import { toHang } from "./msf";

export const CATALOG_FORMATS = ["hang", "msf", "manual"] as const;
export type CatalogFormat = (typeof CATALOG_FORMATS)[number];

export interface BroadcastProps {
	connection?: Moq.Connection.Established | Signal<Moq.Connection.Established | undefined>;

	// All actively announced broadcast paths from the connection.
	announced?: Getter<Set<Moq.Path.Valid>>;

	// Whether to start downloading the broadcast.
	// Defaults to false so you can make sure everything is ready before starting.
	enabled?: boolean | Signal<boolean>;

	// The broadcast name.
	name?: Moq.Path.Valid | Signal<Moq.Path.Valid>;

	// Whether to reload the broadcast when it goes offline.
	// Defaults to false; pass true to wait for an announcement before subscribing.
	reload?: boolean | Signal<boolean>;

	// Which catalog format to use. Default: "hang"
	catalogFormat?: CatalogFormat | Signal<CatalogFormat>;

	// Initial catalog. Used directly when catalogFormat is "manual"; otherwise it's
	// overwritten by whatever the fetched catalog track produces. Note: switching
	// catalogFormat between "manual" and a fetched format will reset this signal
	// to undefined when the fetched-format spawn tears down — set the catalog
	// after switching formats, not before.
	catalog?: Catalog.Root | Signal<Catalog.Root | undefined>;
}

// A catalog source that (optionally) reloads automatically when live/offline.
export class Broadcast {
	connection: Signal<Moq.Connection.Established | undefined>;

	enabled: Signal<boolean>;
	name: Signal<Moq.Path.Valid>;
	status = new Signal<"offline" | "loading" | "live">("offline");
	reload: Signal<boolean>;

	catalogFormat: Signal<CatalogFormat>;

	#active = new Signal<Moq.Broadcast | undefined>(undefined);
	readonly active: Getter<Moq.Broadcast | undefined> = this.#active;

	// The active catalog. Writable so users can supply it directly when
	// catalogFormat is "manual"; otherwise the fetch loop owns writes.
	catalog: Signal<Catalog.Root | undefined>;

	// All actively announced broadcast paths from the connection.
	#announced: Getter<Set<Moq.Path.Valid>>;

	signals = new Effect();

	constructor(props?: BroadcastProps) {
		this.connection = Signal.from(props?.connection);
		this.name = Signal.from(props?.name ?? Path.empty());
		this.enabled = Signal.from(props?.enabled ?? false);
		this.reload = Signal.from(props?.reload ?? false);
		this.catalogFormat = Signal.from(props?.catalogFormat ?? "hang");
		this.catalog = Signal.from(props?.catalog);

		this.#announced = props?.announced ?? new Signal(new Set());

		this.signals.run(this.#runBroadcast.bind(this));
		this.signals.run(this.#runCatalog.bind(this));
	}

	#isAnnounced(effect: Effect): boolean {
		const name = effect.get(this.name);
		return this.#isPathAnnounced(effect, name);
	}

	#isPathAnnounced(effect: Effect, name: Path.Valid): boolean {
		const reload = effect.get(this.reload);
		if (!reload) return true;

		// Cloudflare's relay does not yet support announcement subscriptions,
		// so an announcement will never arrive. Fall back to subscribing
		// immediately (reload=false behaviour) instead of waiting forever.
		const conn = effect.get(this.connection);
		if (conn?.url.hostname.endsWith("mediaoverquic.com")) {
			console.warn("Cloudflare relay does not support broadcast discovery yet; ignoring reload signal.");
			return true;
		}

		const announced = effect.get(this.#announced);
		return announced.has(name);
	}

	#runBroadcast(effect: Effect): void {
		const enabled = effect.get(this.enabled);
		if (!enabled) return;

		if (!this.#isAnnounced(effect)) return;

		const conn = effect.get(this.connection);
		if (!conn) return;

		const name = effect.get(this.name);

		const broadcast = conn.consume(name);
		effect.cleanup(() => broadcast.close());

		effect.set(this.#active, broadcast, undefined);
	}

	#runCatalog(effect: Effect): void {
		const enabled = effect.get(this.enabled);
		if (!enabled) return;

		const format = effect.get(this.catalogFormat);

		if (format === "manual") {
			// User-supplied catalog; no track to fetch.
			const catalog = effect.get(this.catalog);
			this.status.set(catalog ? "live" : "loading");
			return;
		}

		const broadcast = effect.get(this.active);
		if (!broadcast) return;

		this.status.set("loading");

		const trackName = format === "hang" ? "catalog.json" : "catalog";
		const track = broadcast.subscribe(trackName, Catalog.PRIORITY.catalog);
		effect.cleanup(() => track.close());

		const fetchNext =
			format === "hang"
				? async () => Catalog.fetch(track)
				: async () => {
						const update = await Msf.fetch(track);
						return update ? toHang(update) : undefined;
					};

		effect.spawn(async () => {
			try {
				for (;;) {
					const update = await Promise.race([effect.cancel, fetchNext()]);
					if (!update) break;

					console.debug("received catalog", format, this.name.peek(), update);

					this.catalog.set(update);
					this.status.set("live");
				}
			} catch (err) {
				console.warn("error fetching catalog", this.name.peek(), err);
			} finally {
				this.catalog.set(undefined);
				this.status.set("offline");
			}
		});
	}

	/**
	 * Resolve the `Moq.Broadcast` that publishes a given track.
	 *
	 * If `configBroadcast` is set, treat it as a path relative to this broadcast's name and
	 * subscribe to the resolved broadcast on the same connection. Otherwise return the catalog's
	 * own active broadcast.
	 *
	 * Override broadcasts are cached per resolved path and owned by this Broadcast's
	 * `signals`; the caller's `effect` only subscribes to the cached signal. This means
	 * many renditions referencing the same source share one underlying subscription, and
	 * the override outlives any single caller effect.
	 */
	trackBroadcast(effect: Effect, configBroadcast: string | undefined): Moq.Broadcast | undefined {
		if (!configBroadcast) return effect.get(this.active);

		const basePath = effect.get(this.name);
		const resolved = Catalog.resolveBroadcast(basePath, configBroadcast);

		// Self-reference (including `..` paths that walk back to the catalog's own path,
		// and any rel that normalizes to empty): reuse the catalog's broadcast handle
		// instead of opening a duplicate subscription on the same path.
		if (resolved === basePath) return effect.get(this.active);

		return effect.get(this.#override(resolved));
	}

	#overrides = new Map<Path.Valid, Signal<Moq.Broadcast | undefined>>();

	#override(path: Path.Valid): Signal<Moq.Broadcast | undefined> {
		const cached = this.#overrides.get(path);
		if (cached) return cached;

		const signal = new Signal<Moq.Broadcast | undefined>(undefined);
		this.#overrides.set(path, signal);

		this.signals.run((effect) => {
			const conn = effect.get(this.connection);
			if (!conn) return;

			if (!this.#isPathAnnounced(effect, path)) return;

			const remote = conn.consume(path);
			effect.cleanup(() => remote.close());
			effect.set(signal, remote, undefined);
		});

		return signal;
	}

	close() {
		this.signals.close();
	}
}
