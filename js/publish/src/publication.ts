import * as Json from "@moq/json";
import type * as Moq from "@moq/net";
import { type Getter, Signal } from "@moq/signals";
import type { Kind, Rendition } from "./rendition";

/** @internal One catalog ID's rendition in a network broadcast. */
export type Entry = {
	readonly id: string;
	readonly kind: Kind;
	// The track name the catalog lists: the ID itself until new media or a replacement renames it.
	name: string;
	// The network track serving it. Kept after its rendition leaves, so registering the ID again
	// continues it.
	track?: Moq.Track.Producer;
	// The registration that last served `track`.
	owner?: Rendition<unknown>;
};

/** @internal What the catalog lists for a rendition: its kind and its config's media identity. */
export type Listing = { kind: Kind; identity: Identity };

/**
 * @internal The renditions of one network broadcast, keyed by catalog ID. A new network broadcast
 * starts a new publication, so every name, kind, and track starts over with it.
 */
export class Publication {
	// Each catalog ID that holds or held a rendition here.
	readonly #entries = new Map<string, Entry>();

	// Every track name handed out here, with the catalog ID owning it and the identity it was first
	// listed with (none for a replacement never listed). Kept after the ID leaves, so a name always
	// means the same content: an encoder may cache groups on a track before anyone can find it, and
	// a late subscriber or a fetch reads them under the catalog's config.
	readonly #names = new Map<string, { id: string; identity?: Identity }>();

	readonly #renames = new Signal(0);

	/** Bumped on every rename, so the catalog fold and the track loop pick up the new name. */
	readonly renames: Getter<number> = this.#renames;

	// Names outside this publication that a minted name must also avoid.
	readonly #reserved: () => Iterable<string>;

	/**
	 * Seeded with the latest catalog fold, so from here a name keeps the identity it is first listed
	 * with. `reserved` lists the other names a replacement must not take.
	 */
	constructor(listed: Map<string, Listing>, reserved: () => Iterable<string>) {
		this.#reserved = reserved;
		for (const [id, { kind, identity }] of listed) {
			this.#entries.set(id, { id, kind, name: id });
			this.#names.set(id, { id, identity });
		}
	}

	/** Throw unless `id` may register as `kind`. */
	check(id: string, kind: Kind): void {
		// An ID keeps its track, so it keeps its kind: different media takes a different ID.
		const previous = this.#entries.get(id)?.kind;
		if (previous !== undefined && previous !== kind) {
			throw new Error(`rendition ${id} was registered as ${previous}, not ${kind}`);
		}
		// A replacement track holds or held the name, so another rendition's media owns it.
		const owner = this.#names.get(id)?.id;
		if (owner !== undefined && owner !== id) {
			throw new Error(`rendition ${id} names another rendition's replacement track`);
		}
	}

	/** The entry for `rendition`'s catalog ID, created on first sight. */
	entry(rendition: Rendition<unknown>): Entry {
		let entry = this.#entries.get(rendition.name);
		if (!entry) {
			entry = { id: rendition.name, kind: rendition.kind, name: rendition.name };
			this.#entries.set(rendition.name, entry);
			this.#names.set(rendition.name, { id: rendition.name });
		}
		return entry;
	}

	/**
	 * The track name `rendition` would serve its config's `identity` under: its current one, or a
	 * fresh one for a new identity. Everything else updates the served track in place. Changes
	 * nothing until {@link commit}, so a catalog edit that fails leaves the tracks alone.
	 */
	name(rendition: Rendition<unknown>, identity: Identity): string {
		const current = this.#entries.get(rendition.name)?.name ?? rendition.name;
		const listed = this.#names.get(current)?.identity;
		if (listed && !Json.deepEqual(listed, identity)) return this.#mint(rendition.name);
		return current;
	}

	/** Record that the catalog lists `rendition` under `name` with `identity`. */
	commit(rendition: Rendition<unknown>, name: string, identity: Identity): void {
		const entry = this.entry(rendition);
		if (entry.name !== name) this.#move(entry, name);
		const listed = this.#names.get(name);
		if (listed && !listed.identity) listed.identity = identity;
	}

	/**
	 * Move `entry` to a fresh track name, such as for a replacement registration after its track
	 * closed. The track loop ends its old track, so that track's subscribers follow the catalog.
	 */
	rename(entry: Entry): void {
		this.#move(entry, this.#mint(entry.id));
	}

	#move(entry: Entry, name: string): void {
		this.#names.set(name, { id: entry.id });
		entry.name = name;
		this.#renames.update((n) => n + 1);
	}

	// The next unused `<id>/<n>` track name. This publisher owns the namespace and reserves `<id>/` for
	// the rendition (see `Broadcast.net`), so skipping every name handed out here and every reserved
	// one is enough to never reuse one.
	#mint(id: string): string {
		const taken = new Set([...this.#names.keys(), ...this.#entries.keys(), ...this.#reserved()]);
		for (let n = 1; ; n++) {
			const name = `${id}/${n}`;
			if (!taken.has(name)) return name;
		}
	}

	/** End every track. */
	close(): void {
		for (const { track } of this.#entries.values()) track?.finish();
		for (const { track } of this.#entries.values()) track?.close();
	}
}

/**
 * The fields a consumer can't change without reinitializing its decoder (or demuxer, for
 * `container`), so changing any of them takes a new track name. Mirrors hang's
 * `VideoConfig::same_identity` and `AudioConfig::same_identity`.
 */
export type Identity = Record<string, unknown>;

const IDENTITY: Record<Kind, readonly string[]> = {
	video: ["codec", "description", "container", "displayAspectWidth", "displayAspectHeight", "interlaced"],
	audio: ["codec", "description", "container", "sampleRate", "numberOfChannels"],
	text: ["container"],
};

/**
 * @internal The identity of a `kind` config. Cloned, so a producer mutating a config it shares (such
 * as its container) can't rewrite what a name was served with.
 */
export function identity(kind: Kind, config: unknown): Identity {
	const source = config as Record<string, unknown>;
	const fields: Identity = {};
	for (const field of IDENTITY[kind]) fields[field] = source[field];
	return structuredClone(fields);
}
