import type * as Moq from "@moq/lite";
import { type Effect, Signal } from "@moq/signals";
import type { Section } from "./section";

/// A catalog writer that manages typed sections and serializes them to a MoQ track.
///
/// Each section is a Signal that can be set independently. When served on a track,
/// changes are reactively detected and the full catalog JSON is re-serialized.
/// Microtask coalescing in Signal means multiple set() calls in the same tick
/// produce a single write.
export class CatalogWriter {
	// biome-ignore lint/suspicious/noExplicitAny: we store heterogeneous section types
	#sections = new Map<string, { signal: Signal<any> }>();

	/// Register a section for writing. Returns a Signal<T | undefined> for read+write.
	section<T>(def: Section<T>): Signal<T | undefined> {
		const existing = this.#sections.get(def.name);
		if (existing) return existing.signal as Signal<T | undefined>;

		const signal = new Signal<T | undefined>(undefined);
		this.#sections.set(def.name, { signal });
		return signal;
	}

	/// Serve the catalog on a MoQ track.
	///
	/// Uses Effect to reactively subscribe to all registered section signals.
	/// When any signal changes, re-serializes and writes a new frame.
	serve(track: Moq.Track, effect: Effect): void {
		effect.run((inner) => {
			const obj: Record<string, unknown> = {};

			for (const [name, { signal }] of this.#sections) {
				const value = inner.get(signal);
				if (value !== undefined) {
					obj[name] = value;
				}
			}

			const encoder = new TextEncoder();
			track.writeFrame(encoder.encode(JSON.stringify(obj)));
		});
	}
}
