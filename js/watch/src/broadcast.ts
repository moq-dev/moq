import * as Catalog from "@moq/hang/catalog";
import * as Msf from "@moq/msf";
import type * as Moq from "@moq/net";
import { Path } from "@moq/net";
import { Effect, type Getter, Signal } from "@moq/signals";

import { toHang } from "./msf";

/** Consumes a custom track once subscribed, scoped to the subscription's lifetime. */
export type ConsumeTrack = (track: Moq.Track, effect: Effect) => void;

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

export interface BroadcastProps {
	connection?: Moq.Connection.Established | Signal<Moq.Connection.Established | undefined>;

	// All actively announced broadcast paths from the connection. If omitted, reload skips the
	// announcement gate and subscribes immediately.
	announced?: Getter<Set<Moq.Path.Valid>>;

	// Whether to start downloading the broadcast.
	// Defaults to false so you can make sure everything is ready before starting.
	enabled?: boolean | Signal<boolean>;

	// The broadcast name.
	name?: Moq.Path.Valid | Signal<Moq.Path.Valid>;

	// Whether to reload the broadcast when it goes offline.
	// Defaults to true; pass false to subscribe immediately without waiting for an announcement.
	reload?: boolean | Signal<boolean>;

	// Which catalog format to use. When `undefined` (the default), the format is
	// auto-detected from the broadcast name extension (`.hang`, `.msf`), falling
	// back to `"hang"` if the name has no recognized extension. Set to a
	// specific value to override auto-detection. `"hangz"` (the compressed
	// `catalog.json.z` track) is opt-in only and never auto-detected.
	catalogFormat?: CatalogFormat | Signal<CatalogFormat | undefined>;

	// Initial catalog. Used directly when catalogFormat is "manual"; otherwise it's
	// overwritten by whatever the fetched catalog track produces. Note: switching
	// catalogFormat between "manual" and a fetched format will reset this signal
	// to undefined when the fetched-format spawn tears down. Set the catalog
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

	// `undefined` means auto-detect from the broadcast name extension.
	catalogFormat: Signal<CatalogFormat | undefined>;

	#active = new Signal<Moq.Broadcast | undefined>(undefined);
	readonly active: Getter<Moq.Broadcast | undefined> = this.#active;

	// The active catalog. Writable so users can supply it directly when
	// catalogFormat is "manual"; otherwise the fetch loop owns writes.
	catalog: Signal<Catalog.Root | undefined>;

	// All actively announced broadcast paths from the connection.
	#announced?: Getter<Set<Moq.Path.Valid>>;

	// Whether `name` is currently in the announced set (or skipping the check).
	// Derived in its own effect so that flaps for unrelated broadcasts don't
	// retrigger the broadcast/catalog subscriptions.
	#announcedNow = new Signal(false);

	signals = new Effect();

	constructor(props?: BroadcastProps) {
		this.connection = Signal.from(props?.connection);
		this.name = Signal.from(props?.name ?? Path.empty());
		this.enabled = Signal.from(props?.enabled ?? false);
		this.reload = Signal.from(props?.reload ?? true);
		this.catalogFormat = Signal.from<CatalogFormat | undefined>(props?.catalogFormat);
		this.catalog = Signal.from(props?.catalog);

		this.#announced = props?.announced;

		this.signals.run(this.#runAnnouncedNow.bind(this));
		this.signals.run(this.#runBroadcast.bind(this));
		this.signals.run(this.#runCatalog.bind(this));
	}

	#runAnnouncedNow(effect: Effect): void {
		const reload = effect.get(this.reload);
		if (!reload) {
			this.#announcedNow.set(true);
			return;
		}

		if (!this.#announced) {
			this.#announcedNow.set(true);
			return;
		}

		// Cloudflare's relay does not yet support announcement subscriptions,
		// so default to subscribing immediately instead of waiting forever.
		const conn = effect.get(this.connection);
		if (conn?.url.hostname.endsWith("mediaoverquic.com")) {
			this.#announcedNow.set(true);
			return;
		}

		const name = effect.get(this.name);
		const announced = effect.get(this.#announced);
		this.#announcedNow.set(announced.has(name));
	}

	#runBroadcast(effect: Effect): void {
		const enabled = effect.get(this.enabled);
		if (!enabled) return;

		if (!effect.get(this.#announcedNow)) return;

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

		const catalogFormat = effect.get(this.catalogFormat);
		const name = effect.get(this.name);
		// Explicit override beats name-derived auto-detection. When neither is
		// set we fall back to the default, keeping legacy names that have no
		// extension working.
		const format: CatalogFormat = catalogFormat ?? Catalog.detectFormat(name) ?? Catalog.DEFAULT_FORMAT;

		if (format === "manual") {
			// User-supplied catalog; no track to fetch.
			const catalog = effect.get(this.catalog);
			this.status.set(catalog ? "live" : "loading");
			return;
		}

		const broadcast = effect.get(this.active);
		if (!broadcast) return;

		this.status.set("loading");

		const trackName = format === "hang" ? Catalog.TRACK : format === "hangz" ? Catalog.TRACK_COMPRESSED : "catalog";
		const track = broadcast.subscribe(trackName, Catalog.PRIORITY.catalog);
		effect.cleanup(() => track.close());

		// The hang catalog is reconstructed from snapshots (and future deltas) via @moq/json, with
		// "hangz" decompressing the `.z` track; MSF stays on its own one-blob-per-group fetch.
		let fetchNext: () => Promise<Catalog.Root | undefined>;
		if (format === "hang" || format === "hangz") {
			const consumer = new Catalog.Consumer(track, { compression: format === "hangz" });
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
	 * Subscribe to a custom track within this broadcast, following the active broadcast across
	 * reconnects. `consume` runs with a freshly-subscribed track and a subscription-scoped effect
	 * each time a broadcast becomes active (re-running on reconnect).
	 *
	 * For a JSON track, wrap the track with a `@moq/json` `Consumer` and read it in a spawned loop
	 * (e.g. into a Signal). An application advertises the track in its own catalog section, which it
	 * reads back from {@link catalog} (unknown sections pass through the loose schema):
	 *
	 * ```ts
	 * import * as Json from "@moq/json";
	 * const scte35 = new Signal<{ splices: number[] } | undefined>(undefined);
	 * broadcast.subscribeTrack("scte35.json", Catalog.PRIORITY.catalog, (track, effect) => {
	 * 	const consumer = new Json.Consumer<{ splices: number[] }>(track);
	 * 	effect.spawn(async () => {
	 * 		for (;;) {
	 * 			const next = await Promise.race([effect.cancel, consumer.next()]);
	 * 			if (next === undefined) break;
	 * 			scte35.set(next);
	 * 		}
	 * 	});
	 * });
	 * ```
	 *
	 * Returns a function to stop subscribing; also stopped when this broadcast closes.
	 */
	subscribeTrack(name: string, priority: number, consume: ConsumeTrack): () => void {
		const signals = new Effect();
		signals.run((effect) => {
			const active = effect.get(this.active);
			if (!active) return;

			const track = active.subscribe(name, priority);
			effect.cleanup(() => track.close());

			consume(track, effect);
		});
		this.signals.cleanup(() => signals.close());
		return () => signals.close();
	}

	close() {
		this.signals.close();
	}
}
