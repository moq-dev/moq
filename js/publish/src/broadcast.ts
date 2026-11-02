import { Milli } from "@moq/clock";
import * as Catalog from "@moq/hang/catalog";
import * as Container from "@moq/hang/container";
import * as Moq from "@moq/net";
import { Effect, type Getter, getter, type Inputs, type Readonlys, Signal } from "@moq/signals";
import { rotateVideoDimensions } from "@moq/video";
import { CatalogProducer } from "./catalog";
import { Order } from "./order";
import { type Identity, identity, type Listing, Publication, type Slot } from "./publication";
import type { Kind, Rendition } from "./rendition";
import { makeRendition } from "./rendition-internal";

// The refresh ratio video tracks advertise: a joiner forces a keyframe once a group's deltas
// outweigh twice its keyframe. Matches moq-video's capture `REFRESH`; see it for the measurements.
const VIDEO_REFRESH = 2;

/**
 * Signals the broadcast reads. Whoever owns the backing Signal (the element, or another component
 * whose output is wired in, e.g. a Video.Capture's `display`) does the writing.
 */
export type BroadcastInput = {
	// The origin to publish into. Independent of any connection: whichever sessions serve the
	// origin announce the broadcast, and it survives their reconnects.
	origin: Getter<Moq.Origin.Table | undefined>;

	// Whether to create the broadcast. Defaults to true.
	enabled: Getter<boolean>;

	// Whether to announce the broadcast. Defaults to true. Until it is announced nobody can
	// see or subscribe to it. The flip rather than a gate on creating it: tracks can be
	// populated while this is false, then announced once ready. The catalog is served from the
	// first announce, so its first snapshot is the one current at announce time.
	// Turning it off unannounces the broadcast.
	announce: Getter<boolean>;

	// The broadcast name.
	name: Getter<Moq.Path.Valid>;

	// The captured picture size, shared by all video renditions. Usually wired from a
	// Video.Capture's `display` output. The catalog display swaps it for a quarter-turn `rotation`,
	// since it is the size to render at. Omitted from the catalog when undefined.
	display: Getter<{ width: number; height: number } | undefined>;

	// Whether the video should be flipped horizontally on playback. Catalog video-section metadata.
	flip: Getter<boolean>;

	// Clockwise video rotation in degrees. Catalog video-section metadata.
	rotation: Getter<number>;

	/**
	 * How long relays keep a non-latest group of this broadcast's media tracks fetchable, in
	 * milliseconds. Declared on every media track we accept; see {@link Container.trackInfo},
	 * whose default (`undefined` here) is sized so a segmented egress (HLS/DASH) can serve a
	 * full playlist window.
	 *
	 * A retention budget, not a delivery one, so lowering it does not reduce latency: it only
	 * shortens how far back a fetch can reach. A track's retention is fixed when it is created,
	 * so changing this publishes a new network broadcast.
	 */
	maxAge: Getter<Milli | undefined>;

	/**
	 * Override when renditions encode. Omitted: audio retains raw PCM until subscribed, while
	 * video encodes continuously. False encodes both continuously; true gates video too, losing
	 * its past. Unless false, audio keeps raw history up to `maxAge`. Capture stays open.
	 */
	onDemand: Getter<boolean | undefined>;
};

/**
 * A published broadcast: the network broadcast plus a catalog producer and its rendition tracks.
 *
 * Register renditions with {@link video} / {@link audio}; each returns a {@link Rendition} whose
 * producer (usually an encoder) fills the catalog config and encodes into its track.
 * The broadcast owns only its own network/catalog wiring, so {@link close} does not close the
 * renditions' producers.
 */
export class Broadcast {
	/** The catalog track name served to subscribers. */
	static readonly CATALOG_TRACK = Catalog.TRACK;

	/** The wired inputs, read-only. */
	readonly in: Readonlys<BroadcastInput>;

	/**
	 * The catalog, editable at any time regardless of whether anyone is subscribed. The base
	 * `video`/`audio` sections are folded from the registered renditions; an application adds its own
	 * root sections (e.g. `scte35`) by locking it too.
	 */
	readonly catalog = new CatalogProducer();

	readonly #net = new Signal<Moq.Broadcast.Producer | undefined>(undefined);

