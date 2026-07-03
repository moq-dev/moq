import * as Catalog from "@moq/hang/catalog";
import * as Json from "@moq/json";
import * as Msf from "@moq/msf";
import type * as Moq from "@moq/net";
import { Path } from "@moq/net";
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
	active: Signal<Moq.Broadcast | undefined>;

	// The effective catalog: the fetched one, or a copy of input.catalog in manual mode.
	catalog: Signal<Catalog.Root | undefined>;
};

// A catalog source that (optionally) reloads automatically when live/offline.
export class Broadcast {
	readonly input: Readonlys<BroadcastInput>;

	readonly #output: BroadcastOutput = {
		status: new Signal<Status>("offline"),
		active: new Signal<Moq.Broadcast | undefined>(undefined),
		catalog: new Signal<Catalog.Root | undefined>(undefined),
	};
	readonly output = readonlys(this.#output);

	// All actively announced broadcast paths from the connection. If omitted, reload skips the
	// announcement gate and subscribes immediately.
	readonly #announced?: Getter<Set<Moq.Path.Valid>>;

	// Whether `name` is currently in the announced set (or skipping the check).
	// Derived in its own effect so that flaps for unrelated broadcasts don't
	// retrigger the broadcast/catalog subscriptions.
	readonly #announcedNow = new Signal(false);

	signals = new Effect();

	constructor(props?: Inputs<BroadcastInput> & { announced?: Getter<Set<Moq.Path.Valid>> }) {
		this.input = {
			connection: getter(props?.connection),
			name: getter(props?.name ?? Path.empty()),
			enabled: getter(props?.enabled ?? false),
			reload: getter(props?.reload ?? true),
			catalogFormat: getter<CatalogFormat | undefined>(props?.catalogFormat),
			catalog: getter(props?.catalog),
		};

		this.#announced = props?.announced;

		this.signals.run(this.#runAnnouncedNow.bind(this));
		this.signals.run(this.#runBroadcast.bind(this));
		this.signals.run(this.#runCatalog.bind(this));
	}

	#runAnnouncedNow(effect: Effect): void {
		const reload = effect.get(this.input.reload);
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
		const conn = effect.get(this.input.connection);
		if (conn?.url.hostname.endsWith("mediaoverquic.com")) {
			this.#announcedNow.set(true);
			return;
		}

		const name = effect.get(this.input.name);
		const announced = effect.get(this.#announced);
		this.#announcedNow.set(announced.has(name));
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
				console.warn("error fetching catalog", this.input.name.peek(), err);
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
