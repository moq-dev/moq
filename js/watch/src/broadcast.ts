import * as Catalog from "@moq/hang/catalog";
import * as Json from "@moq/json";
import * as Msf from "@moq/msf";
import type * as Moq from "@moq/net";
import { isStreamAbort, Path } from "@moq/net";
import { Effect, type Getter, getter, type Inputs, type Readonlys, readonlys, Signal } from "@moq/signals";

import { toHang } from "./msf";

// Connections already warned about missing broadcast-discovery support, so the
// per-rendition announcement check logs at most once per connection.
const warnedNoDiscovery = new WeakSet<Moq.Connection.Established>();

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

// Backoff for reopening a catalog subscription whose stream ended unexpectedly: a slow-consumer reset, a
// relay bounce, or a publisher that dropped without unannouncing. Nothing else re-runs the catalog effect
// in that case, so without this the viewer would sit offline forever on a connection that is still up.
// Bounded, so a broadcast that never comes back settles into "offline" instead of retrying forever.
const CATALOG_RETRY_INITIAL_MS = 500;
const CATALOG_RETRY_MULTIPLIER = 2;
const CATALOG_RETRY_MAX_MS = 5000;
const CATALOG_RETRY_ATTEMPTS = 6;

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
	active: Signal<Moq.Broadcast.Consumer | undefined>;

	// The effective catalog: the fetched one, or a copy of input.catalog in manual mode.
	catalog: Signal<Catalog.Root | undefined>;
};

// A catalog source that (optionally) reloads automatically when live/offline.
export class Broadcast {
	readonly input: Readonlys<BroadcastInput>;

	readonly #output: BroadcastOutput = {
		status: new Signal<Status>("offline"),
		active: new Signal<Moq.Broadcast.Consumer | undefined>(undefined),
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

	// Bumped by the retry timer to re-run #runCatalog. A reset catalog stream changes none of the signals
	// that effect otherwise reads, so this is the only thing that can reopen the subscription.
	readonly #catalogRetry = new Signal(0);

	// Backoff state for #catalogRetry, reset on every catalog update and whenever we start reading a
	// different broadcast (a reconnect, or a same-name republish), so each of those starts with a full budget.
	#catalogDelay = CATALOG_RETRY_INITIAL_MS;
	#catalogAttempts = 0;
	#catalogSource?: Moq.Broadcast.Consumer;

	signals = new Effect();

