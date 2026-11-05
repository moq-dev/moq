import type { Milli } from "@moq/clock";
import type * as Moq from "@moq/net";
import { type Getter, Signal } from "@moq/signals";
import type { Slot } from "./publication";

/** The media kind of a rendition, selecting its catalog section (`video`, `audio`, or `text`). */
export type Kind = "video" | "audio" | "text";

/**
 * What a rendition's producer has sent, as the broadcast's stats track reports it. Counters are
 * cumulative and a missing one means zero; `bitrate` is a gauge, absent when unknown.
 */
export interface RenditionStats {
	/** Media frames sent. */
	frames?: number;
	/** Media payload bytes sent. */
	bytes?: number;
	/** Keyframes sent. */
	keyframes?: number;
	/** Frames skipped before sending, such as those an overloaded encoder had no room for. */
	skipped?: number;
	/** The latest target encoder bitrate in bits per second. */
	bitrate?: number;
}

/** @internal Register a handle whose track and teardown belong to its broadcast. */
export let makeRendition: <Config>(
	name: string,
	kind: Kind,
	track: Getter<Moq.Track.Producer | undefined>,
	slot: Getter<Slot<Config> | undefined>,
	close: () => void,
) => Rendition<Config>;

/**
 * A registered rendition track on a {@link Broadcast}: the catalog slot plus its track.
 *
 * Constructed only by {@link Broadcast} via `broadcast.video(name)` / `broadcast.audio(name)`; the
 * producer (usually an encoder) writes {@link config} and encodes into {@link track} while it's set.
 */
export class Rendition<Config> {
	/** The stable catalog ID and initial track name, e.g. `"video/hd"`. */
	readonly name: string;

	/** Which catalog section this rendition lands in. */
	readonly kind: Kind;

	/**
	 * The catalog entry for this rendition, written by its producer (usually an encoder).
	 * `undefined` omits it from the catalog (e.g. while disabled). Changing a field a decoder must
	 * reinitialize for (codec, description, container, sample rate, channel count, or display aspect)
	 * ends the current track and serves the rendition under a fresh catalog `track` name, so a name
	 * always means the same content. Every other field updates the current track in place. The
	 * broadcast owns `broadcast` and `track`, so a config setting either is refused.
	 */
	readonly config = new Signal<Config | undefined>(undefined);

	/**
	 * The longest a video or audio rendition's tracks run between group starts outside a pause, when
	 * known: a video encoder's keyframe interval, or an audio encoder's group duration. Undefined, or
	 * not a positive duration, contributes nothing.
	 *
	 * When the broadcast first lists a video or audio rendition in a network broadcast, it declares
	 * the longest of these as the catalog's `media.join.interval`, rounded up to whole milliseconds,
	 * so an HLS or DASH export can cut its segments before any media is encoded. That declaration
	 * is fixed for the network broadcast, so a later change here does not move it.
	 */
	readonly joinInterval = new Signal<Milli | undefined>(undefined);

	/**
	 * What the producer has sent under this rendition, written by the producer and reported under
	 * its ID on the broadcast's stats track. `undefined` leaves the rendition out of the report.
	 */
	readonly stats = new Signal<RenditionStats | undefined>(undefined);

	/**
	 * The open track producer, or `undefined` while the network broadcast is down, after the track
	 * closes, and, for non-audio tracks with the broadcast's `onDemand`, while no subscriber is attached.
	 * Producers should
	 * encode only while this is set, and only media matching the current {@link config}: a new
	 * identity swaps in a new track.
	 */
	readonly track: Getter<Moq.Track.Producer | undefined>;

	/**
	 * @internal What the producer keeps for this rendition in the current network broadcast, or
	 * `undefined` while there is none. Setting its `error` ends the track.
	 */
	readonly slot: Getter<Slot<Config> | undefined>;

	readonly #close: () => void;
	#closed = false;

	private constructor(
		name: string,
		kind: Kind,
		track: Getter<Moq.Track.Producer | undefined>,
		slot: Getter<Slot<Config> | undefined>,
		close: () => void,
	) {
		this.name = name;
		this.kind = kind;
		this.track = track;
		this.slot = slot;
		this.#close = close;
	}

	static {
		makeRendition = (name, kind, track, slot, close) => new Rendition(name, kind, track, slot, close);
	}

	/**
	 * Unregister: removes the catalog entry and detaches the track. The broadcast keeps the
	 * network track open, so registering the name again continues it. A closed track is
	 * replaced under a fresh catalog `track` name instead. Idempotent.
	 */
	close(): void {
		if (this.#closed) return;
		this.#closed = true;
		this.#close();
	}
}
