import * as Catalog from "@moq/hang/catalog";
import * as Moq from "@moq/net";
import { Effect, type Getter, getter, type Inputs, type Readonlys, Signal } from "@moq/signals";
import { CatalogProducer } from "./catalog";
import { type Kind, Rendition } from "./rendition";

// Signals the broadcast reads. Whoever owns the backing Signal (the element, or another component
// whose output is wired in, e.g. a Video.Capture's `display`) does the writing.
export type BroadcastInput = {
	connection: Getter<Moq.Connection.Established | undefined>;

	// Whether to publish the broadcast. Defaults to false so nothing is announced until ready.
	enabled: Getter<boolean>;

	// The broadcast name.
	name: Getter<Moq.Path.Valid>;

	// Catalog video-section display size, shared by all video renditions. Usually wired from a
	// Video.Capture's `display` output. Omitted from the catalog when undefined.
	display: Getter<{ width: number; height: number } | undefined>;

	// Whether the video should be flipped horizontally on playback. Catalog video-section metadata.
	flip: Getter<boolean>;
};

/**
 * A published broadcast: the network broadcast plus a catalog producer, minting per-rendition track
 * handles on demand.
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

	// The underlying network broadcast, (re)created on each (re)connection and `undefined` while
	// offline. Exposed so an application can serve its own tracks alongside the built-in
	// catalog/audio/video, e.g. `net.createTrack("meta.json")` plus a matching `catalog` section.
	// Reacquire it via an effect, since reconnecting swaps in a fresh producer.
	readonly net = new Signal<Moq.Broadcast.Producer | undefined>(undefined);

	// The registered renditions keyed by full track name. A plain object so deep-equality detects a
	// key add/remove; the Rendition values compare by identity, which is stable.
	readonly #renditions = new Signal<Record<string, Rendition<unknown>>>({});

	// The writable track producer signals backing each Rendition's read-only `track`, keyed by name.
	// The request loop sets these on accept; teardown and unregister clear them.
	readonly #tracks = new Map<string, Signal<Moq.Track.Producer | undefined>>();

	#signals = new Effect();

	constructor(props?: Inputs<BroadcastInput>) {
		this.in = {
			connection: getter(props?.connection),
			enabled: getter(props?.enabled ?? false),
			name: getter(props?.name ?? Moq.Path.empty()),
			display: getter(props?.display),
			flip: getter(props?.flip ?? false),
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

	#register<C>(name: string, kind: Kind): Rendition<C> {
		if (this.#renditions.peek()[name]) {
			throw new Error(`rendition already registered: ${name}`);
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
			track.peek()?.close();
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

		const video: Record<string, Catalog.VideoConfig> = {};
		const audio: Record<string, Catalog.AudioConfig> = {};

		for (const rendition of Object.values(renditions)) {
			const config = enabled ? effect.get(rendition.config) : undefined;
			if (config === undefined) continue;

			if (rendition.kind === "video") {
				video[rendition.name] = config as Catalog.VideoConfig;
			} else {
				audio[rendition.name] = config as Catalog.AudioConfig;
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
		});
	}

	#run(effect: Effect) {
		const values = effect.getAll([this.in.enabled, this.in.connection]);
		if (!values) return;
		const [_enabled, connection] = values;

		const name = effect.get(this.in.name);
		if (Catalog.detectFormat(name) === undefined) {
			console.warn(
				`You should append .hang to broadcast name ${JSON.stringify(name)} to make the catalog format explicit.`,
			);
		}

		const broadcast = new Moq.Broadcast.Producer();
		effect.cleanup(() => broadcast.close());

		// Close every active rendition track when the broadcast tears down (reconnect/offline), so an
		// encoder stops encoding into a dead producer. The Rendition handles themselves stay registered.
		effect.cleanup(() => {
			for (const track of this.#tracks.values()) {
				track.peek()?.close();
				track.set(undefined);
			}
		});

		// Publish it before serving so an application reacting to `net` can insert its own tracks.
		this.net.set(broadcast);
		effect.cleanup(() => {
			if (this.net.peek() === broadcast) this.net.set(undefined);
		});

		connection.publish(name, broadcast);

		effect.spawn(this.#runBroadcast.bind(this, broadcast, effect));
	}

	async #runBroadcast(broadcast: Moq.Broadcast.Producer, effect: Effect) {
		for (;;) {
			const request = await broadcast.requested();
			if (!request) break;

			if (request.name === Broadcast.CATALOG_TRACK || request.name === Broadcast.CATALOG_TRACK_COMPRESSED) {
				const compression = request.name === Broadcast.CATALOG_TRACK_COMPRESSED;
				const track = request.accept();

				// Serve from a per-subscription child scope. Releasing it when this subscriber leaves keeps
				// serving state from piling up on the connection-lifetime effect as viewers come and go.
				const dispose = effect.run((effect) => {
					effect.cleanup(() => track.close());
					this.catalog.serve(track, effect, { compression });
				});
				void track.closed.then(dispose);
				continue;
			}

			const signal = this.#tracks.get(request.name);
			if (!signal) {
				console.error("received subscription for unknown track", request.name);
				request.reject(new Error(`Unknown track: ${request.name}`));
				continue;
			}

			const track = request.accept();

			// A second subscription for the same name supersedes the first: close the old producer.
			signal.peek()?.close();
			signal.set(track);

			// Clear the signal when this track closes on its own, unless it's already been replaced. A
			// plain promise callback (no child effect) so nothing lingers on the connection effect.
			void track.closed.then(() => {
				if (signal.peek() === track) signal.set(undefined);
			});
		}
	}

	close() {
		this.#signals.close();
	}
}
