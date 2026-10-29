import { Milli } from "@moq/clock";
import * as Catalog from "@moq/hang/catalog";
import * as Container from "@moq/hang/container";
import * as Json from "@moq/json";
import * as Moq from "@moq/net";
import { Effect, type Getter, getter, type Inputs, type Readonlys, Signal } from "@moq/signals";
import { rotateVideoDimensions } from "@moq/video";
import { CatalogProducer } from "./catalog";
import { errors } from "./internal";
import { Order } from "./order";
import { type Kind, Rendition } from "./rendition";

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
};

/**
 * A published broadcast: the network broadcast plus a catalog producer and its rendition tracks.
 *
 * Register renditions with {@link video} / {@link audio}; each returns a {@link Rendition} whose
 * producer (usually an encoder) fills the catalog config and encodes into the demand-gated track.
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

	// The writable track producer signals backing each Rendition's read-only `track`, keyed by name.
	// A static network track is exposed here only while at least one subscriber uses it.
	readonly #tracks = new Map<string, Signal<Moq.Track.Producer | undefined>>();

	// The kind of each rendition track in the current network broadcast. A new one starts over.
	#kinds = new Map<string, Kind>();

	// Each catalog ID's track name in the current network broadcast, when it is not the ID itself.
	// A replacement for a closed track, or a rendition whose media identity changed, keeps its
	// catalog ID under a fresh track name, so a name always means the same content.
	readonly #names = new Signal<Record<string, string>>({});

	// Every replacement track name minted in the current network broadcast, even one never served.
	#replacements = new Set<string>();

	// The decoder identity of each track name the current network broadcast's catalog has served, or
	// undefined before it serves one. Kept across a rendition's removal, so a name returning with
	// different media takes a fresh name too.
	#served: Map<string, Identity> | undefined;

	// The identity of each track name in the latest catalog fold, recorded when the catalog is first served.
	#listed = new Map<string, Identity>();

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
		// A name keeps its track, so it keeps its kind: different media takes a different name.
		const previous = this.#kinds.get(name);
		if (previous !== undefined && previous !== kind) {
			throw new Error(`rendition ${name} was registered as ${previous}, not ${kind}`);
		}
		// A replacement track holds or held the name, so another rendition's media owns it.
		if (this.#replacements.has(name)) {
			throw new Error(`rendition ${name} names another rendition's replacement track`);
		}

		const track = new Signal<Moq.Track.Producer | undefined>(undefined);
		this.#tracks.set(name, track);

		const rendition = new Rendition<C>(name, kind, track, () => this.#unregister(name));
		this.#renditions.update((renditions) => ({ ...renditions, [name]: rendition as Rendition<unknown> }));

		return rendition;
	}

	#unregister(name: string): void {
		const track = this.#tracks.get(name);
		if (track) {
			// The network track stays with the broadcast, so registering the name again continues it.
			track.set(undefined);
			this.#tracks.delete(name);
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
		const names = effect.get(this.#names);

		const video: Record<string, Catalog.VideoConfig> = {};
		const audio: Record<string, Catalog.AudioConfig> = {};
		const text: Record<string, Catalog.TextConfig> = {};
		const listed = new Map<string, Identity>();
		const renamed: Record<string, string> = {};

		for (const rendition of Object.values(renditions)) {
			const source = enabled ? effect.get(rendition.config) : undefined;
			if (source === undefined) continue;
			// The rendition's media is this broadcast's own track, so its config can't point elsewhere.
			const target = source as { broadcast?: string; track?: string };
			if (target.broadcast !== undefined || target.track !== undefined) {
				throw new Error(`rendition ${rendition.name} config sets broadcast or track, which the broadcast owns`);
			}
			let track = names[rendition.name] ?? rendition.name;
			// Everything else (bitrate, frame rate, coded size, ...) updates the served track in place.
			const media = identity(source);
			const served = this.#served?.get(track);
			if (served && !Json.deepEqual(served, media)) {
				track = this.#mint(rendition.name);
				renamed[rendition.name] = track;
			}
			this.#served?.set(track, media);
			listed.set(track, media);
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

		// The network loop ends each renamed rendition's old track and serves the new name.
		if (Object.keys(renamed).length > 0) this.#names.update((names) => ({ ...names, ...renamed }));
	}

	// The next unused `<id>/<n>` track name in the current network broadcast. This publisher owns the
	// namespace and reserves `<id>/` for the rendition (see `net`), so skipping every name it has
	// served, registered, or listed in the catalog (including an application's own sections) is
	// enough to never reuse one.
	#mint(id: string): string {
		const taken = new Set([
			...this.#replacements,
			...(this.#served?.keys() ?? []),
			...Object.keys(this.#renditions.peek()),
			// Every ID that held a track here, even one removed before it had a config.
			...this.#kinds.keys(),
		]);
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
		for (let n = 1; ; n++) {
			const name = `${id}/${n}`;
			if (taken.has(name)) continue;
			this.#replacements.add(name);
			return name;
		}
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
		// group sequence under the same epoch. Rendition tracks by name, so registering a name
		// again finds its track; the catalog's stay out of reach of a colliding rendition.
		const tracks = new Map<string, { track: Moq.Track.Producer; owner: Rendition<unknown> }>();
		this.#names.set({});
		this.#replacements = new Set();
		this.#served = undefined;
		const kinds = new Map<string, Kind>();
		this.#kinds = kinds;
		effect.cleanup(() => {
			for (const { track } of tracks.values()) track.finish();
			for (const { track } of tracks.values()) track.close();
			if (this.#kinds === kinds) this.#kinds = new Map();
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
				// From here a name keeps the identity this first snapshot gives it.
				this.#served = new Map(this.#listed);
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

		// Static tracks fan out to every subscriber. Keep the encoder-facing handle demand-gated
		// so capture and encoding still stop when the final subscriber leaves.
		effect.run((inner) => {
			const renditions = inner.get(this.#renditions);
			const names = inner.get(this.#names);

			for (const rendition of Object.values(renditions)) {
				const signal = this.#tracks.get(rendition.name);
				if (!signal) continue;

				let entry = tracks.get(rendition.name);
				const cause = entry && errors.get(entry.owner)?.peek();
				if (entry && cause) entry.track.abort(cause);
				const name = names[rendition.name] ?? rendition.name;
				if (entry && entry.track.name !== name) {
					// The catalog moved this rendition to a new name for new media. End the old track
					// cleanly on its last identity, so its subscribers follow the catalog.
					entry.track.finish();
					entry.track.close();
					entry = undefined;
				} else if (entry && entry.owner !== rendition && entry.track.closed.peek() !== undefined) {
					// An error leaves this rendition down. Only a replacement registration may recover,
					// with a fresh track name so its group sequence can start over.
					const fresh = this.#mint(rendition.name);
					this.#names.update((names) => ({ ...names, [rendition.name]: fresh }));
					continue;
				}
				if (!entry) {
					const info = Container.trackInfo({ maxAge, priority: Catalog.PRIORITY[rendition.kind] });
					const track = broadcast.createTrack(
						name,
						rendition.kind === "video" ? { ...info, refresh: VIDEO_REFRESH } : info,
					);
					entry = { track, owner: rendition };
					tracks.set(rendition.name, entry);
					kinds.set(rendition.name, rendition.kind);
				} else {
					entry.owner = rendition;
				}
				const served = entry.track;
				const error = errors.get(rendition);
				if (error) {
					inner.run((failure) => {
						const cause = failure.get(error);
						if (cause) served.abort(cause);
					});
				}
				inner.run((demand) => {
					demand.set(signal, demand.get(served.demand().wanted) !== undefined ? served : undefined);
				});
			}
		});
	}

	/** Stop and release everything this holds. */
	close() {
		this.#signals.close();
	}
}

// The fields a consumer can't change without reinitializing its decoder (or demuxer, for
// `container`), so changing any of them takes a new track name.
type Identity = Pick<
	Partial<Catalog.AudioConfig & Catalog.VideoConfig>,
	| "codec"
	| "description"
	| "container"
	| "sampleRate"
	| "numberOfChannels"
	| "displayAspectWidth"
	| "displayAspectHeight"
>;

// Cloned, so a producer mutating a config it shares (such as its container) can't rewrite what a
// name was served with.
function identity(config: unknown): Identity {
	const { codec, description, container, sampleRate, numberOfChannels, displayAspectWidth, displayAspectHeight } =
		config as Identity;
	return structuredClone({
		codec,
		description,
		container,
		sampleRate,
		numberOfChannels,
		displayAspectWidth,
		displayAspectHeight,
	});
}
