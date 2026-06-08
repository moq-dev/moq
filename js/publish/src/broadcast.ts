import * as Catalog from "@moq/hang/catalog";
import * as Json from "@moq/json";
import * as Moq from "@moq/net";
import { Effect, type Getter, Signal } from "@moq/signals";
import * as Audio from "./audio";
import * as Video from "./video";

// Serves a single application-defined track when subscribed. Same shape as the built-in
// `serve` methods, so an extension can route its own tracks (e.g. a chat message track)
// through the broadcast's request loop.
export type ServeTrack = (track: Moq.Track, effect: Effect) => void;

export type BroadcastProps = {
	connection?: Moq.Connection.Established | Signal<Moq.Connection.Established | undefined>;
	enabled?: boolean | Signal<boolean>;
	name?: Moq.Path.Valid | Signal<Moq.Path.Valid>;
	audio?: Audio.EncoderProps;
	video?: Video.Props;

	// Extra catalog sections merged into the published catalog alongside `video`/`audio`.
	// This is how the application layer adds its own root sections (chat, location, scte35, ...)
	// without hang knowing about them.
	sections?: Record<string, unknown> | Signal<Record<string, unknown> | undefined>;

	// Handlers for application-defined tracks, keyed by track name. Consulted when a
	// subscription arrives for a track the base broadcast doesn't recognize.
	tracks?: Record<string, ServeTrack>;
};

export class Broadcast {
	static readonly CATALOG_TRACK = "catalog.json";

	connection: Signal<Moq.Connection.Established | undefined>;
	enabled: Signal<boolean>;
	name: Signal<Moq.Path.Valid>;

	audio: Audio.Encoder;
	video: Video.Root;

	// Application-supplied extensions, see `BroadcastProps`.
	sections: Getter<Record<string, unknown> | undefined>;
	tracks: Record<string, ServeTrack>;

	signals = new Effect();

	constructor(props?: BroadcastProps) {
		this.connection = Signal.from(props?.connection);
		this.enabled = Signal.from(props?.enabled ?? false);
		this.name = Signal.from(props?.name ?? Moq.Path.empty());

		this.audio = new Audio.Encoder(props?.audio);
		this.video = new Video.Root({ ...props?.video, connection: this.connection });

		this.sections = Signal.from(props?.sections);
		this.tracks = props?.tracks ?? {};

		this.signals.run(this.#run.bind(this));
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
						this.#serveCatalog(new Json.Producer<Record<string, unknown>>(request.track), effect);
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
						const handler = this.tracks[request.track.name];
						if (handler) {
							handler(request.track, effect);
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

	#serveCatalog(producer: Json.Producer<Record<string, unknown>>, effect: Effect): void {
		if (!effect.get(this.enabled)) {
			// Clear the catalog.
			producer.update({});
			return;
		}

		const catalog: Catalog.Root = {
			video: effect.get(this.video.catalog),
			audio: effect.get(this.audio.catalog),
		};

		// Merge any application-defined sections on top of the base catalog.
		producer.update({ ...catalog, ...effect.get(this.sections) });
	}

	close() {
		this.signals.close();
		this.audio.close();
		this.video.close();
	}
}
