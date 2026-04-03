import { AUDIO, type Audio, CatalogReader, PRIORITY, VIDEO, type Video } from "@moq/hang/catalog";
import type * as Moq from "@moq/lite";
import { Path } from "@moq/lite";
import { Effect, type Getter, Signal } from "@moq/signals";
import { CHAT, type Chat, LOCATION, type Location, PREVIEW, type PreviewTrack, USER, type User } from "./sections";

export interface BroadcastProps {
	connection?: Moq.Connection.Established | Signal<Moq.Connection.Established | undefined>;

	// All actively announced broadcast paths from the connection.
	announced?: Getter<Set<Moq.Path.Valid>>;

	// Whether to start downloading the broadcast.
	// Defaults to false so you can make sure everything is ready before starting.
	enabled?: boolean | Signal<boolean>;

	// The broadcast name.
	name?: Moq.Path.Valid | Signal<Moq.Path.Valid>;

	// Whether to reload the broadcast when it goes offline.
	// Defaults to false; pass true to wait for an announcement before subscribing.
	reload?: boolean | Signal<boolean>;
}

// A catalog source that (optionally) reloads automatically when live/offline.
export class Broadcast {
	connection: Signal<Moq.Connection.Established | undefined>;

	enabled: Signal<boolean>;
	name: Signal<Moq.Path.Valid>;
	status = new Signal<"offline" | "loading" | "live">("offline");
	reload: Signal<boolean>;

	#active = new Signal<Moq.Broadcast | undefined>(undefined);
	readonly active: Getter<Moq.Broadcast | undefined> = this.#active;

	// Per-section catalog getters
	#catalogReader = new CatalogReader();

	readonly video: Getter<Video | undefined>;
	readonly audio: Getter<Audio | undefined>;
	readonly chat: Getter<Chat | undefined>;
	readonly user: Getter<User | undefined>;
	readonly preview: Getter<PreviewTrack | undefined>;
	readonly location: Getter<Location | undefined>;

	// All actively announced broadcast paths from the connection.
	#announced: Getter<Set<Moq.Path.Valid>>;

	signals = new Effect();

	constructor(props?: BroadcastProps) {
		this.connection = Signal.from(props?.connection);
		this.name = Signal.from(props?.name ?? Path.empty());
		this.enabled = Signal.from(props?.enabled ?? false);
		this.reload = Signal.from(props?.reload ?? false);

		this.#announced = props?.announced ?? new Signal(new Set());

		// Register all known sections
		this.video = this.#catalogReader.section(VIDEO);
		this.audio = this.#catalogReader.section(AUDIO);
		this.chat = this.#catalogReader.section(CHAT);
		this.user = this.#catalogReader.section(USER);
		this.preview = this.#catalogReader.section(PREVIEW);
		this.location = this.#catalogReader.section(LOCATION);

		this.signals.run(this.#runBroadcast.bind(this));
		this.signals.run(this.#runCatalog.bind(this));
	}

	#isAnnounced(effect: Effect): boolean {
		const reload = effect.get(this.reload);
		if (!reload) return true;

		const name = effect.get(this.name);
		const announced = effect.get(this.#announced);
		return announced.has(name);
	}

	#runBroadcast(effect: Effect): void {
		const enabled = effect.get(this.enabled);
		if (!enabled) return;

		if (!this.#isAnnounced(effect)) return;

		const conn = effect.get(this.connection);
		if (!conn) return;

		const name = effect.get(this.name);

		const broadcast = conn.consume(name);
		effect.cleanup(() => broadcast.close());

		effect.set(this.#active, broadcast, undefined);
	}

	#runCatalog(effect: Effect): void {
		const values = effect.getAll([this.enabled, this.active]);
		if (!values) return;
		const [_, broadcast] = values;

		this.status.set("loading");

		const catalog = broadcast.subscribe("catalog.json", PRIORITY.catalog);
		effect.cleanup(() => catalog.close());

		// Use CatalogReader to consume the catalog track and update per-section signals.
		this.#catalogReader.consume(catalog, effect);

		// Track status based on whether any section has a value.
		effect.run((inner) => {
			const video = inner.get(this.video);
			const audio = inner.get(this.audio);
			const chat = inner.get(this.chat);
			const user = inner.get(this.user);
			const preview = inner.get(this.preview);
			const location = inner.get(this.location);

			if (video || audio || chat || user || preview || location) {
				this.status.set("live");
			}
		});

		effect.cleanup(() => {
			this.status.set("offline");
		});
	}

	close() {
		this.signals.close();
	}
}
