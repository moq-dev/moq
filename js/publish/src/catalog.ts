import { Instant, Milli, Nano, now, origin } from "@moq/clock";
import * as Catalog from "@moq/hang/catalog";
import * as Json from "@moq/json";
import type * as Moq from "@moq/net";
import { Time } from "@moq/net";
import { Effect } from "@moq/signals";

// Estimate-only edits publish at most this often.
const ESTIMATE_INTERVAL = Nano.fromMilli(Milli(1000));

/**
 * A stable catalog producer that fans out to one or more network tracks.
 *
 * Unlike a raw track producer, this exists independently of any subscription: edit it at any time
 * with {@link mutate}, and each subscriber (including a relay that reconnects) is seeded with the
 * current catalog before receiving updates. Independent owners (the base `video`/`audio` and an
 * application's own sections, e.g. `scte35`) each edit only their own keys, so their sections
 * compose instead of clobbering one another.
 *
 * The root `clock` is advertised from the first snapshot: every js/publish timestamp is the
 * installed clock's instant in microseconds, so PTS zero is the wall time of instant zero. The
 * mapping is fixed when the producer is created, so a later system-clock adjustment never retimes
 * the broadcast. A page whose wall clock reads before 2020 has not set it yet (a device booted
 * before it synced), so it omits `clock` rather than advertise a false one.
 */
export class CatalogProducer {
	#value: Catalog.Root = pageRoot();
	#outputs = new Set<Json.Snapshot.Producer<Catalog.Root>>();
	#lastEstimate: Instant | undefined;
	#pending: Effect | undefined;

	/** Edit the catalog; estimate-only changes publish at most once a second with a trailing update. */
	mutate(fn: (catalog: Catalog.Root) => void): void {
		const draft = structuredClone(this.#value);
		fn(draft);
		// Every consumer refuses a catalog that fails this, so refuse the edit before it reaches one.
		const value = Catalog.encode(draft);
		// Estimates only rise for a track; a renamed rendition starts its new track over.
		for (const field of ["jitter", "delay"] as const) {
			const previous = new Map(advertised(this.#value, field));
			for (const [track, estimate] of advertised(value, field)) {
				if (estimate === 0) throw new Error(`omit ${field} rather than advertising 0`);
				const before = previous.get(track);
				if (before !== undefined && (estimate === undefined || estimate < before)) {
					throw new Error(`${field} cannot decrease for an existing track`);
				}
			}
		}
		const estimateOnly = Json.deepEqual(structure(this.#value), structure(value));
		const changed = !Json.deepEqual(this.#value, value);
		this.#value = value;
		if (!changed || this.#outputs.size === 0) return;

		const at = now();
		const next = this.#lastEstimate === undefined ? undefined : Instant.add(this.#lastEstimate, ESTIMATE_INTERVAL);
		if (!estimateOnly || next === undefined || at >= next) {
			this.#publish(estimateOnly || this.#pending !== undefined);
		} else if (!this.#pending) {
			this.#pending = new Effect();
			this.#pending.timer(() => this.#publish(true), Instant.durationSince(next, at));
		}
	}

	#publish(estimate: boolean): void {
		this.#pending?.close();
		this.#pending = undefined;
		if (estimate) this.#lastEstimate = now();
		const at = Time.Timestamp.now();
		for (const output of this.#outputs) output.update({ value: this.#value, at });
	}

	/**
	 * Serve a track: seed it with the current catalog, then forward updates.
	 *
	 * Create the track with `compress: true`: catalog JSON is small and repetitive.
	 */
	serve(track: Moq.Track.Producer, effect: Effect): void {
		const output = new Json.Snapshot.Producer<Catalog.Root>({ track, deltas: false });
		output.update({ value: this.#value, at: Time.Timestamp.now() });

		this.#outputs.add(output);
		effect.cleanup(() => {
			this.#outputs.delete(output);
			output.finish();
			if (this.#outputs.size === 0) {
				this.#pending?.close();
				this.#pending = undefined;
				this.#lastEstimate = undefined;
			}
		});
	}
}

// Strip only the schema's estimate fields, so config edits and arbitrary extension sections remain
// immediate even when an extension happens to contain a property named jitter or delay.
function structure(catalog: Catalog.Root): Catalog.Root {
	const value = structuredClone(catalog);
	for (const tracks of [
		value.audio?.renditions,
		value.video?.renditions,
		value.text?.renditions,
		value.json?.tracks,
		value.binary?.tracks,
	]) {
		for (const config of Object.values(tracks ?? {})) {
			delete config.jitter;
			delete config.delay;
		}
	}
	return value;
}

type Advertised = { broadcast?: string; track?: string; jitter?: number; delay?: number };

/** Every entry's advertised `field`, keyed by its section, ID, broadcast, and track name. */
function advertised(catalog: Catalog.Root, field: "jitter" | "delay"): [string, number | undefined][] {
	const sections = {
		audio: catalog.audio?.renditions,
		video: catalog.video?.renditions,
		text: catalog.text?.renditions,
		json: catalog.json?.tracks,
		binary: catalog.binary?.tracks,
	};
	return Object.entries(sections).flatMap(([section, tracks]) =>
		Object.entries((tracks ?? {}) as Record<string, Advertised>).map(
			([id, config]): [string, number | undefined] => [
				JSON.stringify([section, id, config.broadcast, Catalog.track(id, config)]),
				config[field],
			],
		),
	);
}

// An empty root advertising the wall time of instant zero, the zero every js/publish timestamp
// counts from, when the wall clock is set.
function pageRoot(): Catalog.Root {
	const at = Math.round((origin() - Catalog.MOQ_EPOCH_UNIX_MILLIS) * 1000);
	if (at < 0) return {};
	return { clock: { wall: Catalog.u53(at), timescale: 1_000_000 } };
}
