import type * as Moq from "@moq/lite";
import { type Effect, type Getter, Signal } from "@moq/signals";
import type { z } from "zod/mini";
import type { Section } from "./section";

/// A catalog reader that provides per-section change notifications.
///
/// Sections are registered with a name and Zod schema. When the catalog track
/// receives a new frame, JSON is parsed and each registered section is validated
/// and updated. Signal equality checking ensures subscribers only fire when
/// their specific section's value actually changed.
export class CatalogReader {
	// biome-ignore lint/suspicious/noExplicitAny: we store heterogeneous section types
	#sections = new Map<string, { schema: z.ZodMiniType<any>; signal: Signal<any> }>();

	/// Register interest in a section. Returns a Getter<T | undefined>.
	///
	/// The getter updates when the catalog is re-fetched and this section's value differs.
	section<T>(def: Section<T>): Getter<T | undefined> {
		const existing = this.#sections.get(def.name);
		if (existing) return existing.signal as Getter<T | undefined>;

		const signal = new Signal<T | undefined>(undefined);
		this.#sections.set(def.name, { schema: def.schema, signal });
		return signal;
	}

	/// Start consuming from a MoQ track.
	///
	/// Spawns an async loop that reads frames, parses JSON, and updates
	/// per-section signals. Unregistered keys in the JSON are ignored.
	consume(track: Moq.Track, effect: Effect): void {
		effect.spawn(async () => {
			try {
				for (;;) {
					const frame = await Promise.race([effect.cancel, track.readFrame()]);
					if (!frame) break;

					const decoder = new TextDecoder();
					const str = decoder.decode(frame);
					const json = JSON.parse(str);

					for (const [name, { schema, signal }] of this.#sections) {
						const raw = json[name];
						if (raw !== undefined) {
							try {
								const parsed = schema.parse(raw);
								signal.set(parsed);
							} catch (err) {
								console.warn(`invalid catalog section "${name}"`, err);
								signal.set(undefined);
							}
						} else {
							signal.set(undefined);
						}
					}
				}
			} catch (err) {
				console.warn("error reading catalog", err);
			} finally {
				// Clear all sections when the track ends
				for (const { signal } of this.#sections.values()) {
					signal.set(undefined);
				}
			}
		});
	}
}
