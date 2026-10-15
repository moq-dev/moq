import * as Catalog from "@moq/hang/catalog";
import * as Container from "@moq/hang/container";
import * as Moq from "@moq/net";
import { Effect, type Getter, getter, type Inputs, type Readonlys, Signal } from "@moq/signals";
import { CatalogProducer } from "./catalog";
import { errors } from "./internal";
import { Baseline } from "./jitter";
import { type Kind, Rendition } from "./rendition";

// Signals the broadcast reads. Whoever owns the backing Signal (the element, or another component
// whose output is wired in, e.g. a Video.Capture's `display`) does the writing.
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
	announce: Getter<boolean>;

	// The broadcast name.
	name: Getter<Moq.Path.Valid>;

	// Catalog video-section display size, shared by all video renditions. Usually wired from a
	// Video.Capture's `display` output. Omitted from the catalog when undefined.
	display: Getter<{ width: number; height: number } | undefined>;

	// Whether the video should be flipped horizontally on playback. Catalog video-section metadata.
	flip: Getter<boolean>;

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
	maxAge: Getter<Moq.Time.Milli | undefined>;
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
	/** The DEFLATE-compressed catalog track, served alongside {@link CATALOG_TRACK} with identical content. */
	static readonly CATALOG_TRACK_COMPRESSED = Catalog.TRACK_COMPRESSED;

	readonly in: Readonlys<BroadcastInput>;

	// The catalog, editable at any time regardless of whether anyone is subscribed. The base
	// `video`/`audio` sections are folded from the registered renditions; an application adds its own
	// root sections (e.g. `scte35`) by locking it too.
	readonly catalog = new CatalogProducer();

	// The underlying network broadcast, recreated when the name, enabled state, or maxAge changes,
	// and is `undefined` in between. It lives in the origin rather than any session, so it spans
	// reconnects. Exposed so an application can serve its own tracks alongside the built-in
	// catalog/audio/video, e.g. `net.createTrack("meta.json")` plus a matching `catalog` section.
	// Reacquire it via an effect, since a rename swaps in a fresh producer.
	readonly net = new Signal<Moq.Broadcast.Producer | undefined>(undefined);

	/**
	 * @internal The recent minimum flush lateness across every rendition, which each encoder
	 * measures its catalog `delay` against. Per broadcast, so a swapped one starts fresh.
	 */
	readonly baseline = new Baseline();

	// The registered renditions keyed by full track name. A plain object so deep-equality detects a
	// key add/remove; the Rendition values compare by identity, which is stable.
	readonly #renditions = new Signal<Record<string, Rendition<unknown>>>({});

	// The writable track producer signals backing each Rendition's read-only `track`, keyed by name.
	// A static network track is exposed here only while at least one subscriber uses it.
	readonly #tracks = new Map<string, Signal<Moq.Track.Producer | undefined>>();

	// The kind of each rendition track in the current network broadcast. A new one starts over.
	#kinds = new Map<string, Kind>();

	// Replacement tracks keep their catalog IDs but never reuse a closed track name.
	readonly #names = new Signal<Record<string, string>>({});

	#signals = new Effect();

	constructor(props?: Inputs<BroadcastInput>) {
		this.in = {
			origin: getter(props?.origin),
			enabled: getter(props?.enabled ?? true),
			announce: getter(props?.announce ?? true),
			name: getter(props?.name ?? Moq.Path.empty()),
			display: getter(props?.display),
			flip: getter(props?.flip ?? false),
			maxAge: getter(props?.maxAge),
		};

		this.#signals.run(this.#runCatalog.bind(this));
		this.#signals.run(this.#run.bind(this));
	}

	/** Register a video rendition under a full track name (e.g. `"video/hd"`). Throws if the name is taken. */
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
	 * its own group). See the module docs for the cue framing. Stamp cues with `performance.now()` in
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

		for (const rendition of Object.values(renditions)) {
			const source = enabled ? effect.get(rendition.config) : undefined;
			if (source === undefined) continue;
			const track = names[rendition.name];
			const config = track ? { ...(source as object), track } : source;

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

		this.catalog.mutate((catalog) => {
			if (Object.keys(video).length > 0) {
				const section: Catalog.Video = { renditions: video };
				// display is optional in the schema, so it gates only itself, not the whole section.
				if (display) {
					section.display = { width: Catalog.u53(display.width), height: Catalog.u53(display.height) };
				}
				if (flip) section.flip = true;
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
		const broadcast = origin.createBroadcast(name);
		effect.cleanup(() => broadcast.close());
		// One publisher instance per broadcast, kept across unannounce and announce, so a
		// restart is a new broadcast rather than a resume into the old one.
		const epoch = Moq.Epoch.mint();

		// Expose it before serving so an application reacting to `net` can insert its own tracks.
		this.net.set(broadcast);
		effect.cleanup(() => {
			if (this.net.peek() === broadcast) this.net.set(undefined);
		});

		// Each track lives as long as the broadcast: a track name always means the same content,
		// so a re-announce or a rendition change keeps writing to it instead of restarting its
		// group sequence under the same epoch. Rendition tracks by name, so registering a name
		// again finds its track; the catalog's stay out of reach of a colliding rendition.
		const tracks = new Map<string, { track: Moq.Track.Producer; owner: Rendition<unknown> }>();
		this.#names.set({});
		const kinds = new Map<string, Kind>();
		this.#kinds = kinds;
		effect.cleanup(() => {
			for (const { track } of tracks.values()) track.close();
			if (this.#kinds === kinds) this.#kinds = new Map();
		});

		let serving = false;
		effect.run((inner) => {
			if (!inner.get(this.in.announce)) {
				broadcast.unannounce();
				return;
			}

			// Serve the catalog from the first announce. Nobody can find the broadcast before then, so
			// an earlier catalog would only leave a stale first snapshot behind; this way the first one
			// is whatever the catalog holds at announce time.
			if (!serving) {
				serving = true;
				for (const [name, compression] of [
					[Broadcast.CATALOG_TRACK, false],
					[Broadcast.CATALOG_TRACK_COMPRESSED, true],
				] as const) {
					// A catalog may publish once and stay unchanged for the broadcast's whole life. Keep
					// that sole closed snapshot replayable so a viewer arriving after the ordinary media
					// retention window can still bootstrap.
					const track = broadcast.createTrack(name, {
						// Each catalog snapshot is stamped when written.
						timescale: Moq.Time.Timescale.MILLI,
						maxAge: Moq.Time.Milli(Number.MAX_SAFE_INTEGER),
						priority: Catalog.PRIORITY.catalog,
					});
					effect.cleanup(() => track.close());
					this.catalog.serve(track, effect, { compression });
				}
			}

			broadcast.announce({ epoch });
		});

		// Static tracks fan out to every subscriber. Keep the encoder-facing handle demand-gated
		// so capture and encoding still stop when the final subscriber leaves.
		effect.run((inner) => {
			const renditions = inner.get(this.#renditions);

			for (const rendition of Object.values(renditions)) {
				const signal = this.#tracks.get(rendition.name);
				if (!signal) continue;

				let entry = tracks.get(rendition.name);
				const cause = entry && errors.get(entry.owner)?.peek();
				if (entry && cause) entry.track.close(cause);
				// An error leaves this rendition down. Only a replacement registration may
				// recover, with a fresh track name so its group sequence can start over.
				if (!entry || (entry.owner !== rendition && entry.track.closed.peek() !== undefined)) {
					const name = entry ? `${rendition.name}/${Moq.Epoch.mint()}` : rendition.name;
					const track = broadcast.createTrack(
						name,
						Container.trackInfo({ maxAge, priority: Catalog.PRIORITY[rendition.kind] }),
					);
					entry = { track, owner: rendition };
					tracks.set(rendition.name, entry);
					kinds.set(rendition.name, rendition.kind);
					if (name !== rendition.name) this.#names.update((names) => ({ ...names, [rendition.name]: name }));
				} else {
					entry.owner = rendition;
				}
				const served = entry.track;
				const error = errors.get(rendition);
				if (error) {
					inner.run((failure) => {
						const cause = failure.get(error);
						if (cause) served.close(cause);
					});
				}
				inner.run((demand) => {
					demand.set(signal, demand.get(served.demand().used) ? served : undefined);
				});
			}
		});
	}

	close() {
		this.#signals.close();
	}
}
