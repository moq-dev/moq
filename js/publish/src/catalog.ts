import * as Catalog from "@moq/hang/catalog";
import * as Json from "@moq/json";
import type * as Moq from "@moq/net";
import type { Effect } from "@moq/signals";

/**
 * A stable catalog producer that fans out to one or more network tracks.
 *
 * Unlike a raw track producer, this exists independently of any subscription: edit it at any time
 * with {@link mutate}, and each subscriber (including a relay that reconnects) is seeded with the
 * current catalog before receiving updates. Independent owners (the base `video`/`audio` and an
 * application's own sections, e.g. `scte35`) each edit only their own keys, so their sections
 * compose instead of clobbering one another.
 *
 * The root `clock` is advertised from the first snapshot: every js/publish timestamp is
 * `performance.now()` in microseconds, so PTS zero is `performance.timeOrigin`. The mapping is fixed
 * for the page, so a system-clock adjustment never retimes the broadcast.
 */
export class CatalogProducer {
	#value: Catalog.Root = { clock: pageClock() };
	#outputs = new Set<Json.Snapshot.Producer<Catalog.Root>>();

	/** Edit the catalog in place; the result is published to all current subscribers. */
	mutate(fn: (catalog: Catalog.Root) => void): void {
		const value = structuredClone(this.#value);
		fn(value);
		for (const field of ["jitter", "delay"] as const) {
			const previous = advertised(this.#value, field);
			for (const [section, next] of Object.entries(advertised(value, field))) {
				for (const [name, estimate] of Object.entries(next)) {
					if (estimate === 0) throw new Error(`omit ${field} rather than advertising 0`);
					const before = previous[section]?.[name];
					if (before !== undefined && (estimate === undefined || estimate < before)) {
						throw new Error(`${field} cannot decrease for an existing track`);
					}
				}
			}
		}
		this.#value = value;
		for (const output of this.#outputs) output.update(value);
	}

	/**
	 * Serve a track: seed it with the current catalog, then forward updates.
	 *
	 * Pass `opts.compression` to DEFLATE-compress this subscriber's frames, so the same catalog can be
	 * served both plaintext and compressed (e.g. `catalog.json` and `catalog.json.z`).
	 */
	serve(track: Moq.Track.Producer, effect: Effect, opts?: { compression?: boolean }): void {
		const output = new Json.Snapshot.Producer<Catalog.Root>({
			track,
			compression: opts?.compression ? "deflate" : "none",
			deltaRatio: 0,
		});
		output.update(this.#value);

		this.#outputs.add(output);
		effect.cleanup(() => {
			this.#outputs.delete(output);
			output.finish();
		});
	}
}

/** Every track's advertised `field`, by section and then track name. */
function advertised(
	catalog: Catalog.Root,
	field: "jitter" | "delay",
): Record<string, Record<string, number | undefined>> {
	const pick = (tracks: Record<string, { jitter?: number; delay?: number }> | undefined) =>
		Object.fromEntries(Object.entries(tracks ?? {}).map(([name, config]) => [name, config[field]]));
	return {
		audio: pick(catalog.audio?.renditions),
		video: pick(catalog.video?.renditions),
		text: pick(catalog.text?.renditions),
		json: pick(catalog.json?.tracks),
		binary: pick(catalog.binary?.tracks),
	};
}

// The wall time of `performance.now() === 0`, the zero every js/publish timestamp counts from.
function pageClock(): Catalog.Clock {
	const wall = Math.round((performance.timeOrigin - Catalog.MOQ_EPOCH_UNIX_MILLIS) * 1000);
	return { wall: Catalog.u53(wall), timescale: 1_000_000 };
}
