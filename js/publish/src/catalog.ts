import type * as Catalog from "@moq/hang/catalog";
import * as Json from "@moq/json";
import type * as Moq from "@moq/net";
import type { Effect } from "@moq/signals";

/** A closeable reservation that gates the first catalog snapshot. */
export interface CatalogReservation {
	/** Create another reservation on the same catalog gate. */
	clone(): CatalogReservation;

	/** Reserve a video rendition by track name until its config is set or the guard is closed. */
	video(name: string): ReservedRendition<Catalog.VideoConfig>;

	/** Reserve an audio rendition by track name until its config is set or the guard is closed. */
	audio(name: string): ReservedRendition<Catalog.AudioConfig>;

	/** Release this reservation. */
	close(): void;
}

/** A reserved catalog rendition that writes its config when it becomes known. */
export interface ReservedRendition<T> {
	/** The track name this rendition occupies in the catalog. */
	readonly name: string;

	/** Insert or replace the rendition config, releasing its initial catalog gate. */
	set(config: T): void;

	/** Edit the rendition config in place if it has already been set. */
	update(fn: (config: T) => void): void;

	/** Remove the rendition if present and release its initial catalog gate. */
	close(): void;
}

type RenditionOps<T> = {
	insert(catalog: Catalog.Root, name: string, config: T): void;
	update(catalog: Catalog.Root, name: string, fn: (config: T) => void): void;
	remove(catalog: Catalog.Root, name: string): void;
};

/**
 * A stable catalog producer that fans out to on-demand subscription tracks.
 *
 * Unlike a raw track producer, this exists independently of any subscription: edit it at any time
 * with {@link mutate}, and each subscriber (including a relay that reconnects) is seeded with the
 * current catalog before receiving updates. Independent owners (the base `video`/`audio` and an
 * application's own sections, e.g. `scte35`) each edit only their own keys, so their sections
 * compose instead of clobbering one another. Use {@link reserve} when the first snapshot should wait
 * for an initial set of async track configs to resolve.
 */
export class CatalogProducer {
	#value: Catalog.Root = {};
	#outputs = new Set<Json.Producer<Catalog.Root>>();
	#reservations = 0;
	#pending = false;
	#published = false;
	#reserved = false;

