import * as Catalog from "@moq/hang/catalog";
import * as Json from "@moq/json";
import * as Msf from "@moq/msf";
import type * as Moq from "@moq/net";
import { Path } from "@moq/net";
import { Effect, type Getter, getter, type Inputs, type Readonlys, readonlys, Signal } from "@moq/signals";

import { toHang } from "./msf";

// Connections already warned about missing broadcast-discovery support, so the
// announcement check logs at most once per connection.
const warnedNoDiscovery = new WeakSet<Moq.Connection.Established>();

// Whether to skip the announcement gate for this connection: without discovery, waiting on an
// announcement would hang forever, so subscribe immediately and warn once per connection.
function skipDiscovery(conn: Moq.Connection.Established): boolean {
	if (conn.discovery) return false;
	if (!warnedNoDiscovery.has(conn)) {
		warnedNoDiscovery.add(conn);
		console.warn("relay does not support broadcast discovery; ignoring reload signal.");
	}
	return true;
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
	active: Signal<Moq.Broadcast.Consumer | undefined>;

	// The effective catalog: the fetched one, or a copy of input.catalog in manual mode.
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

	#signals = new Effect();

	constructor(props?: Inputs<BroadcastInput>) {
		this.in = {
			connection: getter(props?.connection),
			name: getter(props?.name ?? Path.empty()),
			enabled: getter(props?.enabled ?? false),
			reload: getter(props?.reload ?? true),
			catalogFormat: getter<CatalogFormat | undefined>(props?.catalogFormat),
			catalog: getter(props?.catalog),
		};

		this.#signals.run(this.#runAnnounced.bind(this));
		this.#signals.run(this.#runBroadcast.bind(this));
		this.#signals.run(this.#runCatalog.bind(this));
	}

	// Maintain the set of announced paths used by `relativeBroadcast`, by draining a connection-scoped
	// announcement stream. Only opened once a relative reference asks for it (see `#wantAnnounced`),
	// and reopened per connection.
	#runAnnounced(effect: Effect): void {
		this.#announced.set(undefined);

		if (!effect.get(this.#wantAnnounced)) return;
		if (!effect.get(this.in.reload)) return;

		const conn = effect.get(this.in.connection);
		if (!conn || skipDiscovery(conn)) return;

		const announced = conn.announced(Path.empty());
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

	// Whether `path` is currently announced, for `relativeBroadcast`'s cross-broadcast refs. Returns
	// true (subscribe immediately) when the gate can't apply: reload is off, or the relay doesn't
	// support discovery. Opens the announcement stream on first use.
	#isPathAnnounced(effect: Effect, path: Moq.Path.Valid): boolean {
		if (!effect.get(this.in.reload)) return true;

		const conn = effect.get(this.in.connection);
		if (conn && skipDiscovery(conn)) return true;

		this.#wantAnnounced.set(true);

		const active = effect.get(this.#announced);
		if (!active) return false; // stream not open yet: wait rather than subscribe to a maybe-absent path
		return active.has(path);
	}

	// Subscribe to the broadcast, re-consuming on every (re-)announce so a same-name republish (a new
	// publisher, or a relay-failover RESTART) re-attaches to the new instance instead of clinging to
	// the dead one. Driven off the announcement stream's updates rather than a membership flag, since
	// a coalesced republish leaves the active set unchanged yet still emits a fresh update.
	#runBroadcast(effect: Effect): void {
		const enabled = effect.get(this.in.enabled);
		if (!enabled) return;

		const conn = effect.get(this.in.connection);
		if (!conn) return;

		const name = effect.get(this.in.name);

		// No announcement gate: subscribe immediately (reload off, or the relay lacks discovery).
		if (!effect.get(this.in.reload) || skipDiscovery(conn)) {
			const broadcast = conn.consume(name);
			effect.cleanup(() => broadcast.close());
			effect.set(this.#out.active, broadcast, undefined);
			return;
		}

		const announced = conn.announced(name);
		effect.cleanup(() => announced.close());

		let current: Moq.Broadcast.Consumer | undefined;
		effect.cleanup(() => {
			current?.close();
			current = undefined;
			this.#out.active.set(undefined);
		});

		effect.spawn(async () => {
			for (;;) {
				const event = await Promise.race([effect.cancel, announced.next()]);
				if (!event) break;

				// Scoped to `name`, so the exact broadcast arrives with an empty suffix; ignore children.
				if (event.path !== Path.empty()) continue;

				if (event.active) {
					// A live subscription survives a redundant (re-)announce; only replace a dead one.
					if (current && current.closed.peek() === undefined) continue;
					current?.close();
					current = conn.consume(name);
					this.#out.active.set(current);
				} else {
					current?.close();
					current = undefined;
					this.#out.active.set(undefined);
				}
			}
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
			// Mirror the caller-supplied catalog into the effective output.
			const catalog = effect.get(this.in.catalog);
			effect.set(this.#out.catalog, catalog, undefined);
			this.#out.status.set(catalog ? "live" : "loading");
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

					this.#out.catalog.set(update);
					this.#out.status.set("live");
				}
			} catch (err) {
				console.warn("error fetching catalog", this.in.name.peek(), err);
			} finally {
				this.#out.catalog.set(undefined);
				this.#out.status.set("offline");
			}
		});
	}

	/**
	 * Resolve the `Moq.Broadcast.Consumer` that publishes a given track.
	 *
	 * If `rel` is set (a rendition's catalog `broadcast` field), treat it as a path
	 * relative to this broadcast's name and consume the resolved broadcast on the same
	 * connection. Otherwise return the catalog's own active broadcast.
	 *
	 * The consumer is scoped to the caller's `effect` (closed on its next run), so a
	 * reference resolves lazily and reacts to `enabled` / connection / announcement
	 * changes exactly like the catalog broadcast.
	 */
	relativeBroadcast(effect: Effect, rel: string | undefined): Moq.Broadcast.Consumer | undefined {
		if (!rel) return effect.get(this.out.active);

		const base = effect.get(this.in.name);
		const resolved = Path.resolve(base, rel);

		// A reference that walks back to the catalog's own broadcast (or resolves to
		// the empty root, via excess `..`) is served by the catalog broadcast itself,
		// avoiding a duplicate subscription on the same path.
		if (resolved === base || resolved === Path.empty()) return effect.get(this.out.active);

		if (!effect.get(this.in.enabled)) return undefined;

		const conn = effect.get(this.in.connection);
		if (!conn) return undefined;

		if (!this.#isPathAnnounced(effect, resolved)) return undefined;

		const broadcast = conn.consume(resolved);
		effect.cleanup(() => broadcast.close());
		return broadcast;
	}

	close() {
		this.#signals.close();
	}
}
