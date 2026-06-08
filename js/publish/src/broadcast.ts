import * as Catalog from "@moq/hang/catalog";
import * as Json from "@moq/json";
import * as Moq from "@moq/net";
import { Effect, type Getter, Signal } from "@moq/signals";
import * as Audio from "./audio";
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

	// The live catalog producer, set while the `catalog.json` track is being served and cleared
	// when it tears down. Applications can `mutate` it to add their own root sections (e.g.
	// `scte35`) alongside the base `video`/`audio`. Because every owner mutates the same shared
	// document, their sections compose instead of clobbering one another.
	#catalog = new Signal<Json.Producer<Catalog.Root> | undefined>(undefined);
	readonly catalog: Getter<Json.Producer<Catalog.Root> | undefined> = this.#catalog;

	signals = new Effect();

	constructor(props?: BroadcastProps) {
		this.connection = Signal.from(props?.connection);
		this.enabled = Signal.from(props?.enabled ?? false);
		this.name = Signal.from(props?.name ?? Moq.Path.empty());

		this.audio = new Audio.Encoder(props?.audio);
		this.video = new Video.Root({ ...props?.video, connection: this.connection });

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
						this.#serveCatalog(new Json.Producer<Catalog.Root>(request.track), effect);
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
					default:
						console.error("received subscription for unknown track", request.track.name);
						request.track.close(new Error(`Unknown track: ${request.track.name}`));
						break;
				}
			});
		}
	}

	#serveCatalog(producer: Json.Producer<Catalog.Root>, effect: Effect): void {
		// Expose the producer so extensions can add their own sections while it's live.
		effect.set(this.#catalog, producer, undefined);

		const enabled = effect.get(this.enabled);
		const video = enabled ? effect.get(this.video.catalog) : undefined;
		const audio = enabled ? effect.get(this.audio.catalog) : undefined;

		// Edit only the base sections so any extension sections survive untouched. A missing
		// section is deleted, which a consumer reads as the section being removed.
		using catalog = producer.lock();
		if (video !== undefined) catalog.value.video = video;
		else delete catalog.value.video;

		if (audio !== undefined) catalog.value.audio = audio;
		else delete catalog.value.audio;
	}

	close() {
		this.signals.close();
		this.audio.close();
		this.video.close();
	}
}
