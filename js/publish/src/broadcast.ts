import * as Catalog from "@moq/hang/catalog";
import * as Moq from "@moq/net";
import { Effect, Signal } from "@moq/signals";
import * as Audio from "./audio";
import { CatalogProducer } from "./catalog";
import * as Video from "./video";

export type BroadcastProps = {
	connection?: Moq.Connection.Established | Signal<Moq.Connection.Established | undefined>;
	enabled?: boolean | Signal<boolean>;
	name?: Moq.Path.Valid | Signal<Moq.Path.Valid>;
	audio?: Audio.EncoderProps;
	video?: Video.Props;
};

export class Broadcast {
	static readonly CATALOG_TRACK = "catalog.json";

	connection: Signal<Moq.Connection.Established | undefined>;
	enabled: Signal<boolean>;
	name: Signal<Moq.Path.Valid>;

	audio: Audio.Encoder;
	video: Video.Root;

	// The catalog, editable at any time regardless of whether anyone is subscribed. The base
	// `video`/`audio` sections are kept in sync from the encoders; an application adds its own root
	// sections (e.g. `scte35`) by locking it too.
	readonly catalog = new CatalogProducer();

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

			// dev's reshape hands back a TrackRequest: switch on its name, reject unknown
			// tracks, and accept the rest into a producer to serve.
			let serve: ((track: Moq.TrackProducer, effect: Effect) => void) | undefined;
			switch (request.name) {
				case Broadcast.CATALOG_TRACK:
					serve = (track, effect) => this.catalog.serve(track, effect);
					break;
				case Audio.Encoder.TRACK:
					serve = (track, effect) => this.audio.serve(track, effect);
					break;
				case Video.Root.TRACK_HD:
					serve = (track, effect) => this.video.hd.serve(track, effect);
					break;
				case Video.Root.TRACK_SD:
					serve = (track, effect) => this.video.sd.serve(track, effect);
					break;
				default:
					console.error("received subscription for unknown track", request.name);
					request.reject(new Error(`Unknown track: ${request.name}`));
					continue;
			}

			const track = request.accept();
			effect.cleanup(() => track.close());
			effect.run((effect) => {
				if (effect.get(track.state.closed)) return;
				serve(track, effect);
			});
		}
	}

	close() {
		this.signals.close();
		this.audio.close();
		this.video.close();
	}
}
