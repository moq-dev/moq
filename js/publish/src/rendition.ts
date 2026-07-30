import type * as Moq from "@moq/net";
import { type Getter, Signal } from "@moq/signals";

/** The media kind of a rendition, selecting its catalog section (`video` or `audio`). */
export type Kind = "video" | "audio";

/**
 * A registered rendition track on a {@link Broadcast}: the catalog slot plus the demand-gated track.
 *
 * Constructed only by {@link Broadcast} via `broadcast.video(name)` / `broadcast.audio(name)`; the
 * producer (usually an encoder) writes {@link config} and encodes into {@link track} while it's set.
 */
export class Rendition<Config> {
	/** The full track name, e.g. `"video/hd"`. */
	readonly name: string;

	/** Which catalog section this rendition lands in. */
	readonly kind: Kind;

	/**
	 * The catalog entry for this rendition, written by its producer (usually an encoder).
	 * `undefined` omits it from the catalog (e.g. while disabled).
	 */
	readonly config = new Signal<Config | undefined>(undefined);

	/**
	 * The live track producer while a subscriber is attached, `undefined` otherwise.
	 * Producers should encode only while this is set (the demand gate).
	 */
	readonly track: Getter<Moq.Track.Producer | undefined>;

	readonly #close: () => void;
	#closed = false;

	/** @internal Constructed by {@link Broadcast}; `track` and `close` are owned by it. */
	constructor(name: string, kind: Kind, track: Getter<Moq.Track.Producer | undefined>, close: () => void) {
		this.name = name;
		this.kind = kind;
		this.track = track;
		this.#close = close;
	}

	/** Unregister: removes the catalog entry and closes any active track. Idempotent. */
	close(): void {
		if (this.#closed) return;
		this.#closed = true;
		this.#close();
	}
}