	/**
	 * The underlying network broadcast, recreated when the name, enabled state, or maxAge changes,
	 * and is `undefined` in between. It lives in the origin rather than any session, so it spans
	 * reconnects. Exposed so an application can serve its own tracks alongside the built-in
	 * catalog/audio/video, e.g. `net.createTrack("meta.json")` plus a matching `catalog` section.
	 * Names under a rendition's ID (`video/1`) are reserved for that rendition's replacement tracks.
	 * Reacquire it via an effect, since a rename swaps in a fresh producer.
	 */
	readonly net: Getter<Moq.Broadcast.Producer | undefined> = this.#net;

	/**
	 * @internal The highest timestamp each rendition has written, which each encoder measures its
	 * catalog `delay` against. Per broadcast, so a swapped one starts fresh.
	 */
	readonly order = new Order();

	// The registered renditions keyed by full track name. A plain object so deep-equality detects a
	// key add/remove; the Rendition values compare by identity, which is stable.
	readonly #renditions = new Signal<Record<string, Rendition<unknown>>>({});

	// The writable signals backing each Rendition's read-only `track` and `slot`, keyed by name.
	// A static network track is exposed here while it is open. On-demand gates non-audio tracks
	// on a subscriber; audio keeps its handle so it can buffer PCM across demand changes.
	// The slot is the current publication's, while there is one.
	readonly #handles = new Map<
		string,
		{ track: Signal<Moq.Track.Producer | undefined>; slot: Signal<Slot | undefined> }
	>();

	// The renditions of the current network broadcast, undefined while there is none.
	readonly #publication = new Signal<Publication | undefined>(undefined);

	// Each catalog ID in the latest catalog fold, which seeds a new network broadcast's publication.
	#listed = new Map<string, Listing>();

	#signals = new Effect();

	constructor(props?: Inputs<BroadcastInput>) {
		this.in = {
			origin: getter(props?.origin),
			enabled: getter(props?.enabled ?? true),
			announce: getter(props?.announce ?? true),
			name: getter(props?.name ?? Moq.Path.empty()),
			display: getter(props?.display),
			flip: getter(props?.flip ?? false),
			rotation: getter(props?.rotation ?? 0),
			maxAge: getter(props?.maxAge),
			onDemand: getter(props?.onDemand),
		};

		this.#signals.run(this.#runCatalog.bind(this));
		this.#signals.run(this.#run.bind(this));
	}

	/**
	 * Register a video rendition under a full track name (e.g. `"video/hd"`). Throws if the name is taken.
	 *
	 * Its track advertises a refresh ratio, so its producer opens a group with a keyframe whenever
	 * `track.demand().wanted` passes the largest group, as the video `Encoder` does.
	 */
	video(name: string): Rendition<Catalog.VideoConfig> {
		return this.#register<Catalog.VideoConfig>(name, "video");
	}

	/** Register an audio rendition under a full track name (e.g. `"audio/data"`). Throws if the name is taken. */
	audio(name: string): Rendition<Catalog.AudioConfig> {
		return this.#register<Catalog.AudioConfig>(name, "audio");
	}

	/**
	 * Register a text (caption/subtitle) rendition under a full track name (e.g. `"captions/en"`).
	 * Throws if the name is taken.
	 *
	 * Set the returned rendition's `config` to a {@link Catalog.TextConfig}, then write one cue per
	 * group into its `track` with `Hang.Container.Legacy.Producer` (each cue is a keyframe, so it opens
	 * its own group). See the module docs for the cue framing. Stamp cues with `@moq/clock`'s `now()` in
	 * microseconds, the broadcast clock the catalog advertises.
	 */
	text(name: string): Rendition<Catalog.TextConfig> {
		return this.#register<Catalog.TextConfig>(name, "text");
	}

	#register<C>(name: string, kind: Kind): Rendition<C> {
		if (this.#renditions.peek()[name]) {
			throw new Error(`rendition already registered: ${name}`);
		}
		this.#publication.peek()?.check(name, kind);

		const handle = {
			track: new Signal<Moq.Track.Producer | undefined>(undefined),
			slot: new Signal<Slot | undefined>(undefined),
		};
		this.#handles.set(name, handle);

		// The slot is the one its producer fills, so it holds this rendition's config.
		const slot = handle.slot as Getter<Slot<C> | undefined>;
		const rendition = makeRendition<C>(name, kind, handle.track, slot, () => this.#unregister(name));
		this.#renditions.update((renditions) => ({ ...renditions, [name]: rendition as Rendition<unknown> }));

		return rendition;
	}

	#unregister(name: string): void {
		const handle = this.#handles.get(name);
		if (handle) {
			// The network track stays with the broadcast, so registering the name again continues it.
			handle.track.set(undefined);
			handle.slot.set(undefined);
			this.#handles.delete(name);
		}

