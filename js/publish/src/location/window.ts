import { PRIORITY, type Track } from "@moq/hang/catalog";
import type * as Moq from "@moq/lite";
import { Effect, Signal } from "@moq/signals";
import type { Position } from "./types";

export type WindowProps = {
	// If true, then we'll publish our position to the broadcast.
	enabled?: boolean | Signal<boolean>;

	// Our current position.
	position?: Position | Signal<Position | undefined>;

	// If set, then this broadcaster allows other peers to request position updates via this handle.
	handle?: string | Signal<string | undefined>;
};

export type LocationCatalog = {
	initial?: Position;
	track?: Track;
	handle?: string;
	peers?: Track;
};

export class Window {
	static readonly TRACK = "location/window.json";
	static readonly PRIORITY = PRIORITY.location;

	enabled: Signal<boolean>;
	position: Signal<Position | undefined>;
	handle: Signal<string | undefined>; // Allow other peers to request position updates via this handle.

	catalog = new Signal<LocationCatalog | undefined>(undefined);

	signals = new Effect();

	constructor(props?: WindowProps) {
		this.enabled = Signal.from(props?.enabled ?? false);
		this.position = Signal.from(props?.position ?? undefined);
		this.handle = Signal.from(props?.handle ?? undefined);

		this.signals.run((effect) => {
			const enabled = effect.get(this.enabled);
			if (!enabled) return;

			effect.set(this.catalog, {
				initial: this.position.peek(),
				track: { name: Window.TRACK },
				handle: effect.get(this.handle),
			});
		});
	}

	serve(track: Moq.Track, effect: Effect): void {
		const values = effect.getAll([this.enabled, this.position]);
		if (!values) return;
		const [_, position] = values;

		track.writeJson(position);
	}

	close() {
		this.signals.close();
	}
}
