import * as Catalog from "@moq/hang/catalog";
import type * as Moq from "@moq/lite";
import { Effect, type Getter, Signal } from "@moq/signals";

export interface BroadcastProps {
	connection?: Moq.Connection.Established | Signal<Moq.Connection.Established | undefined>;

	// Whether to start downloading the broadcast.
	// Defaults to false so you can make sure everything is ready before starting.
	enabled?: boolean | Signal<boolean>;

	// The broadcast name.
	path?: Moq.Path.Valid | Signal<Moq.Path.Valid | undefined>;

	// Whether to reload the broadcast when it goes offline.
	// Defaults to false; pass true to wait for an announcement before subscribing.
	reload?: boolean | Signal<boolean>;
}

// A catalog source that (optionally) reloads automatically when live/offline.
export class Broadcast {
	connection: Signal<Moq.Connection.Established | undefined>;

	enabled: Signal<boolean>;
	path: Signal<Moq.Path.Valid | undefined>;
	status = new Signal<"offline" | "loading" | "live">("offline");
	reload: Signal<boolean>;

	#active = new Signal<Moq.Broadcast | undefined>(undefined);
	readonly active: Getter<Moq.Broadcast | undefined> = this.#active;

	#catalog = new Signal<Catalog.Root | undefined>(undefined);
	readonly catalog: Getter<Catalog.Root | undefined> = this.#catalog;

	// This signal is true when the broadcast has been announced, unless reloading is disabled.
	#announced = new Signal(false);

	signals = new Effect();

	constructor(props?: BroadcastProps) {
		this.connection = Signal.from(props?.connection);
		this.path = Signal.from(props?.path);
		this.enabled = Signal.from(props?.enabled ?? false);
		this.reload = Signal.from(props?.reload ?? false);

		this.signals.run(this.#runReload.bind(this));
		this.signals.run(this.#runBroadcast.bind(this));
		this.signals.run(this.#runCatalog.bind(this));
	}

	#runReload(effect: Effect): void {
		const enabled = effect.get(this.enabled);
		if (!enabled) return;

		const reload = effect.get(this.reload);
		if (!reload) {
			// Mark as active without waiting for an announcement.
			effect.set(this.#announced, true, false);
			return;
		}

		const conn = effect.get(this.connection);
		if (!conn) return;

		const path = effect.get(this.path);
		if (path === undefined) return;

		const announced = conn.announced(path);
		effect.cleanup(() => announced.close());

		effect.spawn(async () => {
			for (;;) {
				const update = await announced.next();
				if (!update) break;

				// Require full equality
				if (update.path !== path) {
					console.warn("ignoring announce", update.path);
					continue;
				}

				effect.set(this.#announced, update.active, false);
			}
		});
	}

	#runBroadcast(effect: Effect): void {
		const values = effect.getAll([this.enabled, this.#announced, this.connection]);
		if (!values) return;
		const [_enabled, _announced, conn] = values;

		const path = effect.get(this.path);
		if (path === undefined) return;

		const broadcast = conn.consume(path);
		effect.cleanup(() => broadcast.close());

		effect.set(this.#active, broadcast);
	}

	#runCatalog(effect: Effect): void {
		const values = effect.getAll([this.enabled, this.active]);
		if (!values) return;
		const [_, broadcast] = values;

		this.status.set("loading");

		const catalog = broadcast.subscribe("catalog.json", Catalog.PRIORITY.catalog);
		effect.cleanup(() => catalog.close());

		effect.spawn(this.#fetchCatalog.bind(this, catalog));
	}

	async #fetchCatalog(catalog: Moq.Track): Promise<void> {
		try {
			for (;;) {
				const update = await Catalog.fetch(catalog);
				if (!update) break;

				console.debug("received catalog", this.path.peek(), update);

				this.#catalog.set(update);
				this.status.set("live");
			}
		} catch (err) {
			console.warn("error fetching catalog", this.path.peek(), err);
		} finally {
			this.#catalog.set(undefined);
			this.status.set("offline");
		}
	}

	close() {
		this.signals.close();
	}
}
