import * as Catalog from "@moq/hang/catalog";
import * as Moq from "@moq/net";
import { Effect, Signal } from "@moq/signals";
import * as Audio from "./audio";
import * as Video from "./video";

export type BroadcastProps = {
	connection?: Moq.Connection.Established | Signal<Moq.Connection.Established | undefined>;
	enabled?: boolean | Signal<boolean>;
	name?: Moq.Path.Valid | Signal<Moq.Path.Valid>;
	audio?: Audio.EncoderProps;
	video?: Video.Props;
};

/** Serves a custom track when a subscriber requests it, scoped to the subscription's lifetime. */
export type ServeTrack = (track: Moq.Track, effect: Effect) => void;

export class Broadcast {
	static readonly CATALOG_TRACK = "catalog.json";

	connection: Signal<Moq.Connection.Established | undefined>;
	enabled: Signal<boolean>;
	name: Signal<Moq.Path.Valid>;

	audio: Audio.Encoder;
	video: Video.Root;

	// The catalog, editable at any time regardless of whether anyone is subscribed. The base
	// `video`/`audio` sections are kept in sync from the encoders; an application adds its own root
	// sections (e.g. `scte35`) by mutating it too. Catalog.Producer pins deltas off (one snapshot per
	// group) to stay byte-compatible with consumers that only read snapshots.
	readonly catalog: Catalog.Producer = new Catalog.Producer();

	// Handlers for custom tracks registered via `publishTrack`, keyed by track name. Persists across
	// reconnects so a new `Moq.Broadcast` still serves them.
	#tracks = new Map<string, ServeTrack>();

	// Built-in track names handled before `#tracks`, so a custom handler registered under one of
	// these would never run. `publishTrack` rejects them to fail fast.
	static readonly #RESERVED_TRACKS: ReadonlySet<string> = new Set([
		Broadcast.CATALOG_TRACK,
		Audio.Encoder.TRACK,
		Video.Root.TRACK_HD,
		Video.Root.TRACK_SD,
	]);

	signals = new Effect();

	constructor(props?: BroadcastProps) {
		this.connection = Signal.from(props?.connection);
		this.enabled = Signal.from(props?.enabled ?? false);
		this.name = Signal.from(props?.name ?? Moq.Path.empty());

		this.audio = new Audio.Encoder(props?.audio);
		this.video = new Video.Root({ ...props?.video, connection: this.connection });

		this.signals.run(this.#runCatalog.bind(this));
		this.signals.run(this.#run.bind(this));
	}

	// Keep the base catalog sections in sync with the encoders, leaving extension sections alone.
	#runCatalog(effect: Effect) {
		const enabled = effect.get(this.enabled);
		const video = enabled ? effect.get(this.video.catalog) : undefined;
		const audio = enabled ? effect.get(this.audio.catalog) : undefined;

		this.catalog.mutate((catalog) => {
			if (video !== undefined) catalog.video = video;
			else delete catalog.video;

			if (audio !== undefined) catalog.audio = audio;
			else delete catalog.audio;
		});
	}

	#run(effect: Effect) {
		const values = effect.getAll([this.enabled, this.connection]);
		if (!values) return;
		const [_enabled, connection] = values;

		const name = effect.get(this.name);
		if (Catalog.detectFormat(name) === undefined) {
			console.warn(
				`You should append .hang to broadcast name ${JSON.stringify(name)} to make the catalog format explicit.`,
			);
		}

		const broadcast = new Moq.Broadcast();
		effect.cleanup(() => broadcast.close());

		connection.publish(name, broadcast);

		effect.spawn(this.#runBroadcast.bind(this, broadcast, effect));
	}

	async #runBroadcast(broadcast: Moq.Broadcast, effect: Effect) {
		for (;;) {
			const request = await broadcast.requested();
			if (!request) break;

			effect.cleanup(() => request.track.close());

			effect.run((effect) => {
				if (effect.get(request.track.state.closed)) return;

				switch (request.track.name) {
					case Broadcast.CATALOG_TRACK:
						this.catalog.serve(request.track, effect);
						break;
					case Audio.Encoder.TRACK:
						this.audio.serve(request.track, effect);
						break;
					case Video.Root.TRACK_HD:
						this.video.hd.serve(request.track, effect);
						break;
					case Video.Root.TRACK_SD:
						this.video.sd.serve(request.track, effect);
						break;
					default: {
						const serve = this.#tracks.get(request.track.name);
						if (serve) {
							serve(request.track, effect);
							break;
						}
						console.error("received subscription for unknown track", request.track.name);
						request.track.close(new Error(`Unknown track: ${request.track.name}`));
						break;
					}
				}
			});
		}
	}

	/**
	 * Serve a custom track within this broadcast, identified by name.
	 *
	 * When a subscriber requests a track with this name, `serve` runs with the track and an effect
	 * scoped to that subscription (cleaned up when the subscriber goes away). The handler persists
	 * across reconnects. This is the generic hook for arbitrary payloads; encode them yourself.
	 *
	 * Returns a function that unregisters the handler. Note this does not close already-served
	 * subscriptions, nor touch the catalog. Throws if `name` collides with a built-in track
	 * (catalog/audio/video), since those are served first and the handler would never run.
	 *
	 * For a JSON track, serve each track from a track-less `@moq/json` `Producer` (the same fan-out
	 * producer the catalog uses, seeding late joiners with the latest value). Advertise the track by
	 * writing your own section to {@link catalog}, e.g. to support a custom `scte35` section with no
	 * hang-specific support:
	 *
	 * ```ts
	 * import { Producer } from "@moq/json";
	 * const scte35 = new Producer({ initial: { splices: [] } });
	 * broadcast.publishTrack("scte35.json", (track, effect) => scte35.serve(track, effect));
	 * broadcast.catalog.mutate((c) => { c.scte35 = { track: "scte35.json" }; });
	 * scte35.update({ splices: [42] });
	 * ```
	 */
	publishTrack(name: string, serve: ServeTrack): () => void {
		if (Broadcast.#RESERVED_TRACKS.has(name)) {
			throw new Error(`Track name is reserved: ${name}`);
		}
		this.#tracks.set(name, serve);
		return () => {
			if (this.#tracks.get(name) === serve) this.#tracks.delete(name);
		};
	}

	close() {
		this.signals.close();
		this.audio.close();
		this.video.close();
	}
}
