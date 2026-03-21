import * as Catalog from "@moq/hang/catalog";
import type * as Moq from "@moq/lite";
import { Path } from "@moq/lite";
import * as Msf from "@moq/msf";
import { Effect, type Getter, Signal } from "@moq/signals";

import { toHang } from "./msf";

export type CatalogFormat = "hang" | "msf";

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

	// Which catalog formats to try. Default: ["hang", "msf"]
	catalog?: CatalogFormat[] | Signal<CatalogFormat[]>;
}

// A catalog source that (optionally) reloads automatically when live/offline.
export class Broadcast {
	connection: Signal<Moq.Connection.Established | undefined>;

	enabled: Signal<boolean>;
	name: Signal<Moq.Path.Valid>;
	status = new Signal<"offline" | "loading" | "live">("offline");
	reload: Signal<boolean>;

	catalogFormats: Signal<CatalogFormat[]>;

	#active = new Signal<Moq.Broadcast | undefined>(undefined);
	readonly active: Getter<Moq.Broadcast | undefined> = this.#active;

	#catalog = new Signal<Catalog.Root | undefined>(undefined);
	readonly catalog: Getter<Catalog.Root | undefined> = this.#catalog;

	// All actively announced broadcast paths from the connection.
	#announced: Getter<Set<Moq.Path.Valid>>;

	signals = new Effect();

	constructor(props?: BroadcastProps) {
		this.connection = Signal.from(props?.connection);
		this.name = Signal.from(props?.name ?? Path.empty());
		this.enabled = Signal.from(props?.enabled ?? false);
		this.reload = Signal.from(props?.reload ?? false);
		this.catalogFormats = Signal.from(props?.catalog ?? (["hang", "msf"] as CatalogFormat[]));

		this.#announced = props?.announced ?? new Signal(new Set());

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

		const formats = effect.get(this.catalogFormats);
		this.status.set("loading");

		const hangTrack = formats.includes("hang")
			? broadcast.subscribe("catalog.json", Catalog.PRIORITY.catalog)
			: undefined;
		const msfTrack = formats.includes("msf") ? broadcast.subscribe("catalog", Catalog.PRIORITY.catalog) : undefined;

		if (hangTrack) effect.cleanup(() => hangTrack.close());
		if (msfTrack) effect.cleanup(() => msfTrack.close());

		effect.spawn(async () => {
			try {
				// Race the first catalog fetch, giving hang a 100ms headstart
				const hangFetch = hangTrack
					? Catalog.fetch(hangTrack).then((r) => (r ? { kind: "hang" as const, root: r } : undefined))
					: undefined;

				const msfFetch = msfTrack
					? new Promise((r) => setTimeout(r, 100))
							.then(() => Msf.fetch(msfTrack))
							.then((c) => (c ? { kind: "msf" as const, root: toHang(c) } : undefined))
					: undefined;

				const candidates = [effect.cancel, hangFetch, msfFetch].filter(
					(c): c is NonNullable<typeof c> => c != null,
				);
				const first = await Promise.race(candidates);
				if (!first) return;

				// Close the loser
				if (first.kind === "hang") {
					msfTrack?.close();
				} else {
					hangTrack?.close();
				}

				console.debug("received catalog", first.kind, this.name.peek(), first.root);
				this.#catalog.set(first.root);
				this.status.set("live");

				// Continue reading updates from the winner
				const fetchNext =
					first.kind === "hang"
						? async () => {
								const update = await Promise.race([
									effect.cancel,
									Catalog.fetch(hangTrack as Moq.Track),
								]);
								return update ?? undefined;
							}
						: async () => {
								const update = await Promise.race([effect.cancel, Msf.fetch(msfTrack as Moq.Track)]);
								return update ? toHang(update) : undefined;
							};

				for (;;) {
					const root = await fetchNext();
					if (!root) break;
					console.debug("received catalog", first.kind, this.name.peek(), root);
					this.#catalog.set(root);
				}
			} catch (err) {
				console.warn("error fetching catalog", this.name.peek(), err);
			} finally {
				this.#catalog.set(undefined);
				this.status.set("offline");
			}
		});
	}

	close() {
		this.signals.close();
	}
}
