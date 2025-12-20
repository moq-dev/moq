import type * as Moq from "@moq/lite";
import * as Zod from "@moq/lite/zod";
import { Effect, Signal } from "@moq/signals";
import * as Catalog from "../../catalog";
import { PRIORITY } from "../priority";

export type WindowProps = {
	// If true, then we'll publish our position to the broadcast.
	enabled?: boolean | Signal<boolean>;

	// Our current position.
	position?: Catalog.Position | Signal<Catalog.Position | undefined>;

	// If set, then this broadcaster allows other peers to request position updates via this handle.
	handle?: string | Signal<string | undefined>;
};

export class Window {
	static readonly TRACK = "location/window.json";
	static readonly PRIORITY = PRIORITY.location;

	enabled: Signal<boolean>;
	position: Signal<Catalog.Position | undefined>;
	handle: Signal<string | undefined>; // Allow other peers to request position updates via this handle.

	catalog = new Signal<Catalog.Location | undefined>(undefined);

	signals = new Effect();

	constructor(props?: WindowProps) {
		this.enabled = Signal.from(props?.enabled ?? false);
		this.position = Signal.from(props?.position ?? undefined);
		this.handle = Signal.from(props?.handle ?? undefined);

		this.signals.effect((effect) => {
			const enabled = effect.get(this.enabled);
			if (!enabled) return;

			effect.set(this.catalog, {
				initial: this.position.peek(),
				track: { name: Window.TRACK, priority: Window.PRIORITY },
				handle: effect.get(this.handle),
			});
		});
	}

	serve(track: Moq.Track, effect: Effect): void {
		const enabled = effect.get(this.enabled);
		if (!enabled) return;

		const position = effect.get(this.position);
		if (!position) return;

		Zod.write(track, position, Catalog.PositionSchema);
	}

	close() {
		this.signals.close();
	}
}
