import * as Catalog from "@moq/hang/catalog";
import * as Json from "@moq/json";
import * as Moq from "@moq/net";
import { Effect, Signal } from "@moq/signals";
import * as Audio from "./audio";
import * as Chat from "./chat";
import * as Location from "./location";
import { Preview, type PreviewProps } from "./preview";
import * as User from "./user";
import * as Video from "./video";

export type BroadcastProps = {
	connection?: Moq.Connection.Established | Signal<Moq.Connection.Established | undefined>;
	enabled?: boolean | Signal<boolean>;
	name?: Moq.Path.Valid | Signal<Moq.Path.Valid>;
	audio?: Audio.EncoderProps;
	video?: Video.Props;
	location?: Location.Props;
	user?: User.Props;
	chat?: Chat.Props;
	preview?: PreviewProps;
};

export class Broadcast {
	static readonly CATALOG_TRACK = "catalog.json";

	// Track names this broadcast knows how to serve; any other request is rejected.
	static readonly #TRACKS: ReadonlySet<string> = new Set([
		Broadcast.CATALOG_TRACK,
		Location.Window.TRACK,
		Location.Peers.TRACK,
		Preview.TRACK,
		Chat.Typing.TRACK,
		Chat.Message.TRACK,
		Audio.Encoder.TRACK,
		Video.Root.TRACK_HD,
		Video.Root.TRACK_SD,
	]);

	connection: Signal<Moq.Connection.Established | undefined>;
	enabled: Signal<boolean>;
	name: Signal<Moq.Path.Valid>;

	audio: Audio.Encoder;
	video: Video.Root;

	location: Location.Root;
	chat: Chat.Root;
	preview: Preview;
	user: User.Info;

	signals = new Effect();

	constructor(props?: BroadcastProps) {
		this.connection = Signal.from(props?.connection);
		this.enabled = Signal.from(props?.enabled ?? false);
		this.name = Signal.from(props?.name ?? Moq.Path.empty());

		this.audio = new Audio.Encoder(props?.audio);
		this.video = new Video.Root({ ...props?.video, connection: this.connection });
		this.location = new Location.Root(props?.location);
		this.chat = new Chat.Root(props?.chat);
		this.preview = new Preview(props?.preview);
		this.user = new User.Info(props?.user);

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

			// Accept the request to commit the track's immutable publisher properties up
			// front (so the wire layer can answer a TRACK request on lite-05+ and pick the
			// frame codec) and obtain the TrackProducer to serve. An unknown track is
			// rejected instead, without accepting, so its TRACK / SUBSCRIBE resets.
			if (!Broadcast.#TRACKS.has(request.name)) {
				console.error("received subscription for unknown track", request.name);
				request.reject(new Error(`Unknown track: ${request.name}`));
				continue;
			}

			const track = request.accept();
			effect.cleanup(() => track.close());

			effect.run((effect) => {
				if (effect.get(track.state.closed)) return;

				switch (request.name) {
					case Broadcast.CATALOG_TRACK:
						this.#serveCatalog(new Json.Producer<Catalog.Root>(track), effect);
						break;
					case Location.Window.TRACK:
						this.location.window.serve(track, effect);
						break;
					case Location.Peers.TRACK:
						this.location.peers.serve(track, effect);
						break;
					case Preview.TRACK:
						this.preview.serve(track, effect);
						break;
					case Chat.Typing.TRACK:
						this.chat.typing.serve(track, effect);
						break;
					case Chat.Message.TRACK:
						this.chat.message.serve(track, effect);
						break;
					case Audio.Encoder.TRACK:
						this.audio.serve(track, effect);
						break;
					case Video.Root.TRACK_HD:
						this.video.hd.serve(track, effect);
						break;
					case Video.Root.TRACK_SD:
						this.video.sd.serve(track, effect);
						break;
				}
			});
		}
	}

	#serveCatalog(producer: Json.Producer<Catalog.Root>, effect: Effect): void {
		if (!effect.get(this.enabled)) {
			// Clear the catalog.
			producer.update({});
			return;
		}

		// Create the new catalog.
		const catalog: Catalog.Root = {
			video: effect.get(this.video.catalog),
			audio: effect.get(this.audio.catalog),
			location: effect.get(this.location.catalog),
			user: effect.get(this.user.catalog),
			chat: effect.get(this.chat.catalog),
			preview: effect.get(this.preview.catalog),
		};

		producer.update(catalog);
	}

	close() {
		this.signals.close();
		this.audio.close();
		this.video.close();
		this.location.close();
		this.chat.close();
		this.preview.close();
		this.user.close();
	}
}
