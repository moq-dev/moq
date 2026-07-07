import * as Catalog from "@moq/hang/catalog";
import * as Json from "@moq/json";
import * as Msf from "@moq/msf";
import type * as Moq from "@moq/net";
import { isStreamAbort, Path } from "@moq/net";
import { Effect, type Getter, getter, type Inputs, type Readonlys, readonlys, Signal } from "@moq/signals";

import { toHang } from "./msf";

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
type BroadcastInput = {
	connection: Getter<Moq.Connection.Established | undefined>;

	// Whether to start downloading the broadcast.
	// Defaults to false so you can make sure everything is ready before starting.
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
	active: Signal<Moq.broadcast.Consumer | undefined>;

	// The effective catalog: the fetched one, or a copy of input.catalog in manual mode.
	catalog: Signal<Catalog.Root | undefined>;
};

// A catalog source that (optionally) reloads automatically when live/offline.
export class Broadcast {
	readonly input: Readonlys<BroadcastInput>;

	readonly #output: BroadcastOutput = {
		status: new Signal<Status>("offline"),
		active: new Signal<Moq.broadcast.Consumer | undefined>(undefined),
		catalog: new Signal<Catalog.Root | undefined>(undefined),
	};
	readonly output = readonlys(this.#output);

	// All actively announced broadcast paths from the connection. If omitted, reload skips the
	// announcement gate and subscribes immediately.
	readonly #announced?: Getter<Set<Moq.Path.Valid>>;

	// Per-path announce generation from the connection (optional; see the constructor props).
	readonly #announcedGenerations?: Getter<ReadonlyMap<Moq.Path.Valid, number>>;

	// The announce generation of `name`: 0 when not announced, else the connection's generation for it (or
	// 1 when generations aren't provided). A NUMBER rather than a boolean so a same-name republish by a new
	// publisher (generation bump) re-runs #runBroadcast and re-consumes against the new instance. Derived
	// in its own effect so flaps for unrelated broadcasts don't retrigger the broadcast/catalog subs.
	readonly #announcedNow = new Signal(0);

	signals = new Effect();

	constructor(
		props?: Inputs<BroadcastInput> & {
			announced?: Getter<Set<Moq.Path.Valid>>;
			announcedGenerations?: Getter<ReadonlyMap<Moq.Path.Valid, number>>;
		},
	) {
		this.input = {
			connection: getter(props?.connection),
			name: getter(props?.name ?? Path.empty()),
			enabled: getter(props?.enabled ?? false),
			reload: getter(props?.reload ?? true),
			catalogFormat: getter<CatalogFormat | undefined>(props?.catalogFormat),
			catalog: getter(props?.catalog),
		};

		this.#announced = props?.announced;
		this.#announcedGenerations = props?.announcedGenerations;

		this.signals.run(this.#runAnnouncedNow.bind(this));
		this.signals.run(this.#runBroadcast.bind(this));
		this.signals.run(this.#runCatalog.bind(this));
	}

	#runAnnouncedNow(effect: Effect): void {
		const reload = effect.get(this.input.reload);
		if (!reload) {
			this.#announcedNow.set(1);
			return;
		}

		if (!this.#announced) {
			this.#announcedNow.set(1);
			return;
		}

		// Cloudflare's relay does not yet support announcement subscriptions,
		// so default to subscribing immediately instead of waiting forever.
		const conn = effect.get(this.input.connection);
		if (conn?.url.hostname.endsWith("mediaoverquic.com")) {
			this.#announcedNow.set(1);
			return;
		}

		const name = effect.get(this.input.name);
		const announced = effect.get(this.#announced);
		if (!announced.has(name)) {
			this.#announcedNow.set(0);
			return;
		}

		// Announced: track the per-path generation so a same-name republish (a new publisher instance)
		// re-runs #runBroadcast and re-consumes, even if the presence Set didn't observably flip.
		const gen = this.#announcedGenerations ? effect.get(this.#announcedGenerations).get(name) : undefined;
		this.#announcedNow.set(gen ?? 1);
	}

	#runBroadcast(effect: Effect): void {
		const enabled = effect.get(this.input.enabled);
		if (!enabled) return;

		if (!effect.get(this.#announcedNow)) return;

		const conn = effect.get(this.input.connection);
		if (!conn) return;

		const name = effect.get(this.input.name);
		const broadcast = conn.consume(name);
		effect.cleanup(() => broadcast.close());

		effect.set(this.#output.active, broadcast, undefined);
	}

	#runCatalog(effect: Effect): void {
		const enabled = effect.get(this.input.enabled);
		if (!enabled) return;

		const catalogFormat = effect.get(this.input.catalogFormat);
		const name = effect.get(this.input.name);
		// Explicit override beats name-derived auto-detection. When neither is
		// set we fall back to the default, keeping legacy names that have no
		// extension working.
		const format: CatalogFormat = catalogFormat ?? Catalog.detectFormat(name) ?? Catalog.DEFAULT_FORMAT;

		if (format === "manual") {
			// Mirror the caller-supplied catalog into the effective output.
			const catalog = effect.get(this.input.catalog);
			effect.set(this.#output.catalog, catalog, undefined);
			this.#output.status.set(catalog ? "live" : "loading");
			return;
		}

		const broadcast = effect.get(this.output.active);
		if (!broadcast) return;

		this.#output.status.set("loading");

		const trackName = format === "hang" ? Catalog.TRACK : format === "hangz" ? Catalog.TRACK_COMPRESSED : "catalog";
		const track = broadcast.track(trackName).subscribe({ priority: Catalog.PRIORITY.catalog });
		effect.cleanup(() => track.close());

		// The hang catalog is reconstructed from snapshots (and future deltas) via @moq/json, with
		// "hangz" decompressing the `.z` track; MSF stays on its own one-blob-per-group fetch.
		let fetchNext: () => Promise<Catalog.Root | undefined>;
		if (format === "hang" || format === "hangz") {
			const consumer = new Json.Consumer<Catalog.Root>(track, {
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

					console.debug("received catalog", format, this.input.name.peek(), update);

					this.#output.catalog.set(update);
					this.#output.status.set("live");
				}
			} catch (err) {
				// A routine transport reset during a publisher handover is expected; a real fetch/parse
				// failure (auth, not-found, protocol, or schema validation) still warns.
				console[isStreamAbort(err) ? "debug" : "warn"]("error fetching catalog", this.input.name.peek(), err);
			} finally {
				this.#output.catalog.set(undefined);
				this.#output.status.set("offline");
			}
		});
	}

	close() {
		this.signals.close();
	}
}