		this.#renditions.update((renditions) => {
			if (!(name in renditions)) return renditions;
			const next = { ...renditions };
			delete next[name];
			return next;
		});
	}

	// Keep the base catalog sections in sync with the registered renditions, leaving extension sections
	// alone. A section with zero defined configs is deleted.
	#runCatalog(effect: Effect) {
		const enabled = effect.get(this.in.enabled);
		const renditions = effect.get(this.#renditions);
		const publication = effect.get(this.#publication);
		if (publication) effect.get(publication.renames);

		const video: Record<string, Catalog.VideoConfig> = {};
		const audio: Record<string, Catalog.AudioConfig> = {};
		const text: Record<string, Catalog.TextConfig> = {};
		const listed = new Map<string, Listing>();
		const served: [Rendition<unknown>, string, Identity][] = [];

		for (const rendition of Object.values(renditions)) {
			const source = enabled ? effect.get(rendition.config) : undefined;
			if (source === undefined) continue;
			// The rendition's media is this broadcast's own track, so its config can't point elsewhere.
			const target = source as { broadcast?: string; track?: string };
			if (target.broadcast !== undefined || target.track !== undefined) {
				throw new Error(`rendition ${rendition.name} config sets broadcast or track, which the broadcast owns`);
			}
			const media = identity(rendition.kind, source);
			listed.set(rendition.name, { kind: rendition.kind, identity: media });
			const track = publication?.name(rendition, media) ?? rendition.name;
			served.push([rendition, track, media]);
			const config = track !== rendition.name ? { ...(source as object), track } : source;

			if (rendition.kind === "video") {
				video[rendition.name] = config as Catalog.VideoConfig;
			} else if (rendition.kind === "audio") {
				audio[rendition.name] = config as Catalog.AudioConfig;
			} else {
				text[rendition.name] = config as Catalog.TextConfig;
			}
		}

		const display = effect.get(this.in.display);
		const flip = effect.get(this.in.flip);
		const rotation = effect.get(this.in.rotation);

		this.catalog.mutate((catalog) => {
			if (Object.keys(video).length > 0) {
				const section: Catalog.Video = { renditions: video };
				// display is optional in the schema, so it gates only itself, not the whole section.
				if (display) {
					const { width, height } = rotateVideoDimensions(display, rotation);
					section.display = { width: Catalog.u53(width), height: Catalog.u53(height) };
				}
				if (flip) section.flip = true;
				if (rotation) section.rotation = rotation;
				catalog.video = section;
			} else {
				delete catalog.video;
			}

			if (Object.keys(audio).length > 0) {
				catalog.audio = { renditions: audio };
			} else {
				delete catalog.audio;
			}

			if (Object.keys(text).length > 0) {
				catalog.text = { renditions: text };
			} else {
				delete catalog.text;
			}
		});
		this.#listed = listed;

		// Only once the catalog holds them, so the track loop never ends a track the catalog still lists.
		if (publication) for (const [rendition, track, media] of served) publication.commit(rendition, track, media);
	}

	// The names outside a publication's own that a replacement must avoid: every registered ID, and
	// every ID and track name in the catalog, including an application's own sections.
	#reserved(): Set<string> {
		const taken = new Set(Object.keys(this.#renditions.peek()));
		// A no-op edit is the catalog's only read; it publishes nothing.
		this.catalog.mutate((catalog) => {
			for (const entries of [
				catalog.audio?.renditions,
				catalog.video?.renditions,
				catalog.text?.renditions,
				catalog.json?.tracks,
				catalog.binary?.tracks,
			]) {
				for (const [key, config] of Object.entries(entries ?? {})) {
					taken.add(key);
					taken.add(Catalog.track(key, config));
				}
			}
		});
		return taken;
	}

	#run(effect: Effect) {
		const values = effect.getAll([this.in.enabled, this.in.origin]);
		if (!values) return;
		const [_enabled, origin] = values;

		const name = effect.get(this.in.name);
		const maxAge = effect.get(this.in.maxAge);
		if (Catalog.detectFormat(name) === undefined) {
			console.warn(
				`You should append .hang to broadcast name ${JSON.stringify(name)} to make the catalog format explicit.`,
			);
		}

		// Creating into the origin outlives any single session: a reconnect re-announces the
		// broadcast and new subscriptions land on the same producer.
		const broadcastPublication = origin.createBroadcast(name);
		const broadcast = broadcastPublication.broadcast;
		effect.cleanup(() => broadcast.close());
		// One publisher instance per broadcast, kept across unannounce and announce, so a
		// restart is a new broadcast rather than a resume into the old one.
		const epoch = Moq.Epoch.mint();

		// Expose it before serving so an application reacting to `net` can insert its own tracks.
		this.#net.set(broadcast);
		effect.cleanup(() => {
			if (this.#net.peek() === broadcast) this.#net.set(undefined);
		});

		// Each track lives as long as the broadcast: a track name always means the same content,
		// so a re-announce or a rendition change keeps writing to it instead of restarting its
		// group sequence under the same epoch. The publication keeps rendition tracks by catalog ID,
		// so registering an ID again finds its track; the catalog's stay out of reach of a colliding
		// rendition.
		const publication = new Publication(this.#listed, () => this.#reserved());
		this.#publication.set(publication);
		effect.cleanup(() => {
			publication.close();
			if (this.#publication.peek() === publication) this.#publication.set(undefined);
		});

		let serving = false;
		effect.run((inner) => {
			if (!inner.get(this.in.announce)) {
				broadcastPublication.unannounce();
				return;
			}

			// Serve the catalog from the first announce. Nobody can find the broadcast before then, so
			// an earlier catalog would only leave a stale first snapshot behind; this way the first one
			// is whatever the catalog holds at announce time.
			if (!serving) {
				serving = true;
				// A catalog may publish once and stay unchanged for the broadcast's whole life. Keep that
				// sole closed snapshot replayable so a viewer arriving after the ordinary media retention
				// window can still bootstrap.
				const track = broadcast.createTrack(Broadcast.CATALOG_TRACK, {
					// Each catalog snapshot is stamped when written.
					timescale: Moq.Time.Timescale.MILLI,
					maxAge: Milli(Number.MAX_SAFE_INTEGER),
					priority: Catalog.PRIORITY.catalog,
					// Catalog JSON is small and repetitive.
					compress: true,
				});
				effect.cleanup(() => {
					track.finish();
					track.close();
				});
				this.catalog.serve(track, effect);
			}

			broadcastPublication.announce({ epoch });
		});

		// Static tracks fan out to every subscriber. The encoder-facing handle stays set while the
		// track is open. Audio keeps it while buffering PCM; on-demand video gates it on a subscriber.
		effect.run((inner) => {
			const renditions = inner.get(this.#renditions);
			inner.get(publication.renames);

			for (const rendition of Object.values(renditions)) {
				const handle = this.#handles.get(rendition.name);
				if (!handle) continue;

				const entry = publication.entry(rendition);
				inner.set(handle.slot, entry.slot);
				// A replacement registration is a new producer, so the last one's failure no longer holds.
				if (entry.owner && entry.owner !== rendition) entry.slot.error.set(undefined);
				const cause = inner.get(entry.slot.error);
				// Including an idle track, which no producer holds to abort.
				if (entry.track && cause) entry.track.abort(cause);
				if (entry.track && entry.track.name !== entry.name) {
					// The catalog moved this rendition to a new name for new media. End the old track
					// cleanly on its last identity, so its subscribers follow the catalog.
					entry.track.finish();
					entry.track.close();
					entry.track = undefined;
				} else if (entry.track && entry.track.closed.peek() !== undefined) {
					// A failure leaves this rendition down until it clears. It then recovers under a fresh
					// track name, so its group sequence can start over.
					if (!cause) publication.rename(entry);
					continue;
				}
				if (!entry.track) {
					const info = Container.trackInfo({ maxAge, priority: Catalog.PRIORITY[rendition.kind] });
					entry.track = broadcast.createTrack(
						entry.name,
						rendition.kind === "video" ? { ...info, refresh: VIDEO_REFRESH } : info,
					);
				}
				entry.owner = rendition;
				const served = entry.track;
				inner.run((gate) => {
					const closed = gate.get(served.closed);
					if (closed !== undefined) {
						// A producer aborting its track directly fails the rendition the same way.
						if (closed && !entry.slot.error.peek()) entry.slot.error.set(closed);
						return;
					}
					if (
						rendition.kind !== "audio" &&
						gate.get(this.in.onDemand) &&
						gate.get(served.demand().wanted) === undefined
					)
						return;
					gate.set(handle.track, served);
				});
			}
		});
	}

	/** Stop and release everything this holds. */
	close() {
		this.#signals.close();
	}
}
