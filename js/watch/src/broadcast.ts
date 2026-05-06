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

		const name = effect.get(this.name);
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

	close() {
		this.signals.close();
	}
}