	constructor(
		props?: Inputs<BroadcastInput> & {
			announced?: Getter<Set<Moq.Path.Valid>>;
			/** Per-path announce generations from the connection; a bump re-runs the broadcast so a same-name republish is re-consumed. Pairs with `announced`. */
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
		const name = effect.get(this.input.name);
		this.#announcedNow.set(this.#announcedGeneration(effect, name));
	}

	// The announce generation for `name`: 0 when it is not announced, otherwise a counter that bumps
	// on every re-announce. The bump is what makes a same-name republish (a new publisher instance)
	// re-consume, even when the unannounce+announce coalesce so the presence Set never observably
	// flips. Returns 1 when the check is skipped (reload is off, no announced set is wired in, or the
	// relay doesn't support announcements). Used by both `#runAnnouncedNow` (for `input.name`) and
	// `relativeBroadcast` (for cross-broadcast refs, which reruns per rendition).
	#announcedGeneration(effect: Effect, name: Moq.Path.Valid): number {
		const reload = effect.get(this.input.reload);
		if (!reload) return 1;

		// Without an announced set to consult, subscribe immediately.
		if (!this.#announced) return 1;

		// Cloudflare's relay does not yet support announcement subscriptions,
		// so default to subscribing immediately instead of waiting forever. This runs in a
		// per-rendition reactive path, so warn at most once per connection instead of on
		// every re-evaluation.
		const conn = effect.get(this.input.connection);
		if (conn?.url.hostname.endsWith("mediaoverquic.com")) {
			if (!warnedNoDiscovery.has(conn)) {
				warnedNoDiscovery.add(conn);
				console.warn("Cloudflare relay does not support broadcast discovery yet; ignoring reload signal.");
			}
			return 1;
		}

		const announced = effect.get(this.#announced);
		if (!announced.has(name)) return 0;

		const gen = this.#announcedGenerations ? effect.get(this.#announcedGenerations).get(name) : undefined;
		return gen ?? 1;
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
		// The retry timer bumps this to reopen a subscription that ended; see the tail of the spawn below.
		effect.get(this.#catalogRetry);

		// Every bail-out below reports offline. A retry leaves the status at "loading" while it waits, so a
		// rerun that finds nothing left to subscribe to has to settle it, or the tile spins forever.
		const enabled = effect.get(this.input.enabled);
		if (!enabled) {
			this.#output.status.set("offline");
			return;
		}

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
		if (!broadcast) {
			this.#output.status.set("offline");
			return;
		}

		// A retry can land after the broadcast itself closed (the session dropped, or the publisher went
		// away). Subscribing to a closed broadcast throws, and #runBroadcast will replace `output.active`
		// once it observes the same thing, so stop here.
		if (broadcast.closedSignal.peek()) {
			this.#output.status.set("offline");
			return;
		}

		// Reading a different broadcast is a fresh start, not a continuation of the previous failures.
		if (this.#catalogSource !== broadcast) {
			this.#catalogSource = broadcast;
			this.#resetCatalogBackoff();
		}

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
			// Pin this run's signal: the getter is swapped for a fresh one the moment a rerun starts, and
			// a rerun starts before it awaits this spawn.
			const abort = effect.abort;

			let failure: unknown;
			try {
				for (;;) {
					const update = await Promise.race([effect.cancel, fetchNext()]);
					if (!update) break;

					console.debug("received catalog", format, this.input.name.peek(), update);

					this.#resetCatalogBackoff();
					this.#output.catalog.set(update);
					this.#output.status.set("live");
				}
			} catch (err) {
				failure = err;
				// A routine transport reset during a publisher handover is expected; a real fetch/parse
				// failure (auth, not-found, protocol, or schema validation) still warns.
				console[isStreamAbort(err) ? "debug" : "warn"]("error fetching catalog", this.input.name.peek(), err);
			}

			this.#output.catalog.set(undefined);

			// Torn down (disabled, renamed, reconnected, or closed). Whatever replaces this run owns the
			// status from here, and arming a timer now would leak it into that run.
			if (abort.aborted) {
				this.#output.status.set("offline");
				return;
			}

			// Only a stream reset is worth retrying: the subscription was killed under us (a slow-consumer
			// drop, a relay bounce, a publisher handover) while the broadcast is still announced on a live
			// connection, and nothing else would ever reopen it. A clean end means the publisher stopped,
			// and a coded fault (auth, not-found, protocol, unroutable) fails identically every time, so
			// both report offline straight away rather than stalling the badge behind a retry ladder.
			const reset = failure !== undefined && isStreamAbort(failure);
			if (!reset || this.#catalogAttempts >= CATALOG_RETRY_ATTEMPTS) {
				this.#output.status.set("offline");
				return;
			}

			// The timer is registered on the effect, so a rerun (handover, reconnect, close) cancels it
			// instead of racing a second subscription against it.
			this.#catalogAttempts += 1;
			this.#output.status.set("loading");

			effect.timer(() => this.#catalogRetry.update((n) => n + 1), this.#catalogDelay);
			this.#catalogDelay = Math.min(this.#catalogDelay * CATALOG_RETRY_MULTIPLIER, CATALOG_RETRY_MAX_MS);
		});
	}

	#resetCatalogBackoff(): void {
		this.#catalogDelay = CATALOG_RETRY_INITIAL_MS;
		this.#catalogAttempts = 0;
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
		if (!rel) return effect.get(this.output.active);

		const base = effect.get(this.input.name);
		const resolved = Path.resolve(base, rel);

		// A reference that walks back to the catalog's own broadcast (or resolves to
		// the empty root, via excess `..`) is served by the catalog broadcast itself,
		// avoiding a duplicate subscription on the same path.
		if (resolved === base || resolved === Path.empty()) return effect.get(this.output.active);

		if (!effect.get(this.input.enabled)) return undefined;

		const conn = effect.get(this.input.connection);
		if (!conn) return undefined;

		if (!this.#announcedGeneration(effect, resolved)) return undefined;

		const broadcast = conn.consume(resolved);
		effect.cleanup(() => broadcast.close());
		return broadcast;
	}

	close() {
		this.signals.close();
	}
}