	/** Edit the catalog in place; the result is published to all current subscribers. */
	mutate(fn: (catalog: Catalog.Root) => void): void {
		const value = structuredClone(this.#value);
		fn(value);
		this.#value = value;
		this.#publish();
	}

	/**
	 * Gate the first catalog snapshot until the returned reservation is closed.
	 *
	 * Callers can clone the reservation, or reserve individual audio/video renditions from it, while
	 * discovering the initial track set. Mutations made while the initial gate is open are buffered and
	 * published as one complete snapshot when the last reservation closes. If no mutation was made, no
	 * empty catalog is emitted.
	 */
	reserve(): CatalogReservation {
		this.#reserved = true;
		this.#reservations += 1;
		return new Reservation(this, () => this.#release());
	}

	#publish(): void {
		if (!this.#published && this.#reservations > 0) {
			this.#pending = true;
			return;
		}

		this.#pending = false;
		this.#published = true;
		for (const output of this.#outputs) output.update(this.#value);
	}

	/**
	 * Serve a subscription request: seed it with the current catalog, then forward updates.
	 *
	 * Pass `opts.compression` to DEFLATE-compress this subscriber's frames, so the same catalog can be
	 * served both plaintext and compressed (e.g. `catalog.json` and `catalog.json.z`).
	 */
	serve(track: Moq.TrackProducer, effect: Effect, opts?: { compression?: boolean }): void {
		const output = new Json.Producer<Catalog.Root>(track, {
			compression: opts?.compression,
			deltaRatio: 0,
		});
		if (this.#published || !this.#reserved) {
			this.#published = true;
			output.update(this.#value);
		}

		this.#outputs.add(output);
		effect.cleanup(() => {
			this.#outputs.delete(output);
			output.finish();
		});
	}

	#release(): void {
		this.#reservations = Math.max(0, this.#reservations - 1);
		if (this.#published || this.#reservations !== 0 || !this.#pending) return;

		this.#publish();
	}
}

class Reservation implements CatalogReservation {
	#catalog: CatalogProducer;
	#release: () => void;
	#closed = false;

	constructor(catalog: CatalogProducer, release: () => void) {
		this.#catalog = catalog;
		this.#release = release;
	}

	clone(): CatalogReservation {
		if (this.#closed) throw new Error("reservation is closed");
		return this.#catalog.reserve();
	}

	video(name: string): ReservedRendition<Catalog.VideoConfig> {
		return new Rendition(name, this.#catalog, this.clone(), VIDEO_OPS);
	}

	audio(name: string): ReservedRendition<Catalog.AudioConfig> {
		return new Rendition(name, this.#catalog, this.clone(), AUDIO_OPS);
	}

	close(): void {
		if (this.#closed) return;
		this.#closed = true;
		this.#release();
	}
}

class Rendition<T> implements ReservedRendition<T> {
	readonly name: string;
	#catalog: CatalogProducer;
	#gate: CatalogReservation | undefined;
	#ops: RenditionOps<T>;
	#present = false;

	constructor(name: string, catalog: CatalogProducer, gate: CatalogReservation, ops: RenditionOps<T>) {
		this.name = name;
		this.#catalog = catalog;
		this.#gate = gate;
		this.#ops = ops;
	}

	set(config: T): void {
		this.#catalog.mutate((catalog) => this.#ops.insert(catalog, this.name, config));
		this.#present = true;
		this.#gate?.close();
		this.#gate = undefined;
	}

	update(fn: (config: T) => void): void {
		if (!this.#present) return;
		this.#catalog.mutate((catalog) => this.#ops.update(catalog, this.name, fn));
	}

	close(): void {
		if (this.#present) {
			this.#catalog.mutate((catalog) => this.#ops.remove(catalog, this.name));
			this.#present = false;
		}
		this.#gate?.close();
		this.#gate = undefined;
	}
}

type VideoSection = {
	renditions: Record<string, Catalog.VideoConfig>;
	display?: { width: number; height: number };
	rotation?: number;
	flip?: boolean;
};

type AudioSection = {
	renditions: Record<string, Catalog.AudioConfig>;
};

const VIDEO_OPS: RenditionOps<Catalog.VideoConfig> = {
	insert(catalog, name, config) {
		videoSection(catalog).renditions[name] = config;
	},
	update(catalog, name, fn) {
		const video = currentVideo(catalog);
		const config = video?.renditions[name];
		if (config) fn(config);
	},
	remove(catalog, name) {
		const video = currentVideo(catalog);
		if (!video) return;

		delete video.renditions[name];
		if (sectionIsEmpty(video)) delete catalog.video;
	},
};

const AUDIO_OPS: RenditionOps<Catalog.AudioConfig> = {
	insert(catalog, name, config) {
		audioSection(catalog).renditions[name] = config;
	},
	update(catalog, name, fn) {
		const audio = currentAudio(catalog);
		const config = audio?.renditions[name];
		if (config) fn(config);
	},
	remove(catalog, name) {
		const audio = currentAudio(catalog);
		if (!audio) return;

		delete audio.renditions[name];
		if (sectionIsEmpty(audio)) delete catalog.audio;
	},
};

function videoSection(catalog: Catalog.Root): VideoSection {
	const current = currentVideo(catalog);
	if (current) return current;

	const video: VideoSection = { renditions: {} };
	catalog.video = video as Catalog.Video;
	return video;
}

function currentVideo(catalog: Catalog.Root): VideoSection | undefined {
	if (!catalog.video || !("renditions" in catalog.video)) return undefined;
	return catalog.video as VideoSection;
}

function audioSection(catalog: Catalog.Root): AudioSection {
	const current = currentAudio(catalog);
	if (current) return current;

	const audio: AudioSection = { renditions: {} };
	catalog.audio = audio as Catalog.Audio;
	return audio;
}

function currentAudio(catalog: Catalog.Root): AudioSection | undefined {
	if (!catalog.audio || !("renditions" in catalog.audio)) return undefined;
	return catalog.audio as AudioSection;
}

/**
 * Whether a media section has nothing left worth keeping: no renditions and no other field carrying
 * a value. `structuredClone` preserves keys whose value is `undefined`, so this inspects values
 * rather than keys, and stays correct if the section gains fields later (e.g. video display or
 * rotation): any defined field keeps the section alive.
 */
function sectionIsEmpty(section: { renditions: Record<string, unknown> }): boolean {
	if (Object.keys(section.renditions).length > 0) return false;
	return Object.entries(section).every(([key, value]) => key === "renditions" || value === undefined);
}
